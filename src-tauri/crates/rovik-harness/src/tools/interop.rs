use super::{args, checked, run, write_json};
use crate::{normalizer::Inventory, require, Result};
use serde::Serialize;
use std::{path::Path, time::Instant};

#[derive(Serialize)]
struct Observation {
    id: String,
    passed: bool,
    exit_code: Option<i32>,
    duration_ms: u64,
}
fn instrumented(path: &str) -> Result<bool> {
    let symbols = checked(&args(&["/usr/bin/nm", "-u", path]), Path::new("/build"))?;
    Ok(symbols.contains("__asan_") && symbols.contains("__ubsan_"))
}
pub(super) fn check() -> Result<i32> {
    let source = std::fs::read_to_string("/oracle/packet.c")?;
    let variants = [
        ("correct", source.clone()),
        ("partial-leak", source.replace("if (!value->second) { free(value->first);", "if (!value->second) {")),
        ("failure-output", source.replace("packet *value = malloc", "*out = NULL; packet *value = malloc")),
        ("abi-value", source.replace("out->size = value->size;", "out->size = value->size + 1;")),
        ("undefined-overflow", source.replace("packet *value = malloc", "volatile int32_t limit = INT32_MAX; limit += (int32_t)size; packet *value = malloc")),
        ("buffer-overrun", source.replace("i < size;", "i <= size;")),
        ("retained-callback", source.replace("struct packet {", "static packet_callback retained; static void *retained_context;\nstruct packet {")
            .replace("if (callback) callback(context, checksum);", "retained = callback; retained_context = context; if (callback) callback(context, checksum);")
            .replace("free(value->second);", "if (retained) { retained(retained_context, 1); } free(value->second);")),
    ];
    let flags = args(&[
        "-g",
        "-O1",
        "-fno-omit-frame-pointer",
        "-Wall",
        "-Wextra",
        "-Werror",
    ]);
    let sanitizer = args(&["-fsanitize=address,undefined", "-fno-sanitize-recover=all"]);
    let cwd = Path::new("/build");
    let mut results = vec![];
    for (name, contents) in variants {
        require(
            name == "correct" || contents != source,
            "interop mutant did not change its source",
        )?;
        let directory = format!("/build/{name}");
        std::fs::create_dir(&directory)?;
        let file = format!("{directory}/packet.c");
        std::fs::write(&file, contents)?;
        for language in ["c", "cpp", "rust"] {
            if language == "rust" && matches!(name, "buffer-overrun" | "undefined-overflow") {
                continue;
            }
            let start = Instant::now();
            let extra = if language == "rust" {
                vec![]
            } else {
                sanitizer.clone()
            };
            let producer = format!("{directory}/{language}-producer.o");
            let allocator = format!("{directory}/{language}-allocator.o");
            let mut command = args(&["/usr/bin/gcc", "-std=c17"]);
            command.extend(flags.clone());
            command.extend(extra.clone());
            command.extend(args(&[
                "-I/oracle",
                "-Dmalloc=harness_malloc",
                "-Dfree=harness_free",
                "-c",
                &file,
                "-o",
                &producer,
            ]));
            checked(&command, cwd)?;
            let mut command = args(&["/usr/bin/gcc", "-std=c17"]);
            command.extend(flags.clone());
            command.extend(extra.clone());
            command.extend(args(&["-c", "/oracle/allocator.c", "-o", &allocator]));
            checked(&command, cwd)?;
            if language != "rust" {
                require(
                    instrumented(&producer)?,
                    "required sanitizer instrumentation absent",
                )?;
            }
            let executable = format!("{directory}/{language}");
            if language == "rust" {
                let archive = format!("{directory}/libpacket.a");
                checked(
                    &args(&["/usr/bin/ar", "rcs", &archive, &producer, &allocator]),
                    cwd,
                )?;
                checked(
                    &args(&[
                        "/usr/local/cargo/bin/rustc",
                        "--edition=2021",
                        "-Dwarnings",
                        "/oracle/consumer.rs",
                        "-L",
                        &directory,
                        "-lstatic=packet",
                        "-o",
                        &executable,
                    ]),
                    cwd,
                )?;
            } else {
                let mut command = args(&[
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
                ]);
                command.extend(flags.clone());
                command.extend(extra);
                command.extend(args(&[
                    "-I/oracle",
                    "/oracle/consumer.c",
                    &producer,
                    &allocator,
                    "-o",
                    &executable,
                ]));
                checked(&command, cwd)?;
            }
            let observed = run(
                &[executable],
                cwd,
                &[
                    ("ASAN_OPTIONS", "detect_leaks=1:abort_on_error=1"),
                    ("UBSAN_OPTIONS", "halt_on_error=1"),
                ],
            )?;
            results.push(Observation {
                id: format!("{language}-{name}"),
                passed: (observed.code == Some(0)) == (name == "correct"),
                exit_code: observed.code,
                duration_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            });
        }
    }
    checked(
        &args(&[
            "/usr/bin/gcc",
            "-std=c17",
            "-I/oracle",
            "-c",
            "/oracle/packet.c",
            "-o",
            "/build/uninstrumented.o",
        ]),
        cwd,
    )?;
    results.push(Observation {
        id: "removed-sanitizers-rejected".into(),
        passed: !instrumented("/build/uninstrumented.o")?,
        exit_code: Some(0),
        duration_ms: 0,
    });
    write_json("/outputs/observations.json", &results)?;
    let cases = results.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    let report = Inventory {
        schema_version: 1,
        discovered: cases.clone(),
        selected: cases,
        passed: results
            .iter()
            .filter(|r| r.passed)
            .map(|r| r.id.clone())
            .collect(),
        failed: results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| r.id.clone())
            .collect(),
        skipped: vec![],
    };
    report.validate()?;
    write_json("/outputs/cases.json", &report)?;
    Ok(i32::from(!report.failed.is_empty()))
}
