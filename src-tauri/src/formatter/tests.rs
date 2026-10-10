use super::*;
use std::fs;

/// A scratch project; removed when dropped.
struct Project(PathBuf);

impl Project {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("formatter-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Project(dir)
    }

    fn file(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        p
    }

    /// An executable shell script. The formatter runs with the test's PATH,
    /// which has only the fake tools, so the script brings its own.
    #[cfg(unix)]
    fn tool(&self, rel: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = self.file(rel, &format!("#!/bin/sh\nPATH=/usr/bin:/bin\n{script}\n"));
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn path_env(&self) -> std::ffi::OsString {
        self.0.join("global-bin").into_os_string()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn found(p: &Project, file: &Path, language: &str) -> Option<(String, bool, PathBuf)> {
    let r = resolve(file, language, Some(&p.0), &p.path_env());
    r.found
        .map(|f| (f.spec.id.to_string(), f.declared, f.program))
}

#[cfg(unix)]
#[test]
fn the_projects_own_copy_of_its_formatter_comes_first() {
    let p = Project::new("own");
    p.file("package.json", r#"{"devDependencies":{"prettier":"3"}}"#);
    let local = p.tool("node_modules/.bin/prettier", "cat");
    p.tool("global-bin/prettier", "cat");
    let file = p.file("src/a.ts", "x");
    assert_eq!(
        found(&p, &file, "typescript"),
        Some(("prettier".into(), true, local))
    );
}

#[cfg(unix)]
#[test]
fn a_declared_formatter_beats_an_earlier_undeclared_one() {
    let p = Project::new("declared");
    // Biome is preferred over Prettier, but this project uses Prettier.
    p.file(".prettierrc", "{}");
    p.tool("global-bin/biome", "cat");
    let prettier = p.tool("global-bin/prettier", "cat");
    let file = p.file("a.css", "x");
    assert_eq!(
        found(&p, &file, "css"),
        Some(("prettier".into(), true, prettier))
    );
    // ...and undeclared Biome is never a fallback.
    fs::remove_file(p.0.join(".prettierrc")).unwrap();
    fs::remove_file(p.0.join("global-bin/prettier")).unwrap();
    assert_eq!(found(&p, &file, "css"), None);
}

#[cfg(unix)]
#[test]
fn a_declared_formatter_that_isnt_installed_blocks_the_fallbacks() {
    let p = Project::new("missing");
    p.file("pyproject.toml", "[tool.black]\nline-length = 100\n");
    p.tool("global-bin/ruff", "cat");
    let file = p.file("pkg/a.py", "x");
    let r = resolve(&file, "python", Some(&p.0), &p.path_env());
    assert!(r.found.is_none());
    assert_eq!(r.missing.map(|s| s.id), Some("black"));
    let err = format_text(&file, "python", Some(&p.0), "x = 1\n".into(), &p.path_env());
    assert_eq!(
        err,
        Err("the project uses Black, but `black` isn't installed".into())
    );
}

#[cfg(unix)]
#[test]
fn standard_formatters_run_without_a_config() {
    let p = Project::new("fallback");
    let gofmt = p.tool("global-bin/gofmt", "cat");
    let venv_ruff = p.tool(".venv/bin/ruff", "cat");
    assert_eq!(
        found(&p, &p.file("main.go", "x"), "go"),
        Some(("gofmt".into(), false, gofmt))
    );
    assert_eq!(
        found(&p, &p.file("a.py", "x"), "python"),
        Some(("ruff".into(), false, venv_ruff))
    );
    assert_eq!(found(&p, &p.file("a.rb", "x"), "ruby"), None);
}

#[cfg(unix)]
#[test]
fn only_toml_files_of_the_ini_language_are_formatted_as_toml() {
    let p = Project::new("toml");
    p.tool("global-bin/taplo", "cat");
    assert!(found(&p, &p.file("Cargo.toml", "x"), "ini").is_some());
    assert!(found(&p, &p.file("setup.cfg", "x"), "ini").is_none());
}

#[cfg(unix)]
#[test]
fn formatting_pipes_the_text_through_the_tool() {
    let p = Project::new("run");
    // Gets the file's path, runs in the folder that declared it.
    p.file(".clang-format", "BasedOnStyle: LLVM\n");
    p.tool(
        "global-bin/clang-format",
        r#"[ "$1" = "--assume-filename=$PWD/src/a.c" ] || { echo "bad args: $1 in $PWD" >&2; exit 2; }
tr a-z A-Z"#,
    );
    let file = p.file("src/a.c", "");
    // Canonical, as $PWD is.
    let file = fs::canonicalize(&file).unwrap();
    let root = fs::canonicalize(&p.0).unwrap();
    let out = format_text(&file, "c", Some(&root), "int x;\n".into(), &p.path_env());
    assert_eq!(out, Ok("INT X;\n".into()));
    // Nothing to format: the tool isn't run.
    let out = format_text(&file, "c", Some(&root), "\n".into(), &p.path_env());
    assert_eq!(out, Ok("\n".into()));
}

#[cfg(unix)]
#[test]
fn a_failing_formatter_reports_its_error() {
    let p = Project::new("fail");
    p.tool(
        "global-bin/gofmt",
        "cat >/dev/null; echo '1:1: expected package' >&2; exit 2",
    );
    let file = p.file("main.go", "");
    let out = format_text(&file, "go", Some(&p.0), "nonsense".into(), &p.path_env());
    assert_eq!(out, Err("gofmt: 1:1: expected package".into()));

    // ktlint exits 1 for issues it couldn't fix: still formatted.
    p.tool("global-bin/ktlint", "cat; exit 1");
    let file = p.file("a.kt", "");
    let out = format_text(
        &file,
        "kotlin",
        Some(&p.0),
        "val x = 1\n".into(),
        &p.path_env(),
    );
    assert_eq!(out, Ok("val x = 1\n".into()));
}

#[test]
fn the_rust_edition_comes_from_the_nearest_cargo_toml() {
    let p = Project::new("edition");
    p.file("Cargo.toml", "[workspace]\n");
    p.file(
        "crates/a/Cargo.toml",
        "[package]\nname = \"a\"\nedition = \"2024\"\n",
    );
    let file = p.file("crates/a/src/lib.rs", "");
    assert_eq!(rust_edition(&search_dirs(&file, Some(&p.0))), "2024");
    let file = p.file("src/main.rs", "");
    assert_eq!(rust_edition(&search_dirs(&file, Some(&p.0))), "2021");
}

#[test]
fn the_search_stops_at_the_project_root() {
    let p = Project::new("dirs");
    let file = p.file("a/b/c.ts", "");
    let dirs = search_dirs(&file, Some(&p.0));
    assert_eq!(dirs, vec![p.0.join("a/b"), p.0.join("a"), p.0.clone()]);
}
