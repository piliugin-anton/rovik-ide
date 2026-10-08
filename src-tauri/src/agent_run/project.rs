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
pub fn effective_checks(profile: &ProjectProfile, settings: &serde_json::Value) -> Vec<ProjectCheck> {
    if let Some(saved) = saved_checks(settings) {
        return saved;
    }
    profile
        .checks
        .iter()
        .map(|c| ProjectCheck { kind: c.kind.clone(), command: c.command.clone(), enabled: c.default_enabled })
        .collect()
}

/// Checks in the order they run: cheap and broad first.
pub fn ordered_checks(checks: &[ProjectCheck]) -> Vec<ProjectCheck> {
    let mut out = checks.to_vec();
    out.sort_by_key(|c| CHECK_KINDS.iter().position(|k| *k == c.kind).unwrap_or(CHECK_KINDS.len()));
    out
}

/// The "## Project" section of the system prompt: stack, the check commands
/// (and that the IDE runs them when verification is on), and the project's
/// own agent instructions.
pub fn project_context(profile: &ProjectProfile, checks: &[ProjectCheck], verify_on: bool) -> String {
    let mut parts = vec!["## Project".to_string()];
    if !profile.stack.is_empty() {
        parts.push(format!("Stack: {}.", profile.stack.join(", ")));
    }

    let enabled: Vec<ProjectCheck> = ordered_checks(checks).into_iter().filter(|c| c.enabled).collect();
    if !enabled.is_empty() {
        let mut list = vec![if verify_on {
            "Checks: when you finish and have changed files, the IDE runs these itself and sends you any failures to fix:"
        } else {
            "Checks you can run with bash after changing code:"
        }
        .to_string()];
        list.extend(enabled.iter().map(|c| format!("- {}: `{}`", check_label(&c.kind), c.command)));
        parts.push(list.join("\n"));
        if verify_on {
            parts.push(
                "Run a relevant check yourself before finishing when it helps you get the change right. \
                 If a check fails for reasons unrelated to your change (it fails on the original code too), \
                 say so plainly and don't change unrelated code to make it pass. \
                 In your final message, say what was verified."
                    .into(),
            );
        }
    }

    if let Some(notes) = &profile.instructions {
        let label = if notes.file.to_lowercase().starts_with("readme") {
            format!("From the project's {} (beginning):", notes.file)
        } else {
            format!("The project's instructions for agents ({}); follow them:", notes.file)
        };
        let more = if notes.truncated { "\n[…truncated]" } else { "" };
        parts.push(format!("{label}\n\n{}{more}", notes.text));
    }
    if parts.len() > 1 {
        parts.join("\n\n")
    } else {
        String::new()
    }
}

/// Detected fresh (a few file reads): the project's checks and its prompt section.
pub fn load<R: tauri::Runtime>(app: &tauri::AppHandle<R>, root: &str, verify_on: bool) -> (Vec<ProjectCheck>, String) {
    if !Path::new(root).is_dir() {
        return (Vec::new(), String::new());
    }
    let profile = project_profile::detect(Path::new(root));
    let settings = project_profile::load_project_settings(app.clone(), root.to_string()).unwrap_or_default();
    let checks = effective_checks(&profile, &settings);
    let context = project_context(&profile, &checks, verify_on);
    (checks, context)
}

/// The project section for estimates (the context meter), re-detected at
/// most every few seconds: estimates follow every message of a run.
pub fn cached_context<R: tauri::Runtime>(app: &tauri::AppHandle<R>, root: &str, verify_on: bool) -> String {
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
    let (_, text) = load(app, root, verify_on);
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|_, (at, _)| at.elapsed() < FRESH);
        cache.insert(key, (Instant::now(), text.clone()));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_profile::{DetectedCheck, ProjectInstructions};

    #[test]
    fn the_project_section() {
        let profile = ProjectProfile {
            stack: vec!["Rust".into()],
            checks: vec![
                DetectedCheck { kind: "test".into(), command: "cargo test".into(), source: "".into(), default_enabled: true },
                DetectedCheck { kind: "typecheck".into(), command: "cargo check".into(), source: "".into(), default_enabled: true },
                DetectedCheck { kind: "build".into(), command: "cargo build".into(), source: "".into(), default_enabled: false },
            ],
            instructions: Some(ProjectInstructions { file: "AGENTS.md".into(), text: "Be nice.".into(), truncated: false }),
            ..Default::default()
        };
        let checks = effective_checks(&profile, &serde_json::Value::Null);
        let text = project_context(&profile, &checks, true);
        assert!(text.starts_with("## Project\n\nStack: Rust.\n\nChecks: when you finish"));
        assert!(text.contains("- Type check: `cargo check`\n- Tests: `cargo test`"));
        assert!(!text.contains("cargo build"));
        assert!(text.ends_with("The project's instructions for agents (AGENTS.md); follow them:\n\nBe nice."));

        let saved = serde_json::json!({ "checks": [{ "kind": "lint", "command": "x", "enabled": false }, { "kind": "nope", "command": "y" }] });
        assert_eq!(effective_checks(&profile, &saved), vec![ProjectCheck { kind: "lint".into(), command: "x".into(), enabled: false }]);
        assert_eq!(project_context(&ProjectProfile::default(), &[], true), "");
    }
}
