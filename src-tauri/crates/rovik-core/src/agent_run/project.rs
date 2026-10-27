//! What the agent is told about the open project, and which commands check
//! its changes. Detection is in project_profile.rs; the user turns the
//! detected checks on and corrects them per project (stored with
//! `save_project_settings`), and those win once saved. Until then every
//! detected check is off: detection never turns one on for the user.

use crate::folders::Folders;
use crate::project_profile::{self, ProjectProfile};
use crate::restricted_files::RestrictedFiles;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const CHECK_KINDS: &[&str] = &["typecheck", "lint", "test", "build"];

pub fn check_label(kind: &str) -> &'static str {
    match kind {
        "typecheck" => "Type check",
        "lint" => "Lint",
        "test" => "Tests",
        "build" => "Build",
        _ => "Check",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectCheck {
    pub kind: String,
    pub command: String,
    pub enabled: bool,
}

/// The user's saved checks for a project, if they saved any.
fn saved_checks(settings: &serde_json::Value) -> Option<Vec<ProjectCheck>> {
    let checks = settings.get("checks")?.as_array()?;
    Some(
        checks
            .iter()
            .filter_map(|c| {
                let kind = c.get("kind")?.as_str()?;
                let command = c.get("command")?.as_str()?;
                CHECK_KINDS.contains(&kind).then(|| ProjectCheck {
                    kind: kind.to_string(),
                    command: command.to_string(),
                    enabled: c.get("enabled") != Some(&serde_json::Value::Bool(false)),
                })
            })
            .collect(),
    )
}

/// The checks in effect: the user's list when they saved one, else what's
/// detected, each off. A check runs once the user turns it on (Settings →
/// Project checks, which saves the list), never because it was detected.
pub fn effective_checks(
    profile: &ProjectProfile,
    settings: &serde_json::Value,
) -> Vec<ProjectCheck> {
    if let Some(saved) = saved_checks(settings) {
        return saved;
    }
    profile
        .checks
        .iter()
        .map(|c| ProjectCheck {
            kind: c.kind.clone(),
            command: c.command.clone(),
            enabled: false,
        })
        .collect()
}

/// Checks in the order they run: cheap and broad first.
pub fn ordered_checks(checks: &[ProjectCheck]) -> Vec<ProjectCheck> {
    let mut out = checks.to_vec();
    out.sort_by_key(|c| {
        CHECK_KINDS
            .iter()
            .position(|k| *k == c.kind)
            .unwrap_or(CHECK_KINDS.len())
    });
    out
}

/// The "## Project" section of the system prompt: stack, the services its
/// compose files declare (`services`, services::prompt_line), the check
/// commands (and that the IDE runs them when verification is on), and the
/// project's own agent instructions.
pub fn project_context(
    profile: &ProjectProfile,
    services: Option<&str>,
    checks: &[ProjectCheck],
    verify_on: bool,
) -> String {
    let mut parts = vec!["## Project".to_string()];
    if !profile.stack.is_empty() {
        parts.push(format!("Stack: {}.", profile.stack.join(", ")));
    }
    if let Some(services) = services {
        parts.push(services.to_string());
    }
    parts.extend(checks_part(checks, verify_on));
    if let Some(notes) = &profile.instructions {
        let label = format!(
            "The project's instructions for agents ({}); follow them:",
            notes.file
        );
        parts.push(instructions_part(&label, &notes.text, notes.truncated));
    }
    if parts.len() > 1 {
        parts.join("\n\n")
    } else {
        String::new()
    }
}

/// The project section's checks: those turned on, and what the IDE does
/// with them when verification is on. Nothing without one.
fn checks_part(checks: &[ProjectCheck], verify_on: bool) -> Vec<String> {
    let enabled: Vec<ProjectCheck> = ordered_checks(checks)
        .into_iter()
        .filter(|c| c.enabled)
        .collect();
    if enabled.is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut list = vec![if verify_on {
        "Checks: when you finish and have changed files, the IDE runs these itself and sends you any failures to fix:"
    } else {
        "Checks you can run with bash after changing code:"
    }
    .to_string()];
    list.extend(
        enabled
            .iter()
            .map(|c| format!("- {}: `{}`", check_label(&c.kind), c.command)),
    );
    parts.push(list.join("\n"));
    if verify_on && enabled.iter().any(|c| !c.command.trim().is_empty()) {
        parts.push(
            "Run a relevant check yourself before finishing when it helps you get the change right. \
             If a check fails for reasons unrelated to your change (it fails on the original code too), \
             say so plainly and don't change unrelated code to make it pass. \
             Do not add a verification step (running checks, tests, build, or lint) to your todo checklist: \
             the IDE runs the checks above itself after you finish and sends any failures back to you to fix. \
             Keep the checklist to the work itself. \
             In your final message, say what was verified."
                .into(),
        );
    }
    parts
}

/// Instructions for agents under `label`, marked when they were cut.
fn instructions_part(label: &str, text: &str, truncated: bool) -> String {
    let more = if truncated { "\n[…truncated]" } else { "" };
    format!("{label}\n\n{text}{more}")
}

/// What a workspace chat's run is told of its folders (4.4, D25), with
/// their checks and restricted files. Each folder is read where the run
/// sees it (`root`, the chat's view: `root/<name>`; None: at its own path)
/// as when it's open alone, by its own settings, and said under its name:
/// its stack (`below`: V09's one level down inside it), its services, its
/// checks run in it (`cd api && …`) and its instructions, the folders
/// sharing [`project_profile::WORKSPACE_INSTRUCTIONS_MAX_CHARS`]. Then where
/// git works, and (`since`: the folders' names at the chat's last run)
/// which joined and which left.
pub fn load_folders(
    config_dir: &Path,
    root: Option<&Path>,
    folders: &Folders,
    verify_on: bool,
    below: bool,
    since: Option<&[String]>,
) -> (Vec<ProjectCheck>, String, RestrictedFiles) {
    let mut stack = Vec::new();
    let mut checks = Vec::new();
    let mut services = Vec::new();
    let mut notes = Vec::new();
    let mut restricted = Vec::new();
    for folder in folders.iter() {
        let name = folder.name.as_str();
        let dir = root.map_or_else(|| folder.path.clone(), |r| r.join(name));
        let settings =
            project_profile::load_project_settings(config_dir, &folder.path.to_string_lossy());
        let profile = project_profile::detect(&dir);
        let own = effective_checks(&profile, &settings);
        checks.extend(own.iter().map(|c| ProjectCheck {
            kind: c.kind.clone(),
            command: in_folder(name, &c.command),
            enabled: c.enabled,
        }));
        stack.extend(profile.stack.iter().map(|s| format!("{s} ({name}/)")));
        if below && saved_checks(&settings).is_none() {
            for (sub, found) in project_profile::detect_below(&dir, &profile) {
                let at = format!("{name}/{sub}");
                stack.extend(found.stack.iter().map(|s| format!("{s} ({at}/)")));
                checks.extend(found.checks.iter().map(|c| ProjectCheck {
                    kind: c.kind.clone(),
                    command: in_folder(&at, &c.command),
                    enabled: false,
                }));
            }
        }
        if let Some(line) = crate::services::prompt_line(&dir) {
            services.push(format!("{name}: {line}"));
        }
        if let Some((file, text)) = project_profile::instructions_file(&dir) {
            notes.push((name, file, text));
        }
        restricted.push((name.to_string(), RestrictedFiles::from_settings(&settings)));
    }
    let mut parts = vec![
        "## Project".to_string(),
        workspace_paragraph(folders),
        git_line(folders),
    ];
    parts.extend(joined_and_left(folders, since));
    if !stack.is_empty() {
        parts.push(format!("Stack: {}.", stack.join(", ")));
    }
    parts.extend(services);
    parts.extend(checks_part(&checks, verify_on));
    let lengths: Vec<usize> = notes.iter().map(|(_, _, t)| t.chars().count()).collect();
    let rooms = shares(&lengths, project_profile::WORKSPACE_INSTRUCTIONS_MAX_CHARS);
    for ((name, file, text), room) in notes.iter().zip(rooms) {
        let (text, truncated) = project_profile::cut(text, room);
        let label =
            format!("{name}'s instructions for agents ({name}/{file}); follow them in {name}:");
        parts.push(instructions_part(&label, &text, truncated));
    }
    (
        checks,
        parts.join("\n\n"),
        RestrictedFiles::of_folders(restricted),
    )
}

/// Each of several texts' room in `limit` characters, for texts of
/// `lengths`: an even share, what one doesn't use going to the others
/// (D25).
fn shares(lengths: &[usize], limit: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lengths.len()).collect();
    order.sort_by_key(|&i| lengths[i]);
    let mut rooms = vec![0; lengths.len()];
    let mut left = limit;
    for (done, i) in order.into_iter().enumerate() {
        rooms[i] = lengths[i].min(left / (lengths.len() - done));
        left -= rooms[i];
    }
    rooms
}

/// How a workspace chat's paths and commands go (4.4).
fn workspace_paragraph(folders: &Folders) -> String {
    let names: Vec<String> = folders.iter().map(|f| format!("`{}`", f.name)).collect();
    let first = &folders.first().name;
    format!(
        "This chat works in folders of a workspace: each is a top-level folder of the project root ({}), and every path starts with one (`{first}/src/main.rs`); nothing can be made at the root itself. Run a folder's commands with `cd {first} && …`. A folder's checks and instructions are for its files.",
        names.join(", ")
    )
}

/// Where git works in the chat's view: in a folder that's a repository,
/// and not in one inside a repository, whose root isn't in the view.
fn git_line(folders: &Folders) -> String {
    let said: Vec<String> = folders
        .iter()
        .map(|f| {
            let name = &f.name;
            if f.path.join(".git").exists() {
                format!("`{name}` is a repository (`git -C {name} status`)")
            } else if git2::Repository::discover(&f.path).is_ok() {
                format!("`{name}` is a folder of a repository whose root isn't in the view, so git doesn't work in it")
            } else {
                format!("`{name}` isn't in a repository")
            }
        })
        .collect();
    format!("Git: {}.", said.join("; "))
}

/// The folders that joined the chat and those that left it since its last
/// run (`since`: their names then). None when they're as they were, or it's
/// the chat's first run.
fn joined_and_left(folders: &Folders, since: Option<&[String]>) -> Option<String> {
    let since = since?;
    let named = |names: Vec<&str>| {
        names
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let joined: Vec<&str> = folders
        .iter()
        .map(|f| f.name.as_str())
        .filter(|n| !since.iter().any(|s| s == n))
        .collect();
    let left: Vec<&str> = since
        .iter()
        .map(String::as_str)
        .filter(|s| !folders.iter().any(|f| f.name == *s))
        .collect();
    let mut changed = Vec::new();
    if !joined.is_empty() {
        changed.push(format!("{} joined", named(joined)));
    }
    if !left.is_empty() {
        changed.push(format!(
            "{} left, so earlier messages may name files that aren't here any more",
            named(left)
        ));
    }
    (!changed.is_empty()).then(|| {
        format!(
            "Since this chat's last run, its folders changed: {}.",
            changed.join("; ")
        )
    })
}

/// Detected fresh (a few file reads): the project's checks and its prompt section.
/// The project's checks and its section of the system prompt. `root` is
/// where the run works (an isolated chat's view); `settings_root`, the
/// project folder its saved settings are kept under, in `config_dir`.
pub fn load(
    config_dir: &Path,
    root: &str,
    settings_root: &str,
    verify_on: bool,
) -> (Vec<ProjectCheck>, String) {
    load_for(config_dir, root, settings_root, verify_on, false)
}

/// `load`, and with `below` (an Autopilot task's run, V09) the stacks one
/// folder down too, each check in its folder, unless the user saved the
/// project's checks: theirs stand. Those checks are detected ones, so off
/// like the project's own: with none on, a task is checked by its plan's
/// validation commands (V02, run.rs).
pub fn load_for(
    config_dir: &Path,
    root: &str,
    settings_root: &str,
    verify_on: bool,
    below: bool,
) -> (Vec<ProjectCheck>, String) {
    if !Path::new(root).is_dir() {
        return (Vec::new(), String::new());
    }
    let mut profile = project_profile::detect(Path::new(root));
    let settings = project_profile::load_project_settings(config_dir, settings_root);
    let mut checks = effective_checks(&profile, &settings);
    if below && saved_checks(&settings).is_none() {
        for (folder, found) in project_profile::detect_below(Path::new(root), &profile) {
            for stack in &found.stack {
                let named = format!("{stack} ({folder}/)");
                if !profile.stack.contains(&named) {
                    profile.stack.push(named);
                }
            }
            checks.extend(found.checks.iter().map(|c| ProjectCheck {
                kind: c.kind.clone(),
                command: in_folder(&folder, &c.command),
                enabled: false,
            }));
        }
    }
    let services = crate::services::prompt_line(Path::new(root));
    let context = project_context(&profile, services.as_deref(), &checks, verify_on);
    (checks, context)
}

/// V09: `command` run in `folder` of the project: `cd api && go test ./...`.
pub fn in_folder(folder: &str, command: &str) -> String {
    let folder = if folder
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        folder.to_string()
    } else {
        format!("'{}'", folder.replace('\'', "'\\''"))
    };
    format!("cd {folder} && {command}")
}

/// V09: the folder a check runs in, as `in_folder` writes it: `api` for
/// `cd api && go test ./...`; "" for the project's own.
pub fn folder_of(command: &str) -> String {
    let Some(rest) = command.trim_start().strip_prefix("cd ") else {
        return String::new();
    };
    let Some((folder, _)) = rest.split_once(" && ") else {
        return String::new();
    };
    let folder = folder.trim();
    match folder.strip_prefix('\'').and_then(|f| f.strip_suffix('\'')) {
        Some(quoted) => quoted.replace("'\\''", "'"),
        None => folder.to_string(),
    }
}

/// The project section for estimates (the context meter), re-detected at
/// most every few seconds: estimates follow every message of a run.
pub fn cached_context(config_dir: &Path, root: &str, verify_on: bool) -> String {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::{Duration, Instant};
    type Cache = HashMap<(String, bool), (Instant, String)>;
    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);
    const FRESH: Duration = Duration::from_secs(5);

    let key = (root.to_string(), verify_on);
    if let Some((at, text)) = CACHE.lock().ok().and_then(|c| c.get(&key).cloned()) {
        if at.elapsed() < FRESH {
            return text;
        }
    }
    let (_, text) = load(config_dir, root, root, verify_on);
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|_, (at, _)| at.elapsed() < FRESH);
        cache.insert(key, (Instant::now(), text.clone()));
    }
    text
}

/*
 * ---------------------------------------------------------------------------
 * Test files (E01): where a stack keeps its tests, and how to run some
 * ---------------------------------------------------------------------------
 */

/// Whether `path` is a test file by its stack's conventions (`*.test.ts`,
/// `__tests__/`, `tests/`, `*_test.go`, `test_*.py`, Rust `tests.rs`
/// modules…).
pub fn is_test_path(path: &str) -> bool {
    super::repair::is_test_path(path)
}

fn split_ext(path: &str) -> (&str, &str) {
    let name_at = path.rfind('/').map_or(0, |i| i + 1);
    match path[name_at..].rfind('.') {
        Some(dot) if dot > 0 => (&path[..name_at + dot], &path[name_at + dot + 1..]),
        _ => (path, ""),
    }
}

/// Where a test for `source` (repo-relative) goes by its stack's
/// conventions, most usual first. Empty for files that aren't code.
pub fn test_paths_for(source: &str) -> Vec<String> {
    let p = source.replace('\\', "/");
    if is_test_path(&p) {
        return vec![p];
    }
    let (stem, ext) = split_ext(&p);
    let dir = p
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();
    let name = stem.rsplit('/').next().unwrap_or(stem);
    let in_dir = |f: &str| {
        if dir.is_empty() {
            f.to_string()
        } else {
            format!("{dir}/{f}")
        }
    };
    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => vec![
            format!("{stem}.test.{ext}"),
            in_dir(&format!("__tests__/{name}.test.{ext}")),
        ],
        "go" => vec![format!("{stem}_test.go")],
        "py" => {
            let mut out = vec![
                format!("tests/test_{name}.py"),
                in_dir(&format!("test_{name}.py")),
            ];
            out.dedup();
            out
        }
        "rs" => {
            // A module's tests in a `tests.rs` beside it (this repository's
            // way), or an integration test in the crate's `tests/`.
            let module_dir = if name == "mod" || name == "lib" || name == "main" {
                dir.clone()
            } else {
                stem.to_string()
            };
            let crate_root = p
                .find("src/")
                .map(|i| p[..i].to_string())
                .unwrap_or_default();
            vec![
                format!("{module_dir}/tests.rs"),
                format!("{crate_root}tests/{name}.rs"),
            ]
        }
        _ => Vec::new(),
    }
}

/// The JavaScript test runner the project uses, from its package.json.
fn js_runner(root: &Path) -> Option<&'static str> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let has = |name: &str| {
        ["dependencies", "devDependencies"]
            .iter()
            .any(|k| v.get(k).and_then(|d| d.get(name)).is_some())
    };
    if has("vitest") {
        Some("npx vitest run")
    } else if has("jest") {
        Some("npx jest")
    } else {
        None
    }
}

/// The nearest folder above `rel` (or the root) with `marker` in it.
fn crate_dir(root: &Path, rel: &str, marker: &str) -> Option<String> {
    let mut dir = Path::new(rel).parent();
    while let Some(d) = dir {
        if root.join(d).join(marker).is_file() {
            return Some(d.to_string_lossy().replace('\\', "/"));
        }
        dir = d.parent();
    }
    root.join(marker).is_file().then(String::new)
}

/// A Rust test file's filter: `tests/x.rs` is `--test x`; a module file
/// under `src/` is its module path (`src/a/b/tests.rs` → `a::b::tests`).
fn rust_filter(rel_to_crate: &str) -> Option<String> {
    if let Some(name) = rel_to_crate.strip_prefix("tests/") {
        return Some(format!("--test {}", name.trim_end_matches(".rs")));
    }
    let module = rel_to_crate.strip_prefix("src/")?.trim_end_matches(".rs");
    let module = module.strip_suffix("/mod").unwrap_or(module);
    // The crate root is no module (`src/library.rs` is `library`).
    if matches!(module, "lib" | "main") {
        return None;
    }
    Some(module.replace('/', "::"))
}

/// E01: a command that runs just `files` (test files, repo-relative):
/// vitest or jest by path, `cargo test` by test target or module, pytest by
/// path, `go test` by package. Anything else, or several stacks at once,
/// falls back to the project's whole test check (`test_check`).
pub fn test_command_for(root: &Path, files: &[String], test_check: Option<&str>) -> Option<String> {
    let fallback = || {
        test_check
            .map(str::to_string)
            .filter(|c| !c.trim().is_empty())
    };
    let files: Vec<String> = files.iter().map(|f| f.replace('\\', "/")).collect();
    if files.is_empty() {
        return fallback();
    }
    let ext = |f: &String| split_ext(f).1.to_string();
    let exts: std::collections::BTreeSet<String> = files.iter().map(ext).collect();
    let quote = |f: &str| {
        if f.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
        {
            f.to_string()
        } else {
            format!("'{}'", f.replace('\'', "'\\''"))
        }
    };
    let all = |pred: &dyn Fn(&str) -> bool| exts.iter().all(|e| pred(e));
    if all(&|e| ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"].contains(&e)) {
        return match js_runner(root) {
            Some(runner) => Some(format!(
                "{runner} {}",
                files.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" ")
            )),
            None => fallback(),
        };
    }
    if all(&|e| e == "py") {
        return Some(format!(
            "python -m pytest {}",
            files.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" ")
        ));
    }
    if all(&|e| e == "go") {
        let mut pkgs: Vec<String> = files
            .iter()
            .map(|f| match f.rsplit_once('/') {
                Some((d, _)) => format!("./{d}"),
                None => ".".into(),
            })
            .collect();
        pkgs.dedup();
        return Some(format!("go test {}", pkgs.join(" ")));
    }
    if all(&|e| e == "rs") {
        let mut parts: Vec<(String, String)> = Vec::new();
        for f in &files {
            let dir = crate_dir(root, f, "Cargo.toml")?;
            let rel = if dir.is_empty() {
                f.clone()
            } else {
                f.strip_prefix(&format!("{dir}/")).unwrap_or(f).to_string()
            };
            parts.push((dir, rust_filter(&rel)?));
        }
        let dirs: std::collections::BTreeSet<&String> = parts.iter().map(|(d, _)| d).collect();
        if dirs.len() != 1 || parts.len() != 1 {
            // cargo takes one filter (or test target) at a time.
            return match (dirs.len(), parts.first()) {
                (1, Some((dir, _))) if parts.iter().all(|(_, f)| !f.starts_with("--test")) => {
                    let common = common_module(parts.iter().map(|(_, f)| f.as_str()));
                    Some(cargo(dir, &common))
                }
                _ => fallback(),
            };
        }
        let (dir, filter) = &parts[0];
        return Some(cargo(dir, filter));
    }
    fallback()
}

/// R03: whether a test run's output says it ran no test at all, which its
/// exit code doesn't: `cargo test` whose filter matches nothing (a Rust
/// test file in a module not declared yet: "running 0 tests" from every
/// binary), `go test` ("[no tests to run]", "[no test files]"), Node's
/// test runner ("tests 0"), mocha ("0 passing"). Such a run passes,
/// proving nothing.
pub fn ran_no_tests(output: &str) -> bool {
    let (mut none, mut some) = (false, false);
    for line in output.lines().map(str::trim) {
        let count = line
            // cargo: each test binary's "running 3 tests".
            .strip_prefix("running ")
            .and_then(|rest| {
                rest.strip_suffix(" tests")
                    .or_else(|| rest.strip_suffix(" test"))
            })
            // Node's test runner: its summary, as TAP or as its spec reporter.
            .or_else(|| line.strip_prefix("# tests "))
            .or_else(|| line.strip_prefix("ℹ tests "))
            // mocha: "3 passing (9ms)".
            .or_else(|| line.split_once(" passing").map(|(n, _)| n))
            .and_then(|n| n.trim().parse::<u64>().ok());
        match count {
            Some(0) => none = true,
            Some(_) => some = true,
            // go: a package whose tests didn't run, or one whose did.
            None if line.ends_with("[no tests to run]") || line.ends_with("[no test files]") => {
                none = true
            }
            None if line.starts_with("ok ") || line.starts_with("--- PASS") => some = true,
            None => {}
        }
    }
    none && !some
}

/// F05b: light checks a parallel worker may run on its files, per stack:
/// a file or a package at a time, never a build or a test run. Tools that
/// aren't installed are left out; a stack with nothing gets only the
/// language server's diagnostics.
pub fn light_check_for(root: &Path, files: &[String]) -> Vec<String> {
    let installed = |tool: &str| {
        root.join("node_modules/.bin").join(tool).is_file()
            || crate::process::child_var("PATH").is_some_and(|path| {
                std::env::split_paths(&path)
                    .any(|d| d.join(tool).is_file() || d.join(format!("{tool}.exe")).is_file())
            })
    };
    let mut out: Vec<String> = Vec::new();
    let mut push = |c: String| {
        if !out.contains(&c) {
            out.push(c)
        }
    };
    for f in files {
        let f = f.replace('\\', "/");
        match split_ext(&f).1 {
            "py" => {
                if installed("ruff") {
                    push(format!("ruff check {f}"));
                }
                if installed("pyright") {
                    push(format!("pyright {f}"));
                }
            }
            "go" => {
                let pkg = f
                    .rsplit_once('/')
                    .map_or(".".to_string(), |(d, _)| format!("./{d}"));
                push(format!("go vet {pkg}"));
            }
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
                if root.join("node_modules/.bin/eslint").is_file() =>
            {
                push(format!("npx eslint {f}"));
            }
            _ => {}
        }
    }
    out
}

fn cargo(dir: &str, filter: &str) -> String {
    let manifest = if dir.is_empty() {
        String::new()
    } else {
        format!(" --manifest-path {dir}/Cargo.toml")
    };
    if filter.is_empty() {
        format!("cargo test{manifest}")
    } else {
        format!("cargo test{manifest} {filter}")
    }
}

/// The module path several filters share (`a::b::x`, `a::b::y` → `a::b`).
fn common_module<'a>(filters: impl Iterator<Item = &'a str>) -> String {
    let mut common: Option<Vec<&str>> = None;
    for f in filters {
        let parts: Vec<&str> = f.split("::").collect();
        common = Some(match common {
            None => parts,
            Some(c) => c
                .iter()
                .zip(&parts)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| *a)
                .collect(),
        });
    }
    common.unwrap_or_default().join("::")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_profile::{DetectedCheck, ProjectInstructions};

    /// R03: a run that ran no test passes without proving anything.
    #[test]
    fn a_run_that_ran_no_test_is_told_apart() {
        let cargo_none = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.00s\n\n     Running tests/api.rs (target/debug/deps/api-1)\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s\n";
        assert!(ran_no_tests(cargo_none));
        let cargo_one = "running 0 tests\n\nrunning 1 test\ntest parse::tests::reads_it ... ok\n\ntest result: ok. 1 passed; 0 failed\n";
        assert!(!ran_no_tests(cargo_one));
        assert!(ran_no_tests("# tests 0\n# suites 0\n# pass 0\n# fail 0\n"));
        assert!(!ran_no_tests(
            "ok 1 - ok.txt says ok\n# tests 1\n# pass 1\n"
        ));
        assert!(ran_no_tests("ℹ tests 0\nℹ pass 0\n"));
        assert!(ran_no_tests(
            "ok  \texample.com/p\t0.002s [no tests to run]\n"
        ));
        assert!(ran_no_tests("?   \texample.com/p\t[no test files]\n"));
        assert!(!ran_no_tests("ok  \texample.com/p\t0.002s\n"));
        assert!(ran_no_tests("\n  0 passing (2ms)\n"));
        assert!(!ran_no_tests("\n  3 passing (9ms)\n"));
        assert!(!ran_no_tests(
            "PASS src/a.test.ts\nTests: 2 passed, 2 total\n"
        ));
        assert!(!ran_no_tests(""));
    }

    #[test]
    fn test_paths_follow_each_stacks_conventions() {
        assert_eq!(
            test_paths_for("src/lib/plan.ts"),
            ["src/lib/plan.test.ts", "src/lib/__tests__/plan.test.ts"]
        );
        assert_eq!(
            test_paths_for("App.tsx"),
            ["App.test.tsx", "__tests__/App.test.tsx"]
        );
        assert_eq!(
            test_paths_for("pkg/util/strings.go"),
            ["pkg/util/strings_test.go"]
        );
        assert_eq!(
            test_paths_for("more_itertools/more.py"),
            ["tests/test_more.py", "more_itertools/test_more.py"]
        );
        assert_eq!(
            test_paths_for("src-tauri/src/agent_run/plan.rs"),
            [
                "src-tauri/src/agent_run/plan/tests.rs",
                "src-tauri/tests/plan.rs"
            ]
        );
        assert_eq!(
            test_paths_for("src/overlay/mod.rs"),
            ["src/overlay/tests.rs", "tests/mod.rs"]
        );
        assert_eq!(
            test_paths_for("src/a.test.ts"),
            ["src/a.test.ts"],
            "already a test"
        );
        assert!(test_paths_for("README.md").is_empty());
        for t in [
            "src/a.test.ts",
            "src/__tests__/a.ts",
            "tests/test_x.py",
            "x_test.go",
            "src/plan/tests.rs",
            "tests/api.rs",
        ] {
            assert!(is_test_path(t), "{t}");
        }
        for t in ["src/a.ts", "src/testing.rs", "x.go"] {
            assert!(!is_test_path(t), "{t}");
        }
    }

    #[test]
    fn light_checks_per_stack_are_light() {
        let dir = std::env::temp_dir().join(format!("light-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
        std::fs::write(dir.join("node_modules/.bin/eslint"), "").unwrap();
        let checks = light_check_for(
            &dir,
            &[
                "pkg/util/a.go".into(),
                "src/a.ts".into(),
                "README.md".into(),
                "pkg/util/b.go".into(),
            ],
        );
        assert_eq!(checks, ["go vet ./pkg/util", "npx eslint src/a.ts"]);
        for c in &checks {
            assert_eq!(
                crate::workload::classify::classify(c, Some(&dir)),
                crate::workload::classify::Class::Light,
                "{c}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_commands_for_files_by_stack() {
        let dir = std::env::temp_dir().join(format!("project-tests-{}", uuid::Uuid::new_v4()));
        let put = |rel: &str, text: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        put(
            "package.json",
            r#"{ "devDependencies": { "vitest": "1" } }"#,
        );
        put("src-tauri/Cargo.toml", "[package]\nname = \"x\"\n");
        let run = |files: &[&str]| {
            let files: Vec<String> = files.iter().map(|f| f.to_string()).collect();
            test_command_for(&dir, &files, Some("npm test"))
        };
        assert_eq!(
            run(&["src/a.test.ts", "src/b c.test.ts"]).as_deref(),
            Some("npx vitest run src/a.test.ts 'src/b c.test.ts'")
        );
        assert_eq!(
            run(&["tests/test_x.py"]).as_deref(),
            Some("python -m pytest tests/test_x.py")
        );
        assert_eq!(
            run(&["pkg/a_test.go", "pkg/b_test.go"]).as_deref(),
            Some("go test ./pkg")
        );
        assert_eq!(
            run(&["src-tauri/src/agent_run/plan/tests.rs"]).as_deref(),
            Some("cargo test --manifest-path src-tauri/Cargo.toml agent_run::plan::tests")
        );
        // A module whose name starts like the crate root's file.
        assert_eq!(
            run(&["src-tauri/src/library.rs"]).as_deref(),
            Some("cargo test --manifest-path src-tauri/Cargo.toml library")
        );
        assert_eq!(
            run(&["src-tauri/src/maintenance/mod.rs"]).as_deref(),
            Some("cargo test --manifest-path src-tauri/Cargo.toml maintenance")
        );
        assert_eq!(
            run(&["src-tauri/tests/api.rs"]).as_deref(),
            Some("cargo test --manifest-path src-tauri/Cargo.toml --test api")
        );
        assert_eq!(
            run(&["src-tauri/src/a/x/tests.rs", "src-tauri/src/a/y/tests.rs"]).as_deref(),
            Some("cargo test --manifest-path src-tauri/Cargo.toml a")
        );
        // Mixed stacks, or no runner known: the whole test check.
        assert_eq!(
            run(&["src/a.test.ts", "tests/test_x.py"]).as_deref(),
            Some("npm test")
        );
        assert_eq!(run(&[]).as_deref(), Some("npm test"));
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        assert_eq!(run(&["src/a.test.ts"]).as_deref(), Some("npm test"));
        assert_eq!(
            test_command_for(&dir, &["src/a.test.ts".into()], None),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_project_section() {
        let detected = |kind: &str, command: &str| DetectedCheck {
            kind: kind.into(),
            command: command.into(),
            source: "".into(),
        };
        let profile = ProjectProfile {
            stack: vec!["Rust".into()],
            checks: vec![
                detected("test", "cargo test"),
                detected("typecheck", "cargo check"),
                detected("build", "cargo build"),
            ],
            instructions: Some(ProjectInstructions {
                file: "AGENTS.md".into(),
                text: "Be nice.".into(),
                truncated: false,
            }),
            ..Default::default()
        };
        // Detected, and the user turned none on: each is off, and the agent
        // isn't told of them.
        let detected_only = effective_checks(&profile, &serde_json::Value::Null);
        assert_eq!(detected_only.len(), 3);
        assert!(
            detected_only.iter().all(|c| !c.enabled),
            "{detected_only:?}"
        );
        assert_eq!(
            project_context(&profile, None, &detected_only, true),
            "## Project\n\nStack: Rust.\n\nThe project's instructions for agents (AGENTS.md); follow them:\n\nBe nice."
        );

        // The user turned the tests and the type check on.
        let turned_on = serde_json::json!({ "checks": [
            { "kind": "test", "command": "cargo test", "enabled": true },
            { "kind": "typecheck", "command": "cargo check", "enabled": true },
            { "kind": "build", "command": "cargo build", "enabled": false },
        ] });
        let checks = effective_checks(&profile, &turned_on);
        let text = project_context(&profile, None, &checks, true);
        assert!(text.starts_with("## Project\n\nStack: Rust.\n\nChecks: when you finish"));
        assert!(text.contains("- Type check: `cargo check`\n- Tests: `cargo test`"));
        assert!(!text.contains("cargo build"));
        assert!(text.contains("Do not add a verification step"), "{text}");
        assert!(
            text.contains("Keep the checklist to the work itself"),
            "{text}"
        );
        // Verification off: checks are offered as commands, no no-verify-step line.
        let off = project_context(&profile, None, &checks, false);
        assert!(
            off.contains("Checks you can run with bash after changing code:"),
            "{off}"
        );
        assert!(!off.contains("Do not add a verification step"), "{off}");
        assert!(text.ends_with(
            "The project's instructions for agents (AGENTS.md); follow them:\n\nBe nice."
        ));

        let saved = serde_json::json!({ "checks": [{ "kind": "lint", "command": "x", "enabled": false }, { "kind": "nope", "command": "y" }] });
        assert_eq!(
            effective_checks(&profile, &saved),
            vec![ProjectCheck {
                kind: "lint".into(),
                command: "x".into(),
                enabled: false
            }]
        );
        assert_eq!(
            project_context(&ProjectProfile::default(), None, &[], true),
            ""
        );
        // The services' line comes after the stack, before the checks.
        let profile = ProjectProfile {
            stack: vec!["TypeScript".into()],
            ..Default::default()
        };
        assert_eq!(
            project_context(&profile, Some("Services (compose.yaml): `db`."), &[], true),
            "## Project\n\nStack: TypeScript.\n\nServices (compose.yaml): `db`."
        );
    }

    #[test]
    fn the_light_checks_offered_are_light() {
        // F05b's commands pass the light-command limit (classify: Light).
        for cmd in [
            "ruff check src/a.py",
            "pyright src/a.py",
            "go vet ./pkg",
            "npx eslint src/a.ts",
        ] {
            assert_eq!(
                crate::workload::classify::classify(cmd, None),
                crate::workload::classify::Class::Light,
                "{cmd}"
            );
        }
    }

    /// V09: an Autopilot task's checks include the stacks one folder
    /// down, each in its folder, and the prompt names the stacks. Detected,
    /// those checks are off like the project's own, so the agent isn't told
    /// of them. Anything else's, and a project whose checks the user saved,
    /// stay as they were.
    #[test]
    fn an_autopilot_task_finds_the_stacks_one_folder_down_with_their_checks_off() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.join("project");
        let config = dir.0.join("config");
        for (path, content) in [
            ("api/go.mod", "module example.com/api\n"),
            ("web/package.json", r#"{"scripts":{"test":"vitest run"}}"#),
        ] {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let r = root.to_string_lossy().into_owned();
        let (checks, context) = load_for(&config, &r, &r, true, true);
        let commands: Vec<&str> = checks.iter().map(|c| c.command.as_str()).collect();
        assert_eq!(
            commands,
            [
                "cd api && go build ./...",
                "cd api && go vet ./...",
                "cd api && go test ./...",
                "cd web && npm test",
            ]
        );
        assert!(checks.iter().all(|c| !c.enabled), "{checks:?}");
        assert!(
            context.contains("Go (api/)") && context.contains("JavaScript (web/)"),
            "{context}"
        );
        assert!(!context.contains("cd web && npm test"), "{context}");
        // Not an Autopilot task: as before.
        assert!(load_for(&config, &r, &r, true, false).0.is_empty());
        assert!(load(&config, &r, &r, true).0.is_empty());
        // The user's own checks win.
        std::fs::create_dir_all(&config).unwrap();
        project_profile::save_project_settings(
            &config,
            r.clone(),
            serde_json::json!({ "checks": [{ "kind": "test", "command": "make test", "enabled": true }] }),
        )
        .unwrap();
        let (saved, _) = load_for(&config, &r, &r, true, true);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].command, "make test");
    }

    #[test]
    fn a_checks_folder_is_read_back() {
        assert_eq!(in_folder("api", "go test ./..."), "cd api && go test ./...");
        assert_eq!(folder_of("cd api && go test ./..."), "api");
        assert_eq!(folder_of(&in_folder("my app", "npm test")), "my app");
        assert_eq!(in_folder("my app", "npm test"), "cd 'my app' && npm test");
        assert_eq!(folder_of("npm test"), "");
        assert_eq!(folder_of("cd api"), "");
    }

    #[test]
    fn instructions_share_their_room_evenly_and_a_short_one_leaves_its_rest_to_the_others() {
        assert_eq!(shares(&[20_000, 20_000], 12_000), [6_000, 6_000]);
        assert_eq!(
            shares(&[20_000, 1_000, 20_000], 12_000),
            [5_500, 1_000, 5_500]
        );
        assert_eq!(shares(&[100, 200], 12_000), [100, 200], "room to spare");
        assert!(shares(&[], 12_000).is_empty());
    }

    fn named(list: &[(&str, &Path)]) -> Folders {
        let list = list
            .iter()
            .map(|(name, path)| crate::folders::Folder {
                name: name.to_string(),
                path: path.to_path_buf(),
            })
            .collect();
        Folders::named(list).unwrap()
    }

    /// 4.4: each of a workspace chat's folders is read as when it's open
    /// alone, by its own settings, and said under its name: its stack, its
    /// services, its checks run in it, its own folders one level down for an
    /// Autopilot task (V09), and whether git works in it. Its Compose is a
    /// shell script.
    #[cfg(unix)]
    #[test]
    fn a_workspace_chats_folders_are_each_read_as_when_open_alone_under_their_names() {
        use std::os::unix::fs::PermissionsExt;
        let _services = crate::services::test_lock();
        let dir = crate::testing::temp_dir();
        let config = dir.0.join("config");
        for (path, content) in [
            ("api/go.mod", "module example.com/api\n"),
            ("api/compose.yaml", "services: {}\n"),
            (
                "api/worker/package.json",
                r#"{"scripts":{"test":"vitest run"}}"#,
            ),
            (
                "outer/web/package.json",
                r#"{"scripts":{"test":"vitest run"}}"#,
            ),
            ("docs/README.md", "Docs.\n"),
        ] {
            let p = dir.0.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        git2::Repository::init(dir.0.join("api")).unwrap();
        git2::Repository::init(dir.0.join("outer")).unwrap();
        let compose = dir.0.join("fake-compose");
        let json = r#"{"name": "api", "services": {"db": {"image": "postgres:16-alpine", "ports": [{"target": 5432, "published": "5432", "protocol": "tcp"}]}}}"#;
        std::fs::write(&compose, format!("#!/bin/sh\ncat <<'JSON'\n{json}\nJSON\n")).unwrap();
        std::fs::set_permissions(&compose, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::services::compose::use_program(Some(crate::services::compose::Program::Standalone(
            compose,
        )));
        let web = dir.0.join("outer/web");
        std::fs::create_dir_all(&config).unwrap();
        project_profile::save_project_settings(
            &config,
            web.to_string_lossy().into_owned(),
            serde_json::json!({ "checks": [{ "kind": "test", "command": "npm test", "enabled": true }] }),
        )
        .unwrap();
        let folders = named(&[
            ("api", &dir.0.join("api")),
            ("web", &web),
            ("docs", &dir.0.join("docs")),
        ]);

        let (checks, context, _) = load_folders(&config, None, &folders, true, true, None);
        let commands: Vec<&str> = checks.iter().map(|c| c.command.as_str()).collect();
        assert!(
            commands.contains(&"cd api && go test ./..."),
            "{commands:?}"
        );
        assert!(
            commands.contains(&"cd api/worker && npm test"),
            "{commands:?}"
        );
        assert!(commands.contains(&"cd web && npm test"), "{commands:?}");
        for said in [
            "Go (api/)",
            "JavaScript (api/worker/)",
            "JavaScript (web/)",
            "- Tests: `cd web && npm test`",
            "api: Services (compose.yaml): `db`",
            "`api` is a repository (`git -C api status`)",
            "`web` is a folder of a repository whose root isn't in the view",
            "`docs` isn't in a repository",
            "every path starts with one (`api/src/main.rs`)",
        ] {
            assert!(context.contains(said), "{said}: {context}");
        }
        // Its own V09 checks are off, as the folder's are alone.
        assert!(!context.contains("cd api && go test"), "{context}");
    }

    #[test]
    fn the_folders_that_joined_and_left_since_the_last_run_are_named() {
        let folders = named(&[("api", Path::new("/w/api")), ("docs", Path::new("/w/docs"))]);
        assert_eq!(joined_and_left(&folders, None), None, "its first run");
        let same = ["api".to_string(), "docs".to_string()];
        assert_eq!(joined_and_left(&folders, Some(&same)), None);
        let before = ["api".to_string(), "web".to_string()];
        let line = joined_and_left(&folders, Some(&before)).unwrap();
        assert!(
            line.contains("`docs` joined") && line.contains("`web` left"),
            "{line}"
        );
    }
}
