//! What Rovik's windows share of the state their webviews keep. A window's
//! browser storage is its own: read once, as the window starts, and written
//! whole, so a window saving its copy of a map undid what another had saved
//! meanwhile (the tabs of a project whose window had closed, a setting, a
//! chat's unread mark). Such a map is kept here instead, a file each in
//! `<data>/shared`: a save changes only the entries it names, one window at
//! a time, and the windows read the file again when another may have
//! changed it (src/lib/sharedState.ts).

use rovik_core::{atomic_file, os_lock};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

type Entries = Map<String, Value>;

/// The file of the map `name` in `dir`: a plain name, so it stays there.
fn file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let plain = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if plain {
        Ok(dir.join(format!("{name}.json")))
    } else {
        Err(format!("Not a name for shared state: {name:?}"))
    }
}

fn load(path: &Path) -> Option<Entries> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(Value::Object(entries)) => Some(entries),
        _ => None,
    }
}

/// The map `name` as saved; None while none was saved.
pub fn read(dir: &Path, name: &str) -> Result<Option<Entries>, String> {
    Ok(load(&file(dir, name)?))
}

/// Saves `patch` onto the map `name` as it is now, a null removing an
/// entry: the map as saved.
pub fn merge(dir: &Path, name: &str, patch: Entries) -> Result<Entries, String> {
    change(dir, name, |entries, _| {
        for (key, value) in patch {
            if value.is_null() {
                entries.remove(&key);
            } else {
                entries.insert(key, value);
            }
        }
    })
}

/// The map `name` as saved, or, while none is, `entries` saved as it: a
/// window's own copy from before the maps were shared. Another window's,
/// saved first, is kept whole.
pub fn start(dir: &Path, name: &str, entries: Entries) -> Result<Entries, String> {
    if let Some(saved) = read(dir, name)? {
        return Ok(saved);
    }
    // Unless another window's was saved meanwhile.
    change(dir, name, |saved, was| {
        if !was {
            *saved = entries;
        }
    })
}

/// `edit` done to the map `name` as saved (and whether one was), one
/// window at a time, so none saves onto a map another is changing.
fn change(
    dir: &Path,
    name: &str,
    edit: impl FnOnce(&mut Entries, bool),
) -> Result<Entries, String> {
    let path = file(dir, name)?;
    let failed = |e: std::io::Error| format!("Can't keep {name}: {e}");
    std::fs::create_dir_all(dir).map_err(failed)?;
    let _turn = os_lock::hold(&dir.join(format!("{name}.lock"))).map_err(failed)?;
    let saved = load(&path);
    let was = saved.is_some();
    let mut entries = saved.unwrap_or_default();
    let before = entries.clone();
    edit(&mut entries, was);
    if !was || entries != before {
        let text = serde_json::to_string(&entries).map_err(|e| e.to_string())?;
        atomic_file::write_atomic(&path, text.as_bytes()).map_err(failed)?;
    }
    Ok(entries)
}

fn dir(app: &AppHandle) -> Result<PathBuf, String> {
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(data.join("shared"))
}

#[tauri::command]
pub async fn shared_state_read(app: AppHandle, name: String) -> Result<Option<Entries>, String> {
    let dir = dir(&app)?;
    rovik_core::rt::spawn_blocking(move || read(&dir, &name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn shared_state_start(
    app: AppHandle,
    name: String,
    entries: Entries,
) -> Result<Entries, String> {
    let dir = dir(&app)?;
    rovik_core::rt::spawn_blocking(move || start(&dir, &name, entries))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn shared_state_merge(
    app: AppHandle,
    name: String,
    patch: Entries,
) -> Result<Entries, String> {
    let dir = dir(&app)?;
    rovik_core::rt::spawn_blocking(move || merge(&dir, &name, patch))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shared-state-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn entries(value: Value) -> Entries {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_save_changes_only_the_entries_it_names() {
        let dir = temp_dir("names");
        assert_eq!(
            read(&dir, "editor-settings").unwrap(),
            None,
            "none saved yet"
        );
        // Two windows, each with a copy from before the other's save.
        merge(&dir, "editor-settings", entries(json!({ "fontSize": 20 }))).unwrap();
        let saved = merge(
            &dir,
            "editor-settings",
            entries(json!({ "wordWrap": "on" })),
        )
        .unwrap();
        assert_eq!(saved, entries(json!({ "fontSize": 20, "wordWrap": "on" })));
        let saved = merge(
            &dir,
            "editor-settings",
            entries(json!({ "fontSize": null })),
        )
        .unwrap();
        assert_eq!(
            saved,
            entries(json!({ "wordWrap": "on" })),
            "a null removes"
        );
        assert_eq!(read(&dir, "editor-settings").unwrap(), Some(saved));
        assert_eq!(read(&dir, "chat-unread").unwrap(), None, "a map of its own");
    }

    #[test]
    fn a_map_starts_as_the_first_windows_copy() {
        let dir = temp_dir("start");
        let first = start(&dir, "editor-settings", entries(json!({ "fontSize": 20 }))).unwrap();
        assert_eq!(first, entries(json!({ "fontSize": 20 })));
        // The next window's copy, older, doesn't replace it.
        let next = start(&dir, "editor-settings", entries(json!({ "fontSize": 14 }))).unwrap();
        assert_eq!(next, first);
        // Nor a change saved since.
        merge(
            &dir,
            "editor-settings",
            entries(json!({ "wordWrap": "on" })),
        )
        .unwrap();
        let later = start(&dir, "editor-settings", entries(json!({ "fontSize": 14 }))).unwrap();
        assert_eq!(later, entries(json!({ "fontSize": 20, "wordWrap": "on" })));
        // An empty map saved is a map: a copy doesn't fill it.
        assert_eq!(
            start(&dir, "chat-unread", Entries::new()).unwrap(),
            Entries::new()
        );
        assert_eq!(read(&dir, "chat-unread").unwrap(), Some(Entries::new()));
        let copy = entries(json!({ "c1": true }));
        assert_eq!(start(&dir, "chat-unread", copy).unwrap(), Entries::new());
    }

    #[test]
    fn windows_saving_at_once_keep_each_others_entries() {
        let dir = temp_dir("at-once");
        let saves: Vec<_> = (0..8)
            .map(|n| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..10 {
                        let patch = entries(json!({ format!("w{n}-{i}"): true }));
                        merge(&dir, "chat-unread", patch).unwrap();
                    }
                })
            })
            .collect();
        for save in saves {
            save.join().unwrap();
        }
        assert_eq!(read(&dir, "chat-unread").unwrap().unwrap().len(), 80);
    }

    #[test]
    fn only_a_plain_name_is_a_map() {
        let dir = temp_dir("names-only");
        for name in ["", "../x", "a/b", "a.json", &"x".repeat(65)] {
            assert!(read(&dir, name).is_err(), "{name:?}");
            assert!(merge(&dir, name, Entries::new()).is_err(), "{name:?}");
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.json"), "{not json").unwrap();
        assert_eq!(
            read(&dir, "broken").unwrap(),
            None,
            "unreadable: as none saved"
        );
    }
}
