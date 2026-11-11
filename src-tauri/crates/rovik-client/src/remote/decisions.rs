//! Local choices that a reconnect cannot reconstruct from the remote server.
use super::{ChatSync, State};
use rovik_core::{atomic_file, os_lock};
use serde::{Deserialize, Serialize};
#[cfg(any(unix, windows, test))]
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct Saved {
    #[serde(default = "version")]
    version: u32,
    chat: String,
    host: Option<String>,
    #[serde(default)]
    kept: Option<ChatSync>,
    #[serde(default)]
    rechecks: u32,
}

fn version() -> u32 {
    1
}

impl State {
    fn decision_path(&self, chat: &str) -> PathBuf {
        self.decision_dir.join(format!(
            "{}.json",
            rovik_core::file_guard::sha256_hex(chat.as_bytes())
        ))
    }

    /// A failed write keeps the live choice and prevents retirement until saved.
    pub(super) fn save_decisions(&mut self, chat: &str) -> Result<(), String> {
        let result = (|| {
            let kept =
                if self.kept.contains(chat) {
                    Some(self.syncs.get(chat).cloned().ok_or(
                        "The kept remote copy has no sync state; synchronize before retrying",
                    )?)
                } else {
                    None
                };
            let rechecks = self.rechecks.get(chat).copied().unwrap_or(0);
            let host = self.chats.get(chat).cloned();
            if host.is_none() && (kept.is_some() || rechecks != 0) {
                return Err("Local remote decisions have no saved host".into());
            }
            let path = self.decision_path(chat);
            if kept.is_none() && rechecks == 0 && !path.try_exists().map_err(|e| e.to_string())? {
                return Ok(());
            }
            std::fs::create_dir_all(&self.decision_dir).map_err(|e| e.to_string())?;
            let _turn = os_lock::hold(&path.with_extension("lock")).map_err(|e| e.to_string())?;
            let bytes = serde_json::to_vec(&Saved {
                version: version(),
                chat: chat.into(),
                host,
                kept,
                rechecks,
            })
            .map_err(|e| e.to_string())?;
            atomic_file::write_atomic_private(&path, &bytes).map_err(|e| e.to_string())
        })()
        .map_err(|error: String| format!("Could not save remote decisions for {chat}: {error}"));
        match &result {
            Ok(()) => {
                self.decision_errors.remove(chat);
            }
            Err(error) => {
                self.decision_errors.insert(chat.into(), error.clone());
            }
        }
        result
    }

    /// Event paths keep a failed decision live and report its storage failure.
    pub(super) fn save_decisions_logged(&mut self, chat: &str) {
        if let Err(error) = self.save_decisions(chat) {
            log::error!("{error}");
        }
    }

    /// Unknown versions and corrupt files must not silently reset a decision.
    pub(super) fn load_decisions(&mut self, chat: &str, host: &str) -> Result<(), String> {
        let bytes = match std::fs::read(self.decision_path(chat)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("Could not read remote decisions: {error}")),
        };
        let saved: Saved = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Could not read remote decisions: {error}"))?;
        if saved.version != version() || saved.chat != chat {
            return Err("Remote decisions have an unsupported version or chat identity".into());
        }
        if saved.host.as_deref() != Some(host) {
            return Ok(());
        }
        if let Some(sync) = saved.kept {
            if sync.dialog_id != chat {
                return Err("The saved Keep decision belongs to another remote chat".into());
            }
            self.syncs.insert(chat.into(), sync);
            self.kept.insert(chat.into());
        }
        if saved.rechecks != 0 {
            self.rechecks.insert(chat.into(), saved.rechecks);
        }
        Ok(())
    }

    /// Retry failed writes and checkpoint every live choice before retirement.
    #[cfg(any(unix, windows, test))]
    pub(super) fn checkpoint_decisions(&mut self) -> Result<(), String> {
        let chats: BTreeSet<String> = self
            .kept
            .iter()
            .chain(self.rechecks.keys())
            .chain(self.decision_errors.keys())
            .cloned()
            .collect();
        for chat in chats {
            self.save_decisions(&chat)?;
        }
        Ok(())
    }
}
