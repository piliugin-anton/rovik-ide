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
    /// which has only the fake tools, so the script brings its own. It's run
    /// once before it's handed over (`runnable`), which `PROBE` makes it end
    /// at once.
    #[cfg(unix)]
    fn tool(&self, rel: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = self.file(
            rel,
            &format!("#!/bin/sh\n[ -n \"${PROBE}\" ] && exit 0\nPATH=/usr/bin:/bin\n{script}\n"),
        );
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        runnable(&p);
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

/// Set, it makes a test's script end at once (`Project::tool`).
#[cfg(unix)]
const PROBE: &str = "FORMATTER_TEST_PROBE";

/// Runs a script just written, once, until it isn't busy. A test running
/// alongside that starts a process while the script is being written can
/// keep it open for writing for a moment, until that process's program
/// replaces it, and Linux won't run a file open for writing ("Text file
/// busy"). Once one run went through, nothing has it open for writing.
#[cfg(unix)]
fn runnable(script: &Path) {
    for _ in 0..500 {
        match Command::new(script).env(PROBE, "1").status() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(status) => {
                assert!(status.success(), "{}: {status}", script.display());
                return;
            }
            Err(e) => panic!("{}: {e}", script.display()),
        }
    }
    panic!("{} stayed busy", script.display());
}

#[cfg(unix)]
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

#[test]
fn the_project_root_matches_whatever_the_case_where_case_doesnt_count() {
    let p = Project::new("case");
    let file = p.file("a/b.ts", "");
    let upper = PathBuf::from(p.0.to_string_lossy().to_uppercase());
    let dirs = search_dirs(&file, Some(&upper));
    if cfg!(any(windows, target_os = "macos")) {
        assert_eq!(dirs, vec![p.0.join("a"), p.0.clone()]);
    } else {
        // Not the project's folder: the search goes on past it.
        assert!(dirs.len() > 2);
    }
}

#[cfg(windows)]
#[test]
fn windows_finds_cmd_wrappers_and_virtualenv_scripts() {
    let p = Project::new("win-lookup");
    p.file("package.json", r#"{"devDependencies":{"prettier":"3"}}"#);
    // npm puts a sh script, a .cmd and a .ps1 in .bin; only the .cmd runs.
    p.file("node_modules/.bin/prettier", "#!/bin/sh\n");
    let cmd = p.file("node_modules/.bin/prettier.cmd", "@echo off\r\n");
    p.file("node_modules/.bin/prettier.ps1", "");
    let ruff = p.file(".venv/Scripts/ruff.exe", "");
    let ts = p.file("src/a.ts", "");
    let py = p.file("a.py", "");
    let pick = |f: &Path, l: &str| {
        resolve(f, l, Some(&p.0), &p.path_env())
            .found
            .map(|r| r.program)
    };
    assert!(pick(&ts, "typescript").is_some_and(|f| same_path(&f, &cmd)));
    assert!(pick(&py, "python").is_some_and(|f| same_path(&f, &ruff)));
}

#[cfg(windows)]
#[test]
fn windows_runs_cmd_wrappers_with_paths_that_have_spaces() {
    let p = Project::new("win-run");
    p.file(".prettierrc", "{}");
    // Prints the --stdin-filepath argument, as cmd.exe received it.
    p.file(
        "node_modules/.bin/prettier.cmd",
        "@echo off\r\necho %~2\r\n",
    );
    let file = p.file("my project/a b.ts", "");
    let out = format_text(&file, "typescript", Some(&p.0), "x\n".into(), &p.path_env());
    assert_eq!(
        out.map(|o| o.trim_end().to_string()),
        Ok(file.to_string_lossy().into_owned())
    );
}

/// A wrapper script whose real tool hangs: the timeout takes down both,
/// and doesn't wait on the pipes the tool still holds.
#[cfg(unix)]
#[test]
fn a_hanging_formatter_is_killed_with_everything_it_started() {
    let p = Project::new("hang");
    let marker = p.0.join("still-running");
    let tool = p.tool(
        "hang.sh",
        &format!("sh -c 'sleep 3; touch {}' & wait", marker.display()),
    );
    let start = Instant::now();
    let out = run(
        Command::new(&tool),
        "x".into(),
        &[0],
        Duration::from_secs(1),
    );
    assert_eq!(out, Err("timed out after 1 s".into()));
    assert!(start.elapsed() < Duration::from_secs(3));
    std::thread::sleep(Duration::from_secs(4));
    assert!(!marker.exists(), "the tool's child outlived the timeout");
}

#[cfg(windows)]
#[test]
fn a_hanging_cmd_wrapper_is_killed_with_everything_it_started() {
    let p = Project::new("win-hang");
    let marker = p.0.join("still-running");
    // The wrapper's child does the (hanging) work, as node does for a .cmd.
    let tool = p.file(
        "hang.cmd",
        &format!(
            "@echo off\r\ncmd /c \"%SystemRoot%\\System32\\PING.EXE -n 4 127.0.0.1 >nul & echo x > \"{}\"\"\r\n",
            marker.display()
        ),
    );
    let start = Instant::now();
    let out = run(
        Command::new(&tool),
        "x".into(),
        &[0],
        Duration::from_secs(1),
    );
    assert_eq!(out, Err("timed out after 1 s".into()));
    assert!(start.elapsed() < Duration::from_secs(3));
    std::thread::sleep(Duration::from_secs(5));
    assert!(!marker.exists(), "the wrapper's child outlived the timeout");
}

/// A wrapper that has exited, leaving a child at work that holds its
/// output: the timeout ends the child too. (Given the wrapper's process,
/// gone by then, taskkill found nothing to end; the child is in its job.)
#[cfg(windows)]
#[test]
fn a_child_a_wrapper_left_holding_its_output_is_killed_at_the_timeout() {
    let p = Project::new("win-left");
    let marker = p.0.join("still-running");
    let child = p.file(
        "child.cmd",
        &format!(
            "@echo off\r\n%SystemRoot%\\System32\\PING.EXE -n 4 127.0.0.1 >nul\r\necho x > \"{}\"\r\n",
            marker.display()
        ),
    );
    let tool = p.file(
        "left.cmd",
        &format!(
            "@echo off\r\nstart \"\" /b cmd /c \"{}\"\r\n",
            child.display()
        ),
    );
    let start = Instant::now();
    let out = run(
        Command::new(&tool),
        "x".into(),
        &[0],
        Duration::from_secs(1),
    );
    assert_eq!(out, Err("timed out after 1 s".into()));
    assert!(start.elapsed() < Duration::from_secs(3));
    std::thread::sleep(Duration::from_secs(5));
    assert!(!marker.exists(), "the wrapper's child outlived the timeout");
}
