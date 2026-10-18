//! Whole check output, kept for reading in pieces (C01 in
//! docs/design/subagents-plan.md). The failure report gives the agent the
//! focused part of a failing check's output (FAILURE_OUTPUT_CHARS); when
//! there was more, the whole of it is saved here, and the agent or its
//! triage subagent reads it with `read` at a virtual path, `@checks/<file>`,
//! a few hundred lines at a time.
//!
//! The files belong to a chat, in the app's temp folder: they're only
//! needed while its run goes on. A chat's first check round of a run
//! starts its folder afresh, and landing, discarding or deleting the chat
//! removes it.

use std::path::{Path, PathBuf};

/// The virtual folder the agent reads saved output from.
pub const PREFIX: &str = "@checks/";

fn base() -> PathBuf {
    std::env::temp_dir().join("rovik-ide-checks")
}

/// A chat's folder; None for an id that can't be a folder name.
fn dir(dialog_id: &str) -> Option<PathBuf> {
    crate::overlay::chats::valid_id(dialog_id).then(|| base().join(dialog_id))
}

/// Where a chat's carried fix-round state spills large file contents
/// while its checks wait for approval (P1.8); not under `@checks/`.
pub fn carry_dir(dialog_id: &str) -> Option<PathBuf> {
    crate::overlay::chats::valid_id(dialog_id).then(|| base().join(format!("{dialog_id}.carry")))
}

/// Whether a tool's path argument names saved output.
pub fn is_checks_path(path: &str) -> bool {
    path.trim().starts_with(PREFIX)
}

/// Saves a check's whole output under `<stem>-<kind>.log` (`stem`: which
/// checks, e.g. `round-2`); returns the path the agent reads it at.
pub fn save(dialog_id: &str, stem: &str, kind: &str, text: &str) -> Option<String> {
    let dir = dir(dialog_id)?;
    std::fs::create_dir_all(&dir).ok()?;
    let kind: String = kind
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let name = format!("{stem}-{kind}.log");
    std::fs::write(dir.join(&name), text).ok()?;
    Some(format!("{PREFIX}{name}"))
}

/// The file behind `@checks/<name>`: a plain name in the chat's folder,
/// never a way out of it.
pub fn resolve(dialog_id: &str, path: &str) -> Result<PathBuf, String> {
    let name = path
        .trim()
        .strip_prefix(PREFIX)
        .ok_or_else(|| format!("{path}: not a saved check output"))?;
    if name.is_empty() || name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
        return Err(format!("{path}: not a saved check output"));
    }
    let file = dir(dialog_id)
        .ok_or_else(|| format!("{path}: not a saved check output"))?
        .join(name);
    if !file.is_file() {
        return Err(format!(
            "{path}: no such saved output (it's kept until the chat's next run runs its checks)"
        ));
    }
    Ok(file)
}

/// `read` on saved output: by line range, as for project files.
pub fn read(
    dialog_id: &str,
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    let file = resolve(dialog_id, path)?;
    let bytes = std::fs::read(&file).map_err(|e| format!("{path}: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);
    crate::read_file::read(path.trim(), &text, start_line, end_line)
}

/// Removes a chat's saved output.
pub fn clean(dialog_id: &str) {
    if let Some(dir) = dir(dialog_id) {
        remove(&dir);
    }
}

/// Removes a chat's saved output but the files whose names start with
/// `keep` (S03: a task's Prepare stage's, `prepare-…`).
pub fn clean_except(dialog_id: &str, keep: &str) {
    let Some(dir) = dir(dialog_id) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(keep) {
            continue;
        }
        let path = entry.path();
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(e) = removed {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("rovik: couldn't remove {}: {e}", path.display());
            }
        }
    }
}

fn remove(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("rovik: couldn't remove {}: {e}", dir.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_saved_whole_and_read_in_pieces() {
        let chat = format!("checks-test-{}", uuid::Uuid::new_v4());
        let text: String = (1..=3_000)
            .map(|i| format!("line {i}: error[E0308]\n"))
            .collect();
        assert!(text.len() > 8_000);
        let path = save(&chat, "round-2", "test", &text).unwrap();
        assert_eq!(path, "@checks/round-2-test.log");
        assert!(is_checks_path(&path) && !is_checks_path("src/a.rs"));
        let saved = std::fs::read_to_string(resolve(&chat, &path).unwrap()).unwrap();
        assert_eq!(saved, text, "saved whole");
        let piece = read(&chat, &path, Some(2_000), Some(2_001)).unwrap();
        assert!(
            piece.contains("line 2000: error") && piece.contains("line 2001: error"),
            "{piece}"
        );
        assert!(!piece.contains("line 1999:"), "{piece}");

        // Only names in the chat's own folder.
        for bad in [
            "@checks/../x",
            "@checks/",
            "@checks/a/b",
            "src/a.rs",
            "@checks/missing.log",
        ] {
            assert!(resolve(&chat, bad).is_err(), "{bad}");
        }
        assert!(resolve("other-chat", &path).is_err());

        clean(&chat);
        assert!(resolve(&chat, &path).is_err());
        clean(&chat); // twice: harmless
    }

    #[test]
    fn a_tasks_later_runs_keep_its_prepare_output() {
        let chat = format!("checks-test-{}", uuid::Uuid::new_v4());
        let prepare = save(&chat, "prepare", "1", "added 1 package").unwrap();
        let round = save(&chat, "round-1", "test", "1 failed").unwrap();
        clean_except(&chat, "prepare-");
        assert!(resolve(&chat, &prepare).is_ok(), "kept");
        assert!(resolve(&chat, &round).is_err(), "gone");
        // A chat with no output: nothing to do.
        clean_except(&format!("checks-test-{}", uuid::Uuid::new_v4()), "prepare-");
        clean(&chat);
        assert!(resolve(&chat, &prepare).is_err());
    }

    #[test]
    fn discarding_or_landing_a_chat_removes_its_output() {
        // Landing discards the chat's overlay once its changes are in the
        // project (overlay::chats::land_now_with): one hook covers both,
        // and deleting a chat too (`forget`).
        let chat = format!("checks-test-{}", uuid::Uuid::new_v4());
        let path = save(&chat, "round-1", "lint", "x").unwrap();
        assert!(resolve(&chat, &path).is_ok());
        crate::overlay::chats::discard(&chat).unwrap();
        assert!(resolve(&chat, &path).is_err());
        assert!(!base().join(&chat).exists());
    }
}
