//! What the agent is told about the open project, and which commands check
//! its changes. Detection is in project_profile.rs; the user can correct the
//! detected checks per project (stored with `save_project_settings`), and
//! those win once saved.

use crate::project_profile::{self, ProjectProfile};
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

/// The checks in effect: the user's list when they saved one, else what's detected.
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
            enabled: c.default_enabled,
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

/// The "## Project" section of the system prompt: stack, the check commands
/// (and that the IDE runs them when verification is on), and the project's
/// own agent instructions.
pub fn project_context(
    profile: &ProjectProfile,
    checks: &[ProjectCheck],
    verify_on: bool,
) -> String {
    let mut parts = vec!["## Project".to_string()];
    if !profile.stack.is_empty() {
        parts.push(format!("Stack: {}.", profile.stack.join(", ")));
    }

    let enabled: Vec<ProjectCheck> = ordered_checks(checks)
        .into_iter()
        .filter(|c| c.enabled)
        .collect();
    if !enabled.is_empty() {
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
    }

    if let Some(notes) = &profile.instructions {
        let label = if notes.file.to_lowercase().starts_with("readme") {
            format!("From the project's {} (beginning):", notes.file)
        } else {
            format!(
                "The project's instructions for agents ({}); follow them:",
                notes.file
            )
        };
        let more = if notes.truncated {
            "\n[…truncated]"
        } else {
            ""
        };
        parts.push(format!("{label}\n\n{}{more}", notes.text));
    }
    if parts.len() > 1 {
        parts.join("\n\n")
    } else {
        String::new()
    }
}

/// Detected fresh (a few file reads): the project's checks and its prompt section.
/// The project's checks and its section of the system prompt. `root` is
/// where the run works (an isolated chat's view); `settings_root`, the
/// project folder its saved settings are kept under.
pub fn load<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    root: &str,
    settings_root: &str,
    verify_on: bool,
) -> (Vec<ProjectCheck>, String) {
    if !Path::new(root).is_dir() {
        return (Vec::new(), String::new());
    }
    let profile = project_profile::detect(Path::new(root));
    let settings = project_profile::load_project_settings(app.clone(), settings_root.to_string())
        .unwrap_or_default();
    let checks = effective_checks(&profile, &settings);
    let context = project_context(&profile, &checks, verify_on);
    (checks, context)
}

/// The project section for estimates (the context meter), re-detected at
/// most every few seconds: estimates follow every message of a run.
pub fn cached_context<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    root: &str,
    verify_on: bool,
) -> String {
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
    let (_, text) = load(app, root, root, verify_on);
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
    let module = module
        .trim_end_matches("/mod")
        .trim_start_matches("lib")
        .trim_start_matches("main")
        .trim_start_matches('/');
    (!module.is_empty()).then(|| module.replace('/', "::"))
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
        let profile = ProjectProfile {
            stack: vec!["Rust".into()],
            checks: vec![
                DetectedCheck {
                    kind: "test".into(),
                    command: "cargo test".into(),
                    source: "".into(),
                    default_enabled: true,
                },
                DetectedCheck {
                    kind: "typecheck".into(),
                    command: "cargo check".into(),
                    source: "".into(),
                    default_enabled: true,
                },
                DetectedCheck {
                    kind: "build".into(),
                    command: "cargo build".into(),
                    source: "".into(),
                    default_enabled: false,
                },
            ],
            instructions: Some(ProjectInstructions {
                file: "AGENTS.md".into(),
                text: "Be nice.".into(),
                truncated: false,
            }),
            ..Default::default()
        };
        let checks = effective_checks(&profile, &serde_json::Value::Null);
        let text = project_context(&profile, &checks, true);
        assert!(text.starts_with("## Project\n\nStack: Rust.\n\nChecks: when you finish"));
        assert!(text.contains("- Type check: `cargo check`\n- Tests: `cargo test`"));
        assert!(!text.contains("cargo build"));
        assert!(text.contains("Do not add a verification step"), "{text}");
        assert!(
            text.contains("Keep the checklist to the work itself"),
            "{text}"
        );
        // Verification off: checks are offered as commands, no no-verify-step line.
        let off = project_context(&profile, &checks, false);
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
        assert_eq!(project_context(&ProjectProfile::default(), &[], true), "");
    }
}
