//! What the agent should know about the open project before it starts:
//! the stack, the commands that check a change (type check, lint, tests),
//! and the project's own instructions for agents (AGENTS.md, CLAUDE.md...).
//!
//! Detection reads a handful of manifest files, no model and no network, so
//! it runs at the start of every agent run instead of being cached. The
//! agent puts the result in its system prompt and the verification gate runs
//! the checks when the agent says it's done (agent_run/project.rs, verify.rs).
//!
//! Also here: per-project settings (the user's corrections to the detected
//! checks) and the agent diagnostics log.

use serde::Serialize;
use std::fs;
use std::path::Path;

mod workspaces;

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

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMember {
    /// Project-relative dir, forward slashes, e.g. "packages/web".
    pub path: String,
    /// package.json `name` / Cargo `[package] name`, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// "node" or "cargo".
    pub kind: String,
    /// Node script names (sorted); empty for cargo (cargo check/test cover it).
    #[serde(default)]
    pub scripts: Vec<String>,
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
    /// Workspace member packages, capped (see workspaces.rs). Empty when the
    /// project is not a workspace. Powers the "Project checks" suggestions:
    /// first open pre-fills from these; saved settings win after that.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceMember>,
    /// Node package manager that owns the workspace commands ("npm" |
    /// "pnpm" | "yarn" | "yarn-berry" | "bun"). Absent when the project has
    /// no package.json. Lets the settings UI build "add script" commands
    /// without duplicating manager detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<String>,
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
    profile.workspaces.sort_by(|a, b| a.path.cmp(&b.path));
    profile.workspaces.dedup_by(|a, b| a.path == b.path);
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

/// The `tsc` run that type-checks the project, and the config it came from.
///
/// tsconfig.json isn't always the config that holds the code: a "solution"
/// tsconfig.json (`"files": []` + `references`, as Vite's templates write)
/// covers nothing itself, so `tsc --noEmit` on it checks nothing and passes.
/// Each referenced config is checked instead (`tsc -b` would emit their
/// outputs). With no tsconfig.json at all, the root's tsconfig.*.json are.
fn typecheck_command(root: &Path, exec: &dyn Fn(&str) -> String) -> Option<(String, String)> {
    let check = |configs: &[String]| {
        configs
            .iter()
            .map(|c| exec(&format!("tsc --noEmit -p {c}")))
            .collect::<Vec<_>>()
            .join(" && ")
    };
    if root.join("tsconfig.json").is_file() {
        let config = read_jsonc(&root.join("tsconfig.json")).unwrap_or_default();
        let refs = tsconfig_references(root, &config);
        if refs.is_empty() {
            return Some((exec("tsc --noEmit"), "tsconfig.json".into()));
        }
        let mut configs = refs;
        if !is_solution_config(&config) {
            configs.insert(0, "tsconfig.json".into());
        }
        return Some((check(&configs), "tsconfig.json references".into()));
    }
    let mut configs: Vec<String> = fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("tsconfig.") && n.ends_with(".json") && n.len() > 14)
        .collect();
    if configs.is_empty() {
        return None;
    }
    configs.sort();
    let source = configs.join(", ");
    Some((check(&configs), source))
}

/// Root-relative, forward-slash paths of the configs tsconfig.json
/// references that exist; a reference may name a folder (its tsconfig.json).
fn tsconfig_references(root: &Path, config: &serde_json::Value) -> Vec<String> {
    let Some(refs) = config.get("references").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for path in refs.iter().filter_map(|r| r.get("path")?.as_str()) {
        let rel = path.replace('\\', "/");
        let rel = rel.trim_start_matches("./").trim_end_matches('/');
        let file = if root.join(rel).is_dir() {
            format!("{rel}/tsconfig.json")
        } else {
            rel.to_string()
        };
        // A path with spaces or quotes would need shell quoting; skip it
        // rather than build a command that breaks.
        let safe = !file.is_empty()
            && file
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-/@+".contains(c));
        if safe && root.join(&file).is_file() && !out.contains(&file) {
            out.push(file);
        }
    }
    out
}

/// `"files": []` with no include (or `"include": []` with no files): the
/// config only groups its references.
fn is_solution_config(config: &serde_json::Value) -> bool {
    let empty = |key: &str| {
        config
            .get(key)
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.is_empty())
    };
    let absent_or_empty = |key: &str| config.get(key).is_none() || empty(key);
    (empty("files") && absent_or_empty("include")) || (empty("include") && absent_or_empty("files"))
}

/// tsconfig files allow comments and trailing commas; serde_json doesn't.
fn read_jsonc(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&strip_jsonc(&fs::read_to_string(path).ok()?)).ok()
}

fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == '/' && chars.peek() == Some(&'/') {
            for n in chars.by_ref() {
                if n == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut prev = ' ';
            for n in chars.by_ref() {
                if prev == '*' && n == '/' {
                    break;
                }
                prev = n;
            }
        } else if c == ',' {
            // Trailing comma: the next non-space char closes the container.
            let next = chars.clone().find(|n| !n.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn detect_node(root: &Path, profile: &mut ProjectProfile) {
    let Some(pkg) = read_json(&root.join("package.json")) else {
        return;
    };

    let pm = workspaces::detect_package_manager(root);
    let yarn_berry = workspaces::is_yarn_berry(root, Some(&pkg));
    profile.package_manager = Some(
        match (pm, yarn_berry) {
            ("yarn", true) => "yarn-berry",
            (pm, _) => pm,
        }
        .to_string(),
    );
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
    let ts_check = typecheck_command(root, &exec);
    let typescript = ts_check.is_some() || has_dep("typescript");
    push_stack(
        profile,
        if typescript {
            "TypeScript"
        } else {
            "JavaScript"
        },
    );
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
            .find(|n| {
                scripts
                    .get(**n)
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| !v.trim().is_empty())
            })
            .map(|n| n.to_string())
    };

    match script(&["typecheck", "type-check", "check-types", "types", "tsc"]) {
        Some(name) => push_check(
            profile,
            "typecheck",
            run(&name),
            format!("package.json scripts.{name}"),
        ),
        None => {
            if let Some((command, source)) = ts_check {
                push_check(profile, "typecheck", command, source);
            }
        }
    }
    if let Some(name) = script(&["lint"]) {
        push_check(
            profile,
            "lint",
            run(&name),
            format!("package.json scripts.{name}"),
        );
    }
    if let Some(name) = script(&["test"]) {
        let body = scripts
            .and_then(|s| s.get(&name))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        // npm init's placeholder fails on purpose.
        if !body.contains("no test specified") {
            push_check(
                profile,
                "test",
                run(&name),
                format!("package.json scripts.{name}"),
            );
        }
    }
    if let Some(name) = script(&["build"]) {
        push_check(
            profile,
            "build",
            run(&name),
            format!("package.json scripts.{name}"),
        );
    }

    // Workspaces: when the root declares member packages, scan their manifests
    // (capped) and fill kinds the root doesn't cover with one workspace-wide
    // command per kind. Sources name the member count so the UI can explain
    // where the suggestion came from.
    let member_dirs = workspaces::node_members(root, Some(&pkg));
    if member_dirs.is_empty() {
        return;
    }
    // Record members for the settings UI (name + script names for pickers).
    let mut member_scripts: Vec<Vec<String>> = Vec::new();
    for rel in &member_dirs {
        if let Some((name, scripts)) = workspaces::read_node_member(root, rel) {
            member_scripts.push(scripts.clone());
            profile.workspaces.push(WorkspaceMember {
                path: rel.clone(),
                name,
                kind: "node".to_string(),
                scripts,
            });
        }
    }
    if member_scripts.is_empty() {
        return;
    }
    push_stack(profile, "Workspaces");

    // For each kind, find the most common script name for it across members.
    let candidates: &[(&str, &[&str])] = &[
        (
            "typecheck",
            &["typecheck", "type-check", "check-types", "types", "tsc"],
        ),
        ("lint", &["lint"]),
        ("test", &["test"]),
        ("build", &["build"]),
    ];
    for (kind, names) in candidates {
        if profile.checks.iter().any(|c| c.kind == *kind) {
            continue;
        }
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for scripts in &member_scripts {
            if let Some(found) = names.iter().find(|n| scripts.iter().any(|s| s == **n)) {
                *counts.entry(found).or_default() += 1;
            }
        }
        if counts.is_empty() {
            continue;
        }
        let (best, n) = counts.into_iter().max_by_key(|(_, c)| *c).unwrap();
        // Skip npm's placeholder test scripts.
        if *kind == "test" {
            let mut real = 0;
            for rel in &member_dirs {
                let body = read_json(&root.join(rel).join("package.json"))
                    .and_then(|p| p.get("scripts")?.get(best)?.as_str().map(str::to_string))
                    .unwrap_or_default();
                if !body.contains("no test specified") {
                    real += 1;
                }
            }
            if real == 0 {
                continue;
            }
        }
        let command = workspaces::workspace_run(pm, best, yarn_berry);
        let source = if member_dirs.len() == 1 {
            format!("{}/package.json scripts.{best}", member_dirs[0])
        } else {
            format!("workspaces ({n} packages): scripts.{best}")
        };
        push_check(profile, kind, command, source);
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
        let manifest_path = if dir.is_empty() {
            root.join("Cargo.toml")
        } else {
            root.join(&dir).join("Cargo.toml")
        };
        let content = fs::read_to_string(&manifest_path).unwrap_or_default();
        let is_workspace =
            workspaces::toml_has_section(&content, "workspace") || content.contains("[workspace]");
        let members = workspaces::cargo_members(root, &dir, &content);
        for rel in &members {
            let name = fs::read_to_string(root.join(rel).join("Cargo.toml"))
                .ok()
                .and_then(|t| workspaces::cargo_package_name(&t));
            if !profile.workspaces.iter().any(|w| w.path == *rel) {
                profile.workspaces.push(WorkspaceMember {
                    path: rel.clone(),
                    name,
                    kind: "cargo".to_string(),
                    scripts: Vec::new(),
                });
            }
        }
        if !members.is_empty() {
            push_stack(profile, "Workspaces");
        }
        let (arg, mut source) = if dir.is_empty() {
            (String::new(), "Cargo.toml".to_string())
        } else {
            (
                format!(" --manifest-path {dir}/Cargo.toml"),
                format!("{dir}/Cargo.toml"),
            )
        };
        if is_workspace {
            // `cargo check` at the workspace root already covers all members.
            source.push_str(" [workspace]");
        }
        push_check(
            profile,
            "typecheck",
            format!("cargo check{arg}"),
            source.clone(),
        );
        push_check(
            profile,
            "lint",
            format!("cargo clippy{arg}"),
            source.clone(),
        );
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
        push_check(
            profile,
            "typecheck",
            "pyright".into(),
            "pyright config".into(),
        );
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
    push_check(
        profile,
        "typecheck",
        "go build ./...".into(),
        "go.mod".into(),
    );
    push_check(profile, "lint", "go vet ./...".into(), "go.mod".into());
    push_check(profile, "test", "go test ./...".into(), "go.mod".into());
}

/* ---------------------------------------------------------------------------
 * Makefile: only kinds nothing else covered
 * ------------------------------------------------------------------------- */

fn detect_make(root: &Path, profile: &mut ProjectProfile) {
    let Ok(makefile) = fs::read_to_string(root.join("Makefile")) else {
        return;
    };
    let has_target = |t: &str| {
        makefile
            .lines()
            .any(|l| l.starts_with(t) && l[t.len()..].trim_start().starts_with(':'))
    };
    for (target, kind) in [
        ("typecheck", "typecheck"),
        ("lint", "lint"),
        ("test", "test"),
    ] {
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
    Some(ProjectInstructions {
        file,
        text,
        truncated,
    })
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

fn project_settings_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("project-settings.json"))
}

/// The user's settings for one project (e.g. corrected check commands), as
/// the JSON the frontend stored; null when there are none.
#[tauri::command]
pub fn load_project_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    root: String,
) -> Result<serde_json::Value, String> {
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
/// Where the agent log goes instead of the app's data folder (the headless
/// runner, headless.rs, keeps it with its report).
static AGENT_LOG_DIR: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

#[cfg(any(test, feature = "headless"))]
pub fn set_agent_log_dir(dir: Option<std::path::PathBuf>) {
    *AGENT_LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()) = dir;
}

pub fn append_agent_log<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    line: String,
) -> Result<(), String> {
    use tauri::Manager;
    let chosen = AGENT_LOG_DIR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let dir = match chosen {
        Some(dir) => dir,
        None => app.path().app_data_dir().map_err(|e| e.to_string())?,
    };
    append_log_line(&dir, &line, 5 * 1024 * 1024)
}

/// One line at the end of `dir`'s agent-log.jsonl, rotated past `max_bytes`.
/// P6.3: one writer at a time, and each line in one write, so lines from
/// runs side by side never interleave and rotation never splits one.
fn append_log_line(dir: &std::path::Path, line: &str, max_bytes: u64) -> Result<(), String> {
    use std::io::Write;
    static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join("agent-log.jsonl");
    let _one = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    if fs::metadata(&path)
        .map(|m| m.len() > max_bytes)
        .unwrap_or(false)
    {
        let _ = fs::rename(&path, dir.join("agent-log.1.jsonl"));
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut text = line.replace('\n', " ");
    text.push('\n');
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines_from_many_writers_stay_whole_across_rotations() {
        let dir = std::env::temp_dir().join(format!("agent-log-{}", uuid::Uuid::new_v4()));
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..50 {
                        let line =
                            format!("{{\"t\":{t},\"i\":{i},\"pad\":\"{}\"}}", "x".repeat(20_000));
                        append_log_line(&dir, &line, 200_000).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        for name in ["agent-log.jsonl", "agent-log.1.jsonl"] {
            let text = fs::read_to_string(dir.join(name)).unwrap();
            assert!(text.ends_with('\n'));
            for l in text.lines() {
                assert!(
                    serde_json::from_str::<serde_json::Value>(l).is_ok(),
                    "a torn line in {name}"
                );
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

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
        p.checks
            .iter()
            .map(|c| format!("{}: {}", c.kind, c.command))
            .collect()
    }

    #[test]
    fn solution_tsconfig_checks_each_reference() {
        let dir = project(&[
            ("package.json", r#"{"devDependencies":{"typescript":"5"}}"#),
            (
                "tsconfig.json",
                "{\n  // solution\n  \"files\": [],\n  \"references\": [\n    { \"path\": \"./tsconfig.app.json\" },\n    { \"path\": \"./tsconfig.node.json\" },\n    { \"path\": \"./missing.json\" },\n    { \"path\": \"packages/lib\" },\n  ],\n}",
            ),
            ("tsconfig.app.json", r#"{"include":["src"]}"#),
            ("tsconfig.node.json", r#"{"include":["vite.config.ts"]}"#),
            ("packages/lib/tsconfig.json", "{}"),
        ]);
        let p = detect(&dir);
        assert_eq!(p.stack, ["TypeScript"]);
        assert_eq!(
            commands(&p),
            ["typecheck: npx tsc --noEmit -p tsconfig.app.json && npx tsc --noEmit -p tsconfig.node.json && npx tsc --noEmit -p packages/lib/tsconfig.json"]
        );
        assert_eq!(p.checks[0].source, "tsconfig.json references");
    }

    #[test]
    fn tsconfig_with_own_files_and_references_checks_both() {
        let dir = project(&[
            ("package.json", "{}"),
            (
                "tsconfig.json",
                r#"{"include":["src"],"references":[{"path":"./tsconfig.node.json"}]}"#,
            ),
            ("tsconfig.node.json", "{}"),
        ]);
        assert_eq!(
            commands(&detect(&dir)),
            ["typecheck: npx tsc --noEmit -p tsconfig.json && npx tsc --noEmit -p tsconfig.node.json"]
        );
    }

    #[test]
    fn only_tsconfig_variants_without_tsconfig_json() {
        let dir = project(&[
            ("package.json", "{}"),
            ("tsconfig.node.json", "{}"),
            ("tsconfig.app.json", "{}"),
        ]);
        let p = detect(&dir);
        assert_eq!(p.stack, ["TypeScript"]);
        assert_eq!(
            commands(&p),
            ["typecheck: npx tsc --noEmit -p tsconfig.app.json && npx tsc --noEmit -p tsconfig.node.json"]
        );
    }

    #[test]
    fn jsonc_comments_and_trailing_commas() {
        let v: serde_json::Value =
            serde_json::from_str(&strip_jsonc("{ \"a\": \"//x,}\", /* c */ \"b\": [1, ], }"))
                .unwrap();
        assert_eq!(v, serde_json::json!({"a": "//x,}", "b": [1]}));
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
            ("README.md", "# rovik-ide\n\nA minimal IDE."),
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
            (
                "pyproject.toml",
                "[tool.ruff]\nline-length = 100\n[tool.pytest.ini_options]\n",
            ),
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
    fn npm_workspaces_prefill_missing_kinds() {
        let dir = project(&[
            (
                "package.json",
                r#"{"private":true,"workspaces":["packages/*"]}"#,
            ),
            (
                "packages/web/package.json",
                r#"{"name":"web","scripts":{"test":"vitest run","build":"vite build"}}"#,
            ),
            (
                "packages/api/package.json",
                r#"{"name":"api","scripts":{"test":"node --test","lint":"eslint ."}}"#,
            ),
        ]);
        let p = detect(&dir);
        assert!(p.stack.contains(&"Workspaces".to_string()));
        assert_eq!(
            p.workspaces
                .iter()
                .map(|w| w.path.as_str())
                .collect::<Vec<_>>(),
            ["packages/api", "packages/web"]
        );
        // Root had none of these: each kind gets one workspace-wide command.
        assert!(commands(&p).contains(&"test: npm test --workspaces --if-present".to_string()));
        assert!(commands(&p).contains(&"lint: npm run lint --workspaces --if-present".to_string()));
        assert!(
            commands(&p).contains(&"build: npm run build --workspaces --if-present".to_string())
        );
        let test = p.checks.iter().find(|c| c.kind == "test").unwrap();
        assert!(
            test.source.starts_with("workspaces (2 packages)"),
            "source: {}",
            test.source
        );
    }

    #[test]
    fn pnpm_workspace_uses_recursive_run() {
        let dir = project(&[
            ("package.json", r#"{"private":true}"#),
            (
                "pnpm-workspace.yaml",
                "packages:\n  - 'apps/*'\n  - '!apps/ignored'\n",
            ),
            ("pnpm-lock.yaml", ""),
            (
                "apps/a/package.json",
                r#"{"name":"a","scripts":{"test":"vitest run"}}"#,
            ),
            (
                "apps/ignored/package.json",
                r#"{"name":"ignored","scripts":{"test":"vitest run"}}"#,
            ),
        ]);
        let p = detect(&dir);
        assert_eq!(
            p.workspaces
                .iter()
                .map(|w| w.path.as_str())
                .collect::<Vec<_>>(),
            ["apps/a"]
        );
        assert_eq!(commands(&p), ["test: pnpm -r --if-present run test"]);
    }

    #[test]
    fn cargo_workspace_source_and_members() {
        let dir = project(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ]);
        let p = detect(&dir);
        assert!(p.stack.contains(&"Rust".to_string()));
        assert!(p.stack.contains(&"Workspaces".to_string()));
        assert_eq!(
            p.workspaces
                .iter()
                .map(|w| w.path.as_str())
                .collect::<Vec<_>>(),
            ["crates/a", "crates/b"]
        );
        // One workspace root covers all members: no per-member duplication.
        assert_eq!(
            commands(&p),
            [
                "typecheck: cargo check",
                "lint: cargo clippy",
                "test: cargo test"
            ]
        );
        assert!(p
            .checks
            .iter()
            .all(|c| c.source == "Cargo.toml [workspace]"));
    }

    #[test]
    fn empty_folder() {
        let p = detect(&project(&[]));
        assert!(p.stack.is_empty() && p.checks.is_empty() && p.instructions.is_none());
    }
}
