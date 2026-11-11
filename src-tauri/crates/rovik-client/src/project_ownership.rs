//! Compatibility with independent embedded backends. Shared backends use
//! client sessions exclusively; native focus locks never own their execution.
use rovik_core::{chat_db::projects::path_key, os_lock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Native window metadata, shared with the window-focus service.
#[derive(Serialize, Deserialize)]
pub struct WindowHolder {
    /// Open folder, or empty for chats without a project.
    pub path: String,
    /// Native process to bring to the foreground.
    pub pid: u32,
    /// Loopback focus listener.
    pub port: u16,
    /// Unique window token, also used to recognize this embedded owner.
    pub token: String,
    /// Whether the window retained the claim for unfinished work.
    #[serde(default)]
    pub at_work: bool,
}

/// Lock and metadata paths for a project or folder claim.
pub fn claim_files(directory: &Path, key: &str) -> (PathBuf, PathBuf) {
    let digest = Sha256::digest(key.as_bytes());
    let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    (
        directory.join(format!("{name}.lock")),
        directory.join(format!("{name}.json")),
    )
}

/// Reads metadata published just after its OS lock was acquired.
pub fn read_holder(info: &Path) -> Option<WindowHolder> {
    for attempt in 0..10 {
        if attempt > 0 {
            #[cfg(not(test))]
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if let Some(holder) = std::fs::read(info)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            return Some(holder);
        }
    }
    None
}

/// Cross-process guards enabled only for a private embedded backend.
#[derive(Clone)]
pub struct EmbeddedOwnership {
    directory: PathBuf,
    token: String,
}

impl EmbeddedOwnership {
    /// Uses the native window's claim directory and unique owner token.
    pub fn new(directory: PathBuf, token: String) -> Self {
        Self { directory, token }
    }

    /// Returns a conflicting folder; missing metadata conservatively blocks changes.
    pub fn other(&self, project: Option<&str>, root: Option<&Path>) -> Option<PathBuf> {
        let keys = project
            .map(|id| format!("project:{id}"))
            .into_iter()
            .chain(root.map(|root| format!("folder:{}", path_key(root))));
        for key in keys {
            let (lock, info) = claim_files(&self.directory, &key);
            if !os_lock::held_elsewhere(&lock) {
                continue;
            }
            let holder = read_holder(&info);
            if holder
                .as_ref()
                .is_some_and(|holder| holder.token == self.token)
            {
                continue;
            }
            return Some(
                holder
                    .map(|holder| holder.path)
                    .filter(|path| !path.is_empty())
                    .unwrap_or_else(|| "another window".into())
                    .into(),
            );
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_ownership_recognizes_tokens_and_ignores_stale_claims() {
        let directory = std::env::temp_dir().join(format!("ownership-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let mine = EmbeddedOwnership::new(directory.clone(), "mine".into());
        let other = EmbeddedOwnership::new(directory.clone(), "other".into());
        let root = directory.join("project");
        for key in [
            "project:id".to_string(),
            format!("folder:{}", path_key(&root)),
        ] {
            let (lock, info) = claim_files(&directory, &key);
            let held = os_lock::try_hold(&lock).unwrap().unwrap();
            assert_eq!(
                mine.other(Some("id"), Some(&root)),
                Some("another window".into())
            );
            std::fs::write(
                &info,
                serde_json::to_vec(&WindowHolder {
                    path: root.to_string_lossy().into(),
                    pid: std::process::id(),
                    port: 0,
                    token: "mine".into(),
                    at_work: false,
                })
                .unwrap(),
            )
            .unwrap();
            assert_eq!(mine.other(Some("id"), Some(&root)), None);
            assert_eq!(other.other(Some("id"), Some(&root)), Some(root.clone()));
            drop(held);
            assert_eq!(other.other(Some("id"), Some(&root)), None);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
