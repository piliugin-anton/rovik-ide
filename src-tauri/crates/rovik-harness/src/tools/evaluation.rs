use super::{args, checked, read, run, write_json};
use crate::{
    evaluation::{Bank, Evaluation, Observation, Outcome, Strategy},
    identity::Digest,
    profile::Language,
    require, wire, Error, Result,
};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path, time::Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Program {
    source: String,
    cases: Vec<String>,
}
fn program(language: &str, task: &str, mutant: bool) -> Result<(String, usize)> {
    let bank: BTreeMap<String, BTreeMap<String, Program>> =
        wire::decode(include_bytes!("../../fixtures/evaluation/programs.json"))?;
    let template = bank
        .get(language)
        .and_then(|b| b.get(task))
        .ok_or_else(|| Error("unknown evaluation program".into()))?;
    let mut body = template.source.clone();
    if mutant {
        body = match (task, language) {
            ("checked-add", _) => body.replace(" < b", " <= b"),
            ("decimal", "c") => body.replace("if (*s<'0' || *s>'9') return 1;", "if (*s<'0' || *s>'9') { *out=value; return 0; }"),
            ("decimal", "cpp") => body.replace(" || result.ptr!=s.data()+s.size()", ""),
            ("decimal", "rust") => body.replace("if input.is_empty()", "let input=input.trim_end_matches(|c:char|!c.is_ascii_digit()); if input.is_empty()"),
            ("copy", "c") => body.replace("if (length>capacity) return 1;", "if (length>capacity) { if(capacity) out[0]=0; return 1; }"),
            ("copy", "cpp") => body.replace("if (source.size()>out.size()) return false;", "if (source.size()>out.size()) { if(!out.empty()) out[0]=0; return false; }"),
            ("copy", "rust") => body.replace("{return false;}", "{if !out.is_empty() {out[0]=0;} return false;}"),
            _ => return Err(Error("unknown evaluation mutation".into())),
        };
        require(
            body != template.source,
            "evaluation mutant did not change source",
        )?;
    }
    let rust = language == "rust";
    let mut lines = if rust {
        vec![
            body,
            "fn main() { let full=std::env::args().nth(1).as_deref()==Some(\"full\");".into(),
        ]
    } else {
        let mut lines = vec![
            "#include <assert.h>\n#include <stdint.h>\n#include <stdio.h>\n#include <string.h>"
                .into(),
        ];
        if language == "cpp" {
            lines.push("#include <algorithm>\n#include <charconv>\n#include <optional>\n#include <span>\n#include <string_view>".into());
        }
        lines.extend([
            body,
            "int main(int argc,char **argv) { int full=argc>1 && !strcmp(argv[1],\"full\");".into(),
        ]);
        lines
    };
    for (number, case) in template.cases.iter().enumerate() {
        if number == 1 {
            lines.push(
                if rust {
                    "if !full {return;}"
                } else {
                    "if (!full) return 0;"
                }
                .into(),
            );
        }
        lines.push(if rust {
            format!("println!(\"case:{}\");", number + 1)
        } else {
            format!("printf(\"case:{}\\n\"); fflush(stdout);", number + 1)
        });
        lines.push(case.clone());
    }
    lines.push(if rust { "}" } else { "return 0; }" }.into());
    Ok((lines.join("\n"), template.cases.len()))
}
pub(super) fn check() -> Result<i32> {
    let cwd = Path::new("/build");
    let bank: Bank = read("/oracle/bank.json")?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Environment {
        digest: Digest,
    }
    let environment: Environment = read("/oracle/environment.json")?;
    require(
        checked(&args(&["/usr/bin/gcc", "-dumpversion"]), cwd)?.trim() == "12",
        "evaluation GCC version mismatch",
    )?;
    require(
        checked(&args(&["/usr/local/cargo/bin/rustc", "--version"]), cwd)?
            .split_whitespace()
            .nth(1)
            == Some("1.85.1"),
        "evaluation Rust version mismatch",
    )?;
    let mut observations = Vec::new();
    for case in &bank.cases {
        crate::scope::validate_path(&case.case_id)?;
        require(
            !case.case_id.contains('/'),
            "evaluation ID must be one path segment",
        )?;
        let language = match case.language {
            Language::Rust => "rust",
            Language::C => "c",
            Language::Cpp => "cpp",
        };
        let (name, variant) = case
            .case_id
            .rsplit_once('-')
            .ok_or_else(|| Error("invalid evaluation identity".into()))?;
        let task = name
            .strip_prefix(&format!("{language}-"))
            .ok_or_else(|| Error("evaluation language mismatch".into()))?;
        require(
            matches!(variant, "correct" | "mutant"),
            "unknown evaluation variant",
        )?;
        let (source, count) = program(language, task, variant == "mutant")?;
        require(
            count as u64 == case.required_cases,
            "evaluation inventory mismatch",
        )?;
        for repetition in 0..bank.repetitions {
            let directory = format!("/build/{}-{repetition}", case.case_id);
            std::fs::create_dir(&directory)?;
            let file = format!(
                "{directory}/main.{}",
                if language == "rust" { "rs" } else { language }
            );
            std::fs::write(&file, &source)?;
            let binary = format!("{directory}/consumer");
            let command = if language == "rust" {
                args(&[
                    "/usr/local/cargo/bin/rustc",
                    "--edition=2021",
                    "--target=x86_64-unknown-linux-gnu",
                    "-Dwarnings",
                    "-Funsafe-code",
                    &file,
                    "-o",
                    &binary,
                ])
            } else {
                args(&[
                    if language == "c" {
                        "/usr/bin/gcc"
                    } else {
                        "/usr/bin/g++"
                    },
                    if language == "c" {
                        "-std=c17"
                    } else {
                        "-std=c++23"
                    },
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    &file,
                    "-o",
                    &binary,
                ])
            };
            let start = Instant::now();
            let compiled = run(&command, cwd, &[])?;
            let compile_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            for strategy in [
                Strategy::CompileOnly,
                Strategy::ExampleOnly,
                Strategy::ProtectedContract,
            ] {
                let mut outcome = Outcome::Error;
                let mut executed = 0;
                let mut duration_ms = compile_ms;
                if compiled.code == Some(0) {
                    if strategy == Strategy::CompileOnly {
                        outcome = Outcome::Accepted;
                    } else {
                        let start = Instant::now();
                        let result = run(
                            &args(&[
                                &binary,
                                if strategy == Strategy::ProtectedContract {
                                    "full"
                                } else {
                                    "example"
                                },
                            ]),
                            cwd,
                            &[],
                        )?;
                        duration_ms = duration_ms.saturating_add(
                            start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                        );
                        let expected = if strategy == Strategy::ProtectedContract {
                            count
                        } else {
                            1
                        };
                        let output =
                            String::from_utf8(result.stdout).map_err(|e| Error(e.to_string()))?;
                        let lines = output.lines().collect::<Vec<_>>();
                        let valid = !lines.is_empty()
                            && lines.len() <= expected
                            && lines
                                .iter()
                                .enumerate()
                                .all(|(i, s)| *s == format!("case:{}", i + 1));
                        if valid {
                            executed = lines.len() as u64;
                            outcome = if result.code == Some(0) && lines.len() == expected {
                                Outcome::Accepted
                            } else if result.code != Some(0) {
                                Outcome::Rejected
                            } else {
                                Outcome::Error
                            };
                        }
                    }
                }
                observations.push(Observation {
                    case_id: case.case_id.clone(),
                    repetition,
                    strategy,
                    outcome,
                    executed_cases: executed,
                    duration_ms,
                });
            }
        }
    }
    let evaluation = Evaluation {
        schema_version: 1,
        bank_digest: Digest::of_json(&bank)?,
        environment_digest: environment.digest.clone(),
        methodology: "native-verifier-ablation; no model calls".into(),
        observations,
    };
    evaluation.summarize(&bank, &environment.digest)?;
    write_json("/outputs/evaluation.json", &evaluation)?;
    Ok(0)
}
