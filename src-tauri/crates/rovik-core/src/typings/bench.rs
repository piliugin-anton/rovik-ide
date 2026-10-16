//! A synthetic large project, for a look at what the editor's load costs:
//! what the old whole-tree walk read against what the closure reads.
//! `cargo test -p rovik-core --release --lib typings::bench -- --ignored --nocapture`

use super::test_support::temp_project;
use super::*;
use std::fs;
use std::time::Instant;
use walkdir::WalkDir;

/// 1,500 packages, 25 declaration files each (37,500 files, ~110 MB);
/// 60 of them imported by 400 source files; each package's declarations
/// import two others of its tier, so the closure has depth.
fn generate() -> std::path::PathBuf {
    let dir = temp_project(
        "typings-bench",
        &[
            (".gitignore", "node_modules\n"),
            ("tsconfig.json", r#"{"compilerOptions":{"types":["node"]}}"#),
        ],
    );
    let nm = dir.join("node_modules");
    let filler = "x".repeat(2_800);
    for i in 0..1_500 {
        let name = format!("pkg{i}");
        let pkg = nm.join(&name);
        fs::create_dir_all(&pkg).unwrap();
        fs::write(
            pkg.join("package.json"),
            format!(r#"{{"name":"{name}","types":"index.d.ts"}}"#),
        )
        .unwrap();
        // The imported packages (0..60) reach a pool of 200 runtime packages;
        // the rest (tools, plugins) reach each other.
        let (a, b) = if i < 60 {
            (60 + (i * 7) % 200, 60 + (i * 13) % 200)
        } else if i < 260 {
            (60 + (i * 3) % 200, 60 + (i * 5) % 200)
        } else {
            (260 + (i * 3) % 1_240, 260 + (i * 11) % 1_240)
        };
        for f in 0..25 {
            let imports = if f == 0 {
                format!(
                    "import 'pkg{a}'; import type {{ X }} from 'pkg{b}';\nexport * from './f1';\n"
                )
            } else {
                String::new()
            };
            fs::write(
                pkg.join(format!(
                    "{}.d.ts",
                    if f == 0 {
                        "index".into()
                    } else {
                        format!("f{f}")
                    }
                )),
                format!("{imports}export declare const v{f}: number; /* {filler} */\n"),
            )
            .unwrap();
        }
    }
    fs::create_dir_all(nm.join("@types/node")).unwrap();
    fs::write(
        nm.join("@types/node/index.d.ts"),
        "declare var process: any;\n".repeat(500),
    )
    .unwrap();
    let mut deps = String::new();
    for i in 0..400 {
        let src = dir.join(format!("src/mod{}/file{i}.ts", i % 20));
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        let p = (i * 3) % 60;
        fs::write(
            &src,
            format!("import {{ v0 }} from 'pkg{p}';\nimport x from 'pkg{}/f2';\nimport './other';\nexport const n = v0;\n", (i * 7) % 60),
        )
        .unwrap();
        if i < 60 {
            deps.push_str(&format!("\"pkg{i}\":\"1\","));
        }
    }
    deps.pop();
    fs::write(
        dir.join("package.json"),
        format!("{{\"dependencies\":{{{deps}}}}}"),
    )
    .unwrap();
    dir
}

/// What node_modules.rs used to do: every declaration under node_modules.
fn old_walk(root: &Path) -> (usize, usize) {
    let (mut files, mut bytes) = (0, 0);
    for entry in WalkDir::new(root.join("node_modules"))
        .into_iter()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_file() && (name.ends_with(".d.ts") || name == "package.json") {
            if let Ok(text) = fs::read_to_string(entry.path()) {
                files += 1;
                bytes += text.len();
            }
        }
    }
    (files, bytes)
}

#[test]
#[ignore]
fn synthetic_large_project() {
    let dir = generate();
    // Both read warm: the first walk warms the page cache for the second,
    // so the old walk goes first, cold, then again warm.
    let started = Instant::now();
    let (files, bytes) = old_walk(&dir);
    let cold = started.elapsed();
    let started = Instant::now();
    old_walk(&dir);
    let warm = started.elapsed();
    println!(
        "old: every declaration: {files} files, {:.1} MB, cold {cold:?}, warm {warm:?}",
        bytes as f64 / 1e6
    );

    let started = Instant::now();
    let loaded = load_definitions(&dir);
    let took = started.elapsed();
    let typing_bytes: usize = loaded.typings.files.iter().map(|f| f.content.len()).sum();
    let started = Instant::now();
    let json = serde_json::to_string(&loaded).unwrap();
    let serialized = started.elapsed();
    println!(
        "new: closure: {} packages, {} files, {:.1} MB (sources {} files), load {took:?}, to JSON ({:.1} MB) {serialized:?}, truncated {}",
        loaded.typings.packages.len(),
        loaded.typings.files.len(),
        typing_bytes as f64 / 1e6,
        loaded.sources.files.len(),
        json.len() as f64 / 1e6,
        loaded.typings.truncated
    );
    let started = Instant::now();
    let later = load_dependencies(
        &[ImportSite {
            from: dir.join("src/mod0/file0.ts").to_string_lossy().into_owned(),
            specifiers: vec!["pkg700".into()],
        }],
        &loaded
            .typings
            .packages
            .iter()
            .map(|p| p.dir.clone())
            .collect::<Vec<_>>(),
    );
    println!(
        "later import of pkg700: {} files in {:?}",
        later.files.len(),
        started.elapsed()
    );
    let _ = fs::remove_dir_all(&dir);
}
