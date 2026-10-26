//! A harness's own work as the chat records it (connectors/mod.rs
//! `HarnessStep`), the same for every harness: a stretch of its thinking as
//! a step in its place among its tool calls, the paths its steps name as
//! the project's, and token counts as the user reads them.

use super::HarnessStep;
use serde_json::Value;
use std::path::Path;
use std::time::Instant;

/// A stretch of thinking as it streams, with the harness's own tools.
pub(super) struct Thought {
    /// Its step, numbered once it says anything (`thought_id`).
    pub(super) step: HarnessStep,
    /// When it began: how long it took shows with it.
    pub(super) started: Instant,
}

impl Thought {
    /// A thought beginning now.
    pub(super) fn new() -> Thought {
        Thought {
            step: HarnessStep {
                thinking: Some(String::new()),
                ..HarnessStep::default()
            },
            started: Instant::now(),
        }
    }
}

/// The step id of a run's `n`th thought shown.
pub(super) fn thought_id(n: usize) -> String {
    format!("thinking-{n}")
}

/// A token count for the user: `35.3k`, `1.2M`, `950`.
pub(super) fn thousands(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// `path` relative to the run's folder when it's in it, as Rovik's own
/// tools take paths: the chat opens it in the project, or in the chat's
/// copy of it. One outside it stays as it is.
pub(super) fn relative_path(path: &str, root: &Path) -> String {
    // The folder as given, and as the system resolves it (`/tmp` and
    // `/private/tmp`): the harness may name it either way.
    let roots = [Some(root.to_path_buf()), root.canonicalize().ok()];
    let inside = roots
        .iter()
        .flatten()
        .find_map(|root| Path::new(path).strip_prefix(root).ok());
    match inside {
        Some(rest) => {
            let rest = rest.to_string_lossy().replace('\\', "/");
            if rest.is_empty() {
                ".".into()
            } else {
                rest
            }
        }
        None => path.to_string(),
    }
}

/// A tool's input with the paths it names (`file_path`, `path`,
/// `notebook_path`) relative to the run's folder where they're in it
/// (`relative_path`). Others stay as they are.
pub(super) fn relative_paths(input: &Value, root: Option<&Path>) -> Value {
    let mut input = input.clone();
    let (Some(root), Some(fields)) = (root, input.as_object_mut()) else {
        return input;
    };
    for key in ["file_path", "path", "notebook_path"] {
        if let Some(Value::String(path)) = fields.get_mut(key) {
            *path = relative_path(path, root);
        }
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn token_counts_read_in_thousands_and_millions() {
        assert_eq!(thousands(950), "950");
        assert_eq!(thousands(35_300), "35.3k");
        assert_eq!(thousands(1_200_000), "1.2M");
    }

    #[test]
    fn paths_in_the_folder_are_relative_to_it_and_others_stay() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.clone();
        let inside = root.join("src").join("a.rs");
        assert_eq!(relative_path(&inside.to_string_lossy(), &root), "src/a.rs");
        assert_eq!(relative_path(&root.to_string_lossy(), &root), ".");
        assert_eq!(relative_path("/etc/hosts", &root), "/etc/hosts");
        let input = json!({ "file_path": inside, "pattern": "x" });
        let out = relative_paths(&input, Some(&root));
        assert_eq!(out, json!({ "file_path": "src/a.rs", "pattern": "x" }));
        assert_eq!(relative_paths(&input, None), input);
    }
}
