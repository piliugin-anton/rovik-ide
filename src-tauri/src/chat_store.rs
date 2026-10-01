//! Chat history storage: one JSON file per chat in `<app data>/chats/`.
//!
//! Replaces the single `chat-history.json`, which was rewritten in full on
//! every change. Now:
//!  - saving touches only the chat that changed;
//!  - every write is atomic (temp file + rename), so a crash can't truncate it;
//!  - a damaged file is moved to `chats/corrupt/` and skipped, never
//!    overwritten or deleted, and it can't take the other chats down with it.
//!
//!
//! Commands do their disk I/O on a blocking thread: Tauri runs synchronous
//! commands on the main thread, where a large load would freeze the window.

use crate::atomic_file::write_atomic;
use serde::de::IgnoredAny;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const CHATS_DIR: &str = "chats";
const CORRUPT_DIR: &str = "corrupt";

fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))
}

/// Chat ids become file names, so only allow UUID-like characters. This also
/// rules out path traversal ("../") from a malformed id.
fn chat_file(dir: &Path, id: &str) -> Result<PathBuf, String> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(format!("Invalid chat id: {id:?}"));
    }
    Ok(dir.join(format!("{id}.json")))
}

fn is_valid_json(text: &str) -> bool {
    serde_json::from_str::<IgnoredAny>(text).is_ok()
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Moves a file aside instead of deleting it, so nothing is ever lost.
fn quarantine(file: &Path, chats_dir: &Path) {
    let corrupt_dir = chats_dir.join(CORRUPT_DIR);
    if fs::create_dir_all(&corrupt_dir).is_err() {
        return;
    }
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown.json".into());
    let target = corrupt_dir.join(format!("{}-{name}", timestamp()));
    if let Err(e) = fs::rename(file, &target) {
        eprintln!("chat_store: could not quarantine {}: {e}", file.display());
    } else {
        eprintln!("chat_store: moved unreadable chat file to {}", target.display());
    }
}

/// Removes temp files left behind by a write that was interrupted.
fn remove_stale_temp_files(chats_dir: &Path) {
    let Ok(entries) = fs::read_dir(chats_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name.ends_with(".tmp") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn load_all(app: &AppHandle) -> Result<String, String> {
    let chats_dir = app_data_dir(app)?.join(CHATS_DIR);
    fs::create_dir_all(&chats_dir)
        .map_err(|e| format!("Failed to create chats directory: {e}"))?;

    remove_stale_temp_files(&chats_dir);

    // A directory we can't list is a real failure: report it, so the
    // frontend knows not to treat the history as empty.
    let entries =
        fs::read_dir(&chats_dir).map_err(|e| format!("Failed to read chats directory: {e}"))?;

    let mut parts: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") || !path.is_file() {
            continue;
        }
        match fs::read_to_string(&path) {
            Ok(text) if is_valid_json(&text) => parts.push(text),
            Ok(_) => quarantine(&path, &chats_dir),
            Err(e) => eprintln!("chat_store: could not read {}: {e}", path.display()),
        }
    }

    // Each file is already a JSON object; join them into an array without
    // parsing and re-serializing every chat.
    Ok(format!("[{}]", parts.join(",")))
}

/// Returns every chat as a JSON array. Fails only if the chats directory
/// itself can't be read; individual bad files are quarantined and skipped.
#[tauri::command]
pub async fn load_chat_history(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || load_all(&app))
        .await
        .map_err(|e| format!("Chat history load task failed: {e}"))?
}

#[tauri::command]
pub async fn save_chat_dialog(app: AppHandle, id: String, json: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !is_valid_json(&json) {
            return Err(format!("Refusing to save chat {id}: not valid JSON"));
        }
        let chats_dir = app_data_dir(&app)?.join(CHATS_DIR);
        let path = chat_file(&chats_dir, &id)?;
        write_atomic(&path, json.as_bytes()).map_err(|e| format!("Failed to save chat {id}: {e}"))
    })
    .await
    .map_err(|e| format!("Chat save task failed: {e}"))?
}

#[tauri::command]
pub async fn delete_chat_dialog(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let chats_dir = app_data_dir(&app)?.join(CHATS_DIR);
        let path = chat_file(&chats_dir, &id)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Failed to delete chat {id}: {e}")),
        }
    })
    .await
    .map_err(|e| format!("Chat delete task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ids_that_could_escape_the_directory() {
        let dir = Path::new("/tmp/chats");
        assert!(chat_file(dir, "0b6f1c2e-91aa-4d6e-9c1f-3b2a1d0e9f88").is_ok());
        for bad in ["", "../secrets", "a/b", "a\\b", "x.json", &"a".repeat(200)] {
            assert!(chat_file(dir, bad).is_err(), "should reject {bad:?}");
        }
    }
}
