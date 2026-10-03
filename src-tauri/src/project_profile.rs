//! What the agent should know about the open project before it starts:
//! the stack, the commands that check a change (type check, lint, tests),
//! and the project's own instructions for agents (AGENTS.md, CLAUDE.md...).
//!
//! Detection reads a handful of manifest files, no model and no network, so
//! it runs at the start of every agent run instead of being cached. The
//! frontend puts the result in the system prompt and the verification gate
//! runs the checks when the agent says it's done (lib/projectProfile.ts).
//!
//! Also here: per-project settings (the user's corrections to the detected
//! checks) and the agent diagnostics log.

use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DetectedCheck {
    /// "typecheck", "lint", "test" or "build".
    pub kind: String,
    pub command: String,
    /// Where it came from, e.g. "package.json scripts.test".
    pub source: String,
    /// Detected but not run by default (builds are slow and rarely needed).
    pub default_enabled: bool,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInstructions {
    pub file: String,
    pub text: String,
    pub truncated: bool,
}

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProfile {
    /// e.g. ["TypeScript", "Solid", "Vite", "Tauri", "Rust"].
    pub stack: Vec<String>,
    pub checks: Vec<DetectedCheck>,
    pub instructions: Option<ProjectInstructions>,
}

/// Agent instruction files, first found wins. README is the fallback.
const INSTRUCTION_FILES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    ".github/copilot-instructions.md",
    ".cursorrules",
];
const INSTRUCTIONS_MAX_CHARS: usize = 6_000;
const README_MAX_CHARS: usize = 1_500;

pub fn detect(root: &Path) -> ProjectProfile {
    let mut profile = ProjectProfile::default();
    detect_node(root, &mut profile);
    detect_rust(root, &mut profile);
    detect_python(root, &mut profile);
    detect_go(root, &mut profile);
    detect_make(root, &mut profile);
    profile.instructions = read_instructions(root);
    profile
}

fn push_check(profile: &mut ProjectProfile, kind: &str, command: String, source: String) {
    if profile.checks.iter().any(|c| c.command == command) {
        return;
    }
    profile.checks.push(DetectedCheck {
        kind: kind.into(),
        default_enabled: kind != "build",
        command,
        source,
    });
}

fn push_stack(profile: &mut ProjectProfile, name: &str) {
    if !profile.stack.iter().any(|s| s == name) {
        profile.stack.push(name.into());
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/* ---------------------------------------------------------------------------
 * JavaScript / TypeScript
 * ------------------------------------------------------------------------- */

fn detect_node(root: &Path, profile: &mut ProjectProfile) {
    let Some(pkg) = read_json(&root.join("package.json")) else { return };

    let pm = if root.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    };
    let run = |script: &str| match (pm, script) {
        ("npm", "test") => "npm test".to_string(),
        (pm, script) => format!("{pm} run {script}"),
    };
    let exec = |bin: &str| match pm {
        "pnpm" => format!("pnpm exec {bin}"),
        "yarn" => format!("yarn {bin}"),
        "bun" => format!("bunx {bin}"),
        _ => format!("npx {bin}"),
    };

    let has_dep = |name: &str| {
        ["dependencies", "devDependencies", "peerDependencies"]
            .iter()
            .any(|section| pkg.get(section).and_then(|d| d.get(name)).is_some())
    };
    let typescript = root.join("tsconfig.json").exists() || has_dep("typescript");
    push_stack(profile, if typescript { "TypeScript" } else { "JavaScript" });
    for (dep, name) in [
        ("react", "React"),
        ("solid-js", "Solid"),
        ("vue", "Vue"),
        ("svelte", "Svelte"),
        ("next", "Next.js"),
        ("vite", "Vite"),
        ("@tauri-apps/api", "Tauri"),
        ("express", "Express"),
    ] {
        if has_dep(dep) {
            push_stack(profile, name);
        }
    }

    let scripts = pkg.get("scripts").and_then(|s| s.as_object());
    let script = |names: &[&str]| -> Option<String> {
        let scripts = scripts?;
        names
            .iter()
            .find(|n| scripts.get(**n).and_then(|v| v.as_str()).is_some_and(|v| !v.trim().is_empty()))
            .map(|n| n.to_string())
    };

    match script(&["typecheck", "type-check", "check-types", "types", "tsc"]) {
        Some(name) => push_check(profile, "typecheck", run(&name), format!("package.json scripts.{name}")),
        None if root.join("tsconfig.json").exists() => push_check(
            profile,
            "typecheck",
            exec("tsc --noEmit"),
            "tsconfig.json".into(),
        ),
        None => {}
    }
    if let Some(name) = script(&["lint"]) {
        push_check(profile, "lint", run(&name), format!("package.json scripts.{name}"));
    }
    if let Some(name) = script(&["test"]) {
        let body = scripts
            .and_then(|s| s.get(&name))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        // npm init's placeholder fails on purpose.
        if !body.contains("no test specified") {
            push_check(profile, "test", run(&name), format!("package.json scripts.{name}"));
        }
    }
    if let Some(name) = script(&["build"]) {
        push_check(profile, "build", run(&name), format!("package.json scripts.{name}"));
    }
}

/* ---------------------------------------------------------------------------
 * Rust: a Cargo.toml in the root or one folder down (e.g. a Tauri app's
 * src-tauri/)
 * ------------------------------------------------------------------------- */

fn detect_rust(root: &Path, profile: &mut ProjectProfile) {
    let mut manifests: Vec<String> = Vec::new();
    if root.join("Cargo.toml").is_file() {
        manifests.push(String::new());
    } else if let Ok(entries) = fs::read_dir(root) {
        let mut dirs: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().join("Cargo.toml").is_file())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|name| !name.starts_with('.') && name != "target" && name != "node_modules")
            .collect();
        dirs.sort();
        manifests.extend(dirs.into_iter().take(2));
    }
    if manifests.is_empty() {
        return;
    }
    push_stack(profile, "Rust");
    for dir in manifests {
        let (arg, source) = if dir.is_empty() {
            (String::new(), "Cargo.toml".to_string())
        } else {
            (format!(" --manifest-path {dir}/Cargo.toml"), format!("{dir}/Cargo.toml"))
        };
        push_check(profile, "typecheck", format!("cargo check{arg}"), source.clone());
        push_check(profile, "lint", format!("cargo clippy{arg}"), source.clone());
        push_check(profile, "test", format!("cargo test{arg}"), source);
    }
}

/* ---------------------------------------------------------------------------
 * Python
 * ------------------------------------------------------------------------- */

fn detect_python(root: &Path, profile: &mut ProjectProfile) {
    let pyproject = fs::read_to_string(root.join("pyproject.toml")).unwrap_or_default();
    let python = !pyproject.is_empty()
        || root.join("setup.py").exists()
        || root.join("requirements.txt").exists();
    if !python {
        return;
    }
    push_stack(profile, "Python");
    let has = |section: &str| pyproject.contains(&format!("[{section}"));

    if has("tool.mypy") || root.join("mypy.ini").exists() {
        push_check(profile, "typecheck", "mypy .".into(), "mypy config".into());
    } else if root.join("pyrightconfig.json").exists() || has("tool.pyright") {
        push_check(profile, "typecheck", "pyright".into(), "pyright config".into());
    }
    if has("tool.ruff") || root.join("ruff.toml").exists() || root.join(".ruff.toml").exists() {
        push_check(profile, "lint", "ruff check .".into(), "ruff config".into());
    }
    if has("tool.pytest")
        || root.join("pytest.ini").exists()
        || root.join("conftest.py").exists()
        || root.join("tests").is_dir()
    {
        push_check(profile, "test", "pytest".into(), "pytest config".into());
    }
}

/* ---------------------------------------------------------------------------
 * Go
 * ------------------------------------------------------------------------- */

fn detect_go(root: &Path, profile: &mut ProjectProfile) {
    if !root.join("go.mod").is_file() {
        return;
    }
    push_stack(profile, "Go");
    push_check(profile, "typecheck", "go build ./...".into(), "go.mod".into());
    push_check(profile, "lint", "go vet ./...".into(), "go.mod".into());
    push_check(profile, "test", "go test ./...".into(), "go.mod".into());
}

/* ---------------------------------------------------------------------------
 * Makefile: only kinds nothing else covered
 * ------------------------------------------------------------------------- */

fn detect_make(root: &Path, profile: &mut ProjectProfile) {
    let Ok(makefile) = fs::read_to_string(root.join("Makefile")) else { return };
    let has_target = |t: &str| {
        makefile
            .lines()
            .any(|l| l.starts_with(t) && l[t.len()..].trim_start().starts_with(':'))
    };
    for (target, kind) in [("typecheck", "typecheck"), ("lint", "lint"), ("test", "test")] {
        if has_target(target) && !profile.checks.iter().any(|c| c.kind == kind) {
            push_check(profile, kind, format!("make {target}"), "Makefile".into());
        }
    }
}

/* ---------------------------------------------------------------------------
 * Instructions
 * ------------------------------------------------------------------------- */

fn read_instructions(root: &Path) -> Option<ProjectInstructions> {
    let found = INSTRUCTION_FILES
        .iter()
        .find_map(|f| {
            fs::read_to_string(root.join(f))
                .ok()
                .map(|t| (f.to_string(), t, INSTRUCTIONS_MAX_CHARS))
        })
        .or_else(|| {
            ["README.md", "README", "readme.md"].iter().find_map(|f| {
                fs::read_to_string(root.join(f))
                    .ok()
                    .map(|t| (f.to_string(), t, README_MAX_CHARS))
            })
        })?;
    let (file, text, max) = found;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let truncated = text.chars().count() > max;
    let text = if truncated {
        let cut: String = text.chars().take(max).collect();
        // End on a line break, so a sentence or code line isn't cut in half.
        match cut.rfind('\n') {
            Some(i) if i > max / 2 => cut[..i].to_string(),
            _ => cut,
        }
    } else {
        text.to_string()
    };
    Some(ProjectInstructions { file, text, truncated })
}

// ---- tauri commands (not compiled into the standalone tests) ----

#[tauri::command]
pub fn project_profile(root: String) -> Result<ProjectProfile, String> {
    let root = Path::new(&root);
    if !root.is_dir() {
        return Err(format!("{}: not a folder", root.display()));
    }
    Ok(detect(root))
}

fn project_settings_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("project-settings.json"))
}

/// The user's settings for one project (e.g. corrected check commands), as
/// the JSON the frontend stored; null when there are none.
#[tauri::command]
pub fn load_project_settings(app: tauri::AppHandle, root: String) -> Result<serde_json::Value, String> {
    let path = project_settings_path(&app)?;
    let all: serde_json::Value = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(all.get(&root).cloned().unwrap_or(serde_json::Value::Null))
}

/// Saves one project's settings; null removes them.
#[tauri::command]
pub fn save_project_settings(
    app: tauri::AppHandle,
    root: String,
    settings: serde_json::Value,
) -> Result<(), String> {
    let path = project_settings_path(&app)?;
    let mut all: serde_json::Map<String, serde_json::Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if settings.is_null() {
        all.remove(&root);
    } else {
        all.insert(root, settings);
    }
    let json = serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?;
    crate::atomic_file::write_atomic(&path, json.as_bytes()).map_err(|e| e.to_string())
}

/// Agent diagnostics: one JSON object per line in agent-log.jsonl in the
/// app's data folder. Rotated to agent-log.1.jsonl at 5 MB.
#[tauri::command]
pub fn append_agent_log(app: tauri::AppHandle, line: String) -> Result<(), String> {
    use std::io::Write;
    use tauri::Manager;
    const MAX_BYTES: u64 = 5 * 1024 * 1024;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("agent-log.jsonl");
    if fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = fs::rename(&path, dir.join("agent-log.1.jsonl"));
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{}", line.replace('\n', " ")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "profile-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        for (path, content) in files {
            let p = dir.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        dir
    }

    fn commands(p: &ProjectProfile) -> Vec<String> {
        p.checks.iter().map(|c| format!("{}: {}", c.kind, c.command)).collect()
    }

    #[test]
    fn tauri_app_like_this_one() {
        let dir = project(&[
            (
                "package.json",
                r#"{"scripts":{"dev":"vite","build":"vite build"},"dependencies":{"solid-js":"1","@tauri-apps/api":"2"},"devDependencies":{"typescript":"5","vite":"8"}}"#,
            ),
            ("tsconfig.json", "{}"),
            ("package-lock.json", "{}"),
            ("src-tauri/Cargo.toml", "[package]"),
            ("README.md", "# tauri-ide\n\nA minimal IDE."),
        ]);
        let p = detect(&dir);
        assert_eq!(p.stack, ["TypeScript", "Solid", "Vite", "Tauri", "Rust"]);
        assert_eq!(
            commands(&p),
            [
                "typecheck: npx tsc --noEmit",
                "build: npm run build",
                "typecheck: cargo check --manifest-path src-tauri/Cargo.toml",
                "lint: cargo clippy --manifest-path src-tauri/Cargo.toml",
                "test: cargo test --manifest-path src-tauri/Cargo.toml",
            ]
        );
        assert!(!p.checks[1].default_enabled, "builds are off by default");
        let notes = p.instructions.unwrap();
        assert_eq!(notes.file, "README.md");
    }

    #[test]
    fn scripts_and_package_manager() {
        let dir = project(&[
            (
                "package.json",
                r#"{"scripts":{"typecheck":"tsc -b","lint":"eslint .","test":"vitest run"}}"#,
            ),
            ("pnpm-lock.yaml", ""),
        ]);
        assert_eq!(
            commands(&detect(&dir)),
            [
                "typecheck: pnpm run typecheck",
                "lint: pnpm run lint",
                "test: pnpm run test"
            ]
        );
    }

    #[test]
    fn npm_placeholder_test_is_ignored() {
        let dir = project(&[(
            "package.json",
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
        )]);
        assert!(detect(&dir).checks.is_empty());
    }

    #[test]
    fn python_go_and_make() {
        let py = project(&[
            ("pyproject.toml", "[tool.ruff]\nline-length = 100\n[tool.pytest.ini_options]\n"),
            ("mypy.ini", ""),
        ]);
        assert_eq!(
            commands(&detect(&py)),
            ["typecheck: mypy .", "lint: ruff check .", "test: pytest"]
        );
        let go = project(&[("go.mod", "module x")]);
        assert_eq!(detect(&go).checks.len(), 3);
        let make = project(&[("Makefile", "build:\n\tcc x.c\ntest: build\n\t./run\n")]);
        assert_eq!(commands(&detect(&make)), ["test: make test"]);
    }

    #[test]
    fn agent_instructions_win_over_readme_and_are_cut_on_a_line() {
        let long = "rule\n".repeat(3_000);
        let dir = project(&[("README.md", "readme"), ("AGENTS.md", &long)]);
        let notes = detect(&dir).instructions.unwrap();
        assert_eq!(notes.file, "AGENTS.md");
        assert!(notes.truncated);
        assert!(notes.text.len() <= INSTRUCTIONS_MAX_CHARS && notes.text.ends_with("rule"));
    }

    #[test]
    fn empty_folder() {
        let p = detect(&project(&[]));
        assert!(p.stack.is_empty() && p.checks.is_empty() && p.instructions.is_none());
    }
}
