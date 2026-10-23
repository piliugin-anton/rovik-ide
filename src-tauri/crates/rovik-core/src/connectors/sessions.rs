//! The harness session each chat continues (connectors/mod.rs): a small
//! JSON file in the core's data folder, `connectors/sessions.json`, one
//! entry per connector and chat. A harness names its session when it
//! starts (`ChatStreamEvent::HarnessSession`), and the chat's next turn
//! resumes it. Nothing here is secret: a session id names a transcript the
//! harness keeps in its own folder.

use super::ConnectorKind;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The file, under the data folder.
const FILE: &str = "connectors/sessions.json";

/// One process reads and writes the file at a time.
static LOCK: Mutex<()> = Mutex::new(());

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

fn key(kind: ConnectorKind, dialog_id: &str) -> String {
    format!("{}:{dialog_id}", kind.id())
}

fn load(data_dir: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path(data_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn save(data_dir: &Path, map: &Map<String, Value>) {
    let file = path(data_dir);
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = serde_json::to_string_pretty(&Value::Object(map.clone())).unwrap_or_default();
    if let Err(e) = crate::atomic_file::write_atomic(&file, text.as_bytes()) {
        eprintln!("connectors: {}: {e}", file.display());
    }
}

/// The session `dialog_id` continues on `kind`, if one was kept.
pub fn get(data_dir: &Path, kind: ConnectorKind, dialog_id: &str) -> Option<String> {
    let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load(data_dir)
        .get(&key(kind, dialog_id))?
        .get("session")?
        .as_str()
        .map(str::to_string)
}

/// Keeps `session` as the one `dialog_id` continues on `kind`.
pub fn put(data_dir: &Path, kind: ConnectorKind, dialog_id: &str, session: &str) {
    let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = load(data_dir);
    let k = key(kind, dialog_id);
    if map
        .get(&k)
        .and_then(|v| v.get("session"))
        .and_then(|s| s.as_str())
        == Some(session)
    {
        return;
    }
    map.insert(
        k,
        serde_json::json!({ "session": session, "at": crate::chat_db::store::now_ms() }),
    );
    save(data_dir, &map);
}

/// Forgets the session kept for `dialog_id` on `kind` (one the harness no
/// longer has): the next turn starts a new one.
pub fn forget(data_dir: &Path, kind: ConnectorKind, dialog_id: &str) {
    let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = load(data_dir);
    if map.remove(&key(kind, dialog_id)).is_some() {
        save(data_dir, &map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;

    #[test]
    fn sessions_are_kept_per_connector_and_chat() {
        let dir = temp_dir();
        assert_eq!(get(&dir.0, ConnectorKind::ClaudeCode, "d1"), None);
        put(&dir.0, ConnectorKind::ClaudeCode, "d1", "s-1");
        put(&dir.0, ConnectorKind::ClaudeCode, "d2", "s-2");
        assert_eq!(
            get(&dir.0, ConnectorKind::ClaudeCode, "d1").as_deref(),
            Some("s-1")
        );
        assert_eq!(
            get(&dir.0, ConnectorKind::ClaudeCode, "d2").as_deref(),
            Some("s-2")
        );
        put(&dir.0, ConnectorKind::ClaudeCode, "d1", "s-1b");
        assert_eq!(
            get(&dir.0, ConnectorKind::ClaudeCode, "d1").as_deref(),
            Some("s-1b")
        );
        forget(&dir.0, ConnectorKind::ClaudeCode, "d1");
        assert_eq!(get(&dir.0, ConnectorKind::ClaudeCode, "d1"), None);
        assert_eq!(
            get(&dir.0, ConnectorKind::ClaudeCode, "d2").as_deref(),
            Some("s-2")
        );
        assert!(dir.0.join("connectors/sessions.json").is_file());
    }
}
