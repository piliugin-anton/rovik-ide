//! Guessing, from a command's text, whether it will keep the CPU busy.
//!
//! The guess decides whether a command waits for a slot before it starts
//! (queue.rs). It can't be exact: `cargo build` with nothing to do takes a
//! second, after `cargo clean` it takes minutes. So it's only the first
//! word: past runs of the same command (history.rs) override it, and the
//! tracker (tracker.rs) corrects it within seconds while the command runs.
//!
//! Commands are split with the permission checker's parser (`&&`, `;`,
//! `|`), and wrappers are followed to what they run: `npm run x` to the
//! script in package.json, `just r` / `task r` to the recipe, `./build.sh`
//! to its lines. A command is as heavy as its heaviest part.

use crate::agent_run::permissions::parse_command;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Class {
    /// Finishes quickly with little CPU: `ls`, `git status`, `cargo fmt`.
    Light,
    /// Nothing known: started at once, watched.
    Unknown,
    /// A server or watcher: busy at times, runs until stopped.
    LongRunning,
    /// A build, a test run, a type check: waits for a slot.
    Heavy,
}

/// How deep wrappers are followed (a script calling a script calling …).
const MAX_DEPTH: usize = 3;
/// Scripts read to classify them are cut at this size.
const MAX_SCRIPT_BYTES: u64 = 64 * 1024;

/// The class of `command`, run in `dir` (the project root, usually).
pub fn classify(command: &str, dir: Option<&Path>) -> Class {
    classify_at(command, dir.map(Path::to_path_buf), 0)
}

fn classify_at(command: &str, dir: Option<PathBuf>, depth: usize) -> Class {
    let parsed = parse_command(command);
    let mut cwd = dir;
    let mut class = if parsed.opaque {
        Class::Unknown
    } else {
        Class::Light
    };
    for segment in &parsed.segments {
        // `cd sub && npm test`: later parts run in `sub`.
        if segment.first().map(String::as_str) == Some("cd") {
            if let (Some(base), Some(to)) = (&cwd, segment.get(1)) {
                cwd = Some(base.join(to));
            }
            continue;
        }
        class = class.max(classify_words(segment, cwd.as_deref(), depth));
        if class == Class::Heavy {
            break;
        }
    }
    class
}

/// The program's name: its file name without a Windows extension.
fn program(word: &str) -> String {
    let name = word
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stem) = name.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    name
}

/// Whether a command names files (`x.py`, `src/a.ts`), not folders or
/// the whole project.
fn file_args(words: &[String]) -> bool {
    let args: Vec<&String> = words
        .iter()
        .skip(1)
        .filter(|w| !w.starts_with('-'))
        .collect();
    !args.is_empty()
        && args.iter().all(|a| {
            let name = a.rsplit(['/', '\\']).next().unwrap_or(a);
            name.contains('.') && !name.ends_with('.') && *a != "." && *a != ".."
        })
}

fn has_flag(words: &[String], flags: &[&str]) -> bool {
    words.iter().any(|w| flags.contains(&w.as_str()))
}

/// The first word that isn't a flag, after the program.
fn subcommand(words: &[String]) -> Option<&str> {
    words
        .iter()
        .skip(1)
        .map(String::as_str)
        .find(|w| !w.starts_with('-'))
}

const LIGHT: &[&str] = &[
    "ls", "cat", "head", "tail", "grep", "rg", "fd", "find", "sed", "awk", "echo", "printf", "pwd",
    "wc", "sort", "uniq", "cut", "tr", "diff", "which", "where", "true", "false", "test", "[",
    "mkdir", "rm", "rmdir", "cp", "mv", "touch", "chmod", "ln", "stat", "file", "du", "df", "tree",
    "jq", "yq", "curl", "wget", "type", "dir", "rustfmt", "basename", "dirname", "realpath",
    "date", "sleep", "whoami", "uname", "tee", "less", "more", "open", "set", "export", "source",
    ".", "exit", "cd",
];

/// Programs whose own processes do the heavy work (also used by the tracker).
pub const HEAVY_PROGRAMS: &[&str] = &[
    "rustc",
    "rustdoc",
    "gcc",
    "g++",
    "cc",
    "c++",
    "clang",
    "clang++",
    "cl",
    "ld",
    "lld",
    "ld.lld",
    "ld64",
    "javac",
    "kotlinc",
    "swiftc",
    "scalac",
    "ghc",
    "zig",
    "nvcc",
    "msbuild",
    "xcodebuild",
    "ninja",
    "make",
    "gmake",
    "nmake",
    "bazel",
    "buck",
    "buck2",
    "gradle",
    "gradlew",
    "mvn",
    "mvnw",
    "sbt",
    "ant",
    "pytest",
    "mypy",
    "py.test",
    "tox",
    "nox",
    "jest",
    "mocha",
    "ava",
    "karma",
    "webpack",
    "rollup",
    "esbuild",
    "parcel",
    "tsc",
    "vue-tsc",
    "svelte-check",
];

const LONG_RUNNING: &[&str] = &[
    "nodemon",
    "serve",
    "http-server",
    "live-server",
    "browser-sync",
    "watchexec",
];

/// Script names that start something that keeps running.
const LONG_SCRIPTS: &[&str] = &["dev", "start", "serve", "watch", "preview"];

fn classify_words(words: &[String], cwd: Option<&Path>, depth: usize) -> Class {
    let words = crate::agent_run::permissions::unwrap_wrappers(words);
    let Some(first) = words.first() else {
        return Class::Light;
    };
    let prog = program(first);
    let sub = subcommand(words);
    let watch = has_flag(words, &["--watch", "watch"]);

    if LIGHT.contains(&prog.as_str()) {
        return Class::Light;
    }
    if LONG_RUNNING.contains(&prog.as_str()) {
        return Class::LongRunning;
    }
    match prog.as_str() {
        "git" => {
            return if matches!(sub, Some("gc" | "repack" | "fsck")) {
                Class::Heavy
            } else {
                Class::Light
            };
        }
        "cargo" => {
            return match sub {
                Some(
                    "build" | "b" | "check" | "c" | "test" | "t" | "clippy" | "run" | "r" | "bench"
                    | "doc" | "install" | "nextest" | "miri" | "llvm-cov" | "tarpaulin" | "rustc"
                    | "xtask",
                ) => Class::Heavy,
                Some("watch") => Class::LongRunning,
                Some(
                    "fmt" | "metadata" | "tree" | "add" | "remove" | "rm" | "update" | "search"
                    | "pkgid" | "locate-project" | "version" | "help" | "new" | "init" | "clean",
                ) => Class::Light,
                _ => Class::Unknown,
            };
        }
        // F05b: light checks on a file or one package (project::light_check_for).
        "ruff" => return Class::Light,
        "pyright" | "eslint" => {
            return if file_args(words) {
                Class::Light
            } else {
                Class::Heavy
            };
        }
        "go" if sub == Some("vet") => {
            let pkgs: Vec<&String> = words
                .iter()
                .skip(2)
                .filter(|w| !w.starts_with('-'))
                .collect();
            return if pkgs.len() == 1 && !pkgs[0].contains("...") {
                Class::Light
            } else {
                Class::Heavy
            };
        }
        "go" => {
            return match sub {
                Some("build" | "test" | "vet" | "install" | "run" | "generate") => Class::Heavy,
                Some("fmt" | "mod" | "env" | "version" | "list" | "doc") => Class::Light,
                _ => Class::Unknown,
            };
        }
        "tsc" | "vue-tsc" | "jest" | "webpack" => {
            return if watch || has_flag(words, &["-w"]) || sub == Some("serve") {
                Class::LongRunning
            } else {
                Class::Heavy
            };
        }
        "vitest" => {
            return if watch || has_flag(words, &["-w", "dev"]) {
                Class::LongRunning
            } else {
                Class::Heavy
            };
        }
        "vite" | "next" | "nuxt" | "nuxi" | "astro" | "remix" | "svelte-kit" | "ng" => {
            return match sub {
                Some("build" | "test" | "check" | "generate" | "export") => Class::Heavy,
                None | Some("dev" | "serve" | "start" | "preview") => Class::LongRunning,
                _ => Class::Unknown,
            };
        }
        "tauri" => {
            return match sub {
                Some("build") => Class::Heavy,
                Some("dev") => Class::LongRunning,
                _ => Class::Unknown,
            };
        }
        "python" | "python3" | "py" => {
            let module = words
                .iter()
                .position(|w| w == "-m")
                .and_then(|i| words.get(i + 1))
                .map(String::as_str);
            return match module {
                Some("pytest" | "unittest" | "tox" | "nox" | "mypy" | "pyright") => Class::Heavy,
                Some("http.server") => Class::LongRunning,
                _ => Class::Unknown,
            };
        }
        "dotnet" | "swift" | "flutter" | "dart" | "mix" | "stack" | "cabal" | "deno" => {
            return match sub {
                Some("build" | "test" | "run" | "publish" | "compile" | "check" | "bench") => {
                    Class::Heavy
                }
                _ => Class::Unknown,
            };
        }
        "docker" | "podman" => {
            let builds =
                sub == Some("build") || (sub == Some("compose") && has_flag(words, &["build"]));
            return if builds { Class::Heavy } else { Class::Unknown };
        }
        "cmake" => {
            return if has_flag(words, &["--build"]) {
                Class::Heavy
            } else {
                Class::Unknown
            }
        }
        "meson" => {
            return if matches!(sub, Some("compile" | "test")) {
                Class::Heavy
            } else {
                Class::Unknown
            }
        }
        "playwright" | "cypress" => {
            return if matches!(sub, Some("test" | "run")) {
                Class::Heavy
            } else {
                Class::Unknown
            }
        }
        _ => {}
    }
    if HEAVY_PROGRAMS.contains(&prog.as_str()) {
        return if watch {
            Class::LongRunning
        } else {
            Class::Heavy
        };
    }
    if depth >= MAX_DEPTH {
        return Class::Unknown;
    }
    match prog.as_str() {
        "npm" | "pnpm" | "yarn" | "bun" => package_script(&prog, words, cwd, depth),
        "npx" | "bunx" => tool_after(words, 1, cwd, depth),
        // `uv run pytest`, `poetry run mypy .`: what they run.
        "uv" | "poetry" | "pipenv" | "pdm" | "hatch" | "rye" if sub == Some("run") => {
            let at = words.iter().position(|w| w == "run").unwrap_or(1);
            tool_after(words, at + 1, cwd, depth)
        }
        "uvx" | "pipx" => tool_after(words, if sub == Some("run") { 2 } else { 1 }, cwd, depth),
        "just" => recipe(cwd, sub, depth, just_recipe),
        "task" => recipe(cwd, sub, depth, taskfile_recipe),
        "sh" | "bash" | "zsh" | "dash" => {
            // `bash -c 'cargo test'`: the command it runs.
            if let Some(at) = words.iter().position(|w| w == "-c" || w == "-lc") {
                return match words.get(at + 1) {
                    Some(script) => classify_at(script, cwd.map(Path::to_path_buf), depth + 1),
                    None => Class::Unknown,
                };
            }
            match words.get(1).filter(|w| !w.starts_with('-')) {
                Some(script) => shell_script(cwd, script, depth),
                None => Class::Unknown,
            }
        }
        _ if first.starts_with("./") || first.starts_with(".\\") => shell_script(cwd, first, depth),
        _ => Class::Unknown,
    }
}

/// `npm run build`, `pnpm test`, `yarn dev`, `npm exec tsc`, …
fn package_script(prog: &str, words: &[String], cwd: Option<&Path>, depth: usize) -> Class {
    let args: Vec<&str> = words
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|w| !w.starts_with('-'))
        .collect();
    // `bun test` is bun's own test runner, whatever package.json's "test"
    // script says (`bun run test` runs the script).
    if prog == "bun" && args.first() == Some(&"test") {
        return Class::Heavy;
    }
    let name = match args.as_slice() {
        ["run" | "run-script", name, ..] => *name,
        ["exec" | "dlx" | "x", ..] => return tool_after(words, 2, cwd, depth),
        ["test" | "t" | "tst", ..] => "test",
        ["start", ..] => "start",
        [first, ..] if prog != "npm" => first,
        _ => return Class::Unknown,
    };
    if LONG_SCRIPTS.contains(&name) {
        return Class::LongRunning;
    }
    match cwd.and_then(|d| package_json_script(d, name)) {
        Some(body) => classify_at(&body, cwd.map(Path::to_path_buf), depth + 1),
        // `bun test` (bun's own runner), or a test command with no script
        // to look at: tests are heavy.
        None if name == "test" => Class::Heavy,
        // `pnpm tsc`, `yarn vitest`: a binary, not a script.
        None if prog != "npm" => classify_words(
            &words[1..]
                .iter()
                .filter(|w| *w != "run")
                .cloned()
                .collect::<Vec<_>>(),
            cwd,
            depth + 1,
        ),
        None => Class::Unknown,
    }
}

/// `npx tsc --noEmit`: the words from `skip` on.
fn tool_after(words: &[String], skip: usize, cwd: Option<&Path>, depth: usize) -> Class {
    let rest: Vec<String> = words
        .iter()
        .skip(skip)
        .skip_while(|w| w.starts_with('-'))
        .cloned()
        .collect();
    if rest.is_empty() {
        return Class::Unknown;
    }
    classify_words(&rest, cwd, depth + 1)
}

fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SCRIPT_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn package_json_script(dir: &Path, name: &str) -> Option<String> {
    let text = read_small(&dir.join("package.json"))?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("scripts")?.get(name)?.as_str().map(str::to_string)
}

fn recipe(
    cwd: Option<&Path>,
    name: Option<&str>,
    depth: usize,
    find: fn(&Path, Option<&str>) -> Option<Vec<String>>,
) -> Class {
    let Some(dir) = cwd else {
        return Class::Unknown;
    };
    match find(dir, name) {
        Some(lines) => lines
            .iter()
            .map(|l| classify_at(l, Some(dir.to_path_buf()), depth + 1))
            .max()
            .unwrap_or(Class::Light),
        None => Class::Unknown,
    }
}

/// A justfile recipe's lines: the indented lines under `name:` (or the
/// first recipe when no name is given).
fn just_recipe(dir: &Path, name: Option<&str>) -> Option<Vec<String>> {
    let text = ["justfile", "Justfile", ".justfile"]
        .iter()
        .find_map(|f| read_small(&dir.join(f)))?;
    let mut lines = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if !line.starts_with([' ', '\t']) {
            if inside {
                break;
            }
            let head = line
                .split(':')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("");
            let is_recipe = line.contains(':')
                && !line.contains(":=")
                && !head.is_empty()
                && !line.starts_with(['#', '@']);
            inside = is_recipe && name.is_none_or(|n| n == head);
            continue;
        }
        if inside {
            let body = line.trim().trim_start_matches(['@', '-']).to_string();
            if !body.is_empty() && !body.starts_with('#') {
                lines.push(body);
            }
        }
    }
    (!lines.is_empty()).then_some(lines)
}

/// A Taskfile task's `cmds`, read loosely: the `- ` items under `cmds:`
/// inside `name:`.
fn taskfile_recipe(dir: &Path, name: Option<&str>) -> Option<Vec<String>> {
    let text = ["Taskfile.yml", "Taskfile.yaml", "taskfile.yml"]
        .iter()
        .find_map(|f| read_small(&dir.join(f)))?;
    let name = name.unwrap_or("default");
    let mut lines = Vec::new();
    let mut task_indent = None;
    let mut in_cmds = false;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        match task_indent {
            None => {
                if trimmed == format!("{name}:") {
                    task_indent = Some(indent);
                }
            }
            Some(at) => {
                if !trimmed.is_empty() && indent <= at {
                    break;
                }
                if trimmed == "cmds:" {
                    in_cmds = true;
                } else if in_cmds {
                    if let Some(item) = trimmed.strip_prefix("- ") {
                        let item = item
                            .trim_start_matches("cmd:")
                            .trim()
                            .trim_matches(['"', '\'']);
                        lines.push(item.to_string());
                    } else if trimmed.ends_with(':') {
                        in_cmds = false;
                    }
                }
            }
        }
    }
    (!lines.is_empty()).then_some(lines)
}

/// A shell script inside the project: its lines, comments skipped.
fn shell_script(cwd: Option<&Path>, script: &str, depth: usize) -> Class {
    let Some(dir) = cwd else {
        return Class::Unknown;
    };
    let Some(text) = read_small(&dir.join(script)) else {
        return Class::Unknown;
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| classify_at(l, Some(dir.to_path_buf()), depth + 1))
        .max()
        .unwrap_or(Class::Light)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project(files: &[(&str, &str)]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "classify-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (path, content) in files {
            let p = dir.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        dir
    }

    #[test]
    fn known_programs() {
        let c = |cmd: &str| classify(cmd, None);
        assert_eq!(c("cargo build --release"), Class::Heavy);
        assert_eq!(c("cargo test -p foo some_test"), Class::Heavy);
        assert_eq!(c("cargo fmt"), Class::Light);
        assert_eq!(c("cargo watch -x check"), Class::LongRunning);
        assert_eq!(c("go test ./..."), Class::Heavy);
        assert_eq!(c("tsc --noEmit"), Class::Heavy);
        assert_eq!(c("tsc --watch"), Class::LongRunning);
        assert_eq!(c("vite build"), Class::Heavy);
        assert_eq!(c("vite"), Class::LongRunning);
        assert_eq!(c("python -m pytest -q"), Class::Heavy);
        assert_eq!(c("pytest"), Class::Heavy);
        // F05b: light checks on one file or package.
        assert_eq!(c("ruff check src/a.py"), Class::Light);
        assert_eq!(c("pyright src/a.py"), Class::Light);
        assert_eq!(c("pyright"), Class::Heavy);
        assert_eq!(c("npx eslint src/a.ts"), Class::Light);
        assert_eq!(c("eslint ."), Class::Heavy);
        assert_eq!(c("go vet ./pkg/util"), Class::Light);
        assert_eq!(c("go vet ./..."), Class::Heavy);
        assert_eq!(c("python -m pytest tests/test_a.py"), Class::Heavy);
        assert_eq!(c("make"), Class::Heavy);
        assert_eq!(c("ls -la"), Class::Light);
        assert_eq!(c("git status"), Class::Light);
        assert_eq!(c("git gc"), Class::Heavy);
        assert_eq!(c("python script.py"), Class::Unknown);
        assert_eq!(c("cargo.exe build"), Class::Heavy);
        assert_eq!(c("/usr/local/bin/cargo build"), Class::Heavy);
    }

    #[test]
    fn the_heaviest_part_wins_and_wrappers_are_skipped() {
        let c = |cmd: &str| classify(cmd, None);
        assert_eq!(c("cd src-tauri && cargo test"), Class::Heavy);
        assert_eq!(c("ls && echo done"), Class::Light);
        assert_eq!(c("RUST_LOG=1 env FOO=bar time cargo check"), Class::Heavy);
        assert_eq!(c("cargo build 2>&1 | tail -20"), Class::Heavy);
        assert_eq!(c("echo $(date)"), Class::Unknown);
    }

    #[test]
    fn package_scripts_are_followed() {
        let dir = project(&[(
            "package.json",
            r#"{ "scripts": { "build": "vite build", "test": "node scripts/test.mjs", "dev": "vite", "lint": "tsc --noEmit && echo ok", "fmt": "prettier --write ." } }"#,
        )]);
        let c = |cmd: &str| classify(cmd, Some(&dir));
        assert_eq!(c("npm run build"), Class::Heavy);
        assert_eq!(c("pnpm lint"), Class::Heavy);
        assert_eq!(c("yarn dev"), Class::LongRunning);
        assert_eq!(c("npm start"), Class::LongRunning);
        assert_eq!(c("npm test"), Class::Unknown);
        assert_eq!(c("npm run fmt"), Class::Unknown);
        assert_eq!(c("npx tsc --noEmit"), Class::Heavy);
        assert_eq!(c("pnpm exec vitest run"), Class::Heavy);
        assert_eq!(c("pnpm vitest run"), Class::Heavy);
    }

    #[test]
    fn scripts_in_a_subfolder_after_cd() {
        let dir = project(&[(
            "web/package.json",
            r#"{ "scripts": { "check": "tsc -p ." } }"#,
        )]);
        assert_eq!(
            classify("cd web && npm run check", Some(&dir)),
            Class::Heavy
        );
        assert_eq!(classify("npm run check", Some(&dir)), Class::Unknown);
    }

    #[test]
    fn recipes_and_shell_scripts() {
        let dir = project(&[
            ("justfile", "set shell := [\"bash\", \"-c\"]\n\nfmt:\n    cargo fmt\n\ntest:\n    @echo testing\n    cargo test --all\n"),
            ("Taskfile.yml", "version: '3'\ntasks:\n  build:\n    cmds:\n      - go build ./...\n  hello:\n    cmds:\n      - echo hi\n"),
            ("build.sh", "#!/bin/sh\n# builds everything\nset -e\ncargo build --release\n"),
            ("tidy.sh", "#!/bin/sh\nrm -rf out\nmkdir out\n"),
        ]);
        let c = |cmd: &str| classify(cmd, Some(&dir));
        assert_eq!(c("just test"), Class::Heavy);
        assert_eq!(c("just fmt"), Class::Light);
        assert_eq!(c("task build"), Class::Heavy);
        assert_eq!(c("task hello"), Class::Light);
        assert_eq!(c("./build.sh"), Class::Heavy);
        assert_eq!(c("sh tidy.sh"), Class::Light);
        assert_eq!(c("bash missing.sh"), Class::Unknown);
    }

    #[test]
    fn wrappers_and_runners_are_seen_through() {
        let c = |cmd: &str| classify(cmd, None);
        for heavy in [
            "timeout 600 cargo test",
            "timeout -s KILL 5m cargo build",
            "xargs cargo test",
            "bash -c 'cargo test'",
            "sh -c \"npm test\"",
            "uv run pytest",
            "poetry run mypy .",
            "pipenv run pytest tests/",
            "env CI=1 timeout 60 cargo check",
        ] {
            assert_eq!(c(heavy), Class::Heavy, "{heavy}");
        }
        for light in [
            "uvx ruff check a.py",
            "timeout 5 ls",
            "bash -c 'git status'",
            "npx eslint src/a.ts",
        ] {
            assert_eq!(c(light), Class::Light, "{light}");
        }
    }
}
