//! state.json (5.1, 6.2): each chat's computer and how its run last
//! started, whether that run is going, and each computer's configuration,
//! all without keys (keys.json has them, D11) or the message's text. Read
//! at start-up, so a chat's events still find their computer, its runs
//! their computer's configuration (D16), and an interrupted run its way
//! back (5.4).

use crate::keys::{self, Keys};
use rovik_protocol::agent::{AgentConfig, ModelProfile, OpenAIChatOptions, StartRequest};
use rovik_protocol::{Fail, FailCode};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(default)]
    pub chats: BTreeMap<String, ChatState>,
    /// Each computer's last configuration.
    #[serde(default)]
    pub computers: BTreeMap<String, AgentConfig>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatState {
    /// The computer that started it here: only it gets the chat's events.
    pub computer: String,
    /// How its last run started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<StartSettings>,
    #[serde(default)]
    pub running: bool,
    /// When the daemon restarted its run after a crash, in epoch ms (5.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarts: Vec<i64>,
}

/// A start's settings, all a restarted run needs besides the computer's
/// configuration and the keys.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StartSettings {
    /// Without its key.
    pub profile: ModelProfile,
    pub options: Option<OpenAIChatOptions>,
    pub mode: Option<String>,
    pub context_mode: Option<String>,
    pub root: Option<String>,
    pub local_date: Option<String>,
}

impl StartSettings {
    fn of(start: &StartRequest) -> StartSettings {
        let mut profile = start.profile.clone();
        profile.api_key.clear();
        StartSettings {
            profile,
            options: start.options.clone(),
            mode: start.mode.clone(),
            context_mode: start.context_mode.clone(),
            root: start.root.clone(),
            local_date: start.local_date.clone(),
        }
    }
}

pub struct Store {
    state_file: PathBuf,
    keys_file: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    state: State,
    keys: Keys,
    /// Each computer's configuration, with its keys.
    configs: HashMap<String, AgentConfig>,
}

impl Store {
    /// The daemon's store in `root` (its own folder).
    pub fn open(root: &Path) -> Result<Store, String> {
        let state_file = root.join("state.json");
        let keys_file = root.join("keys.json");
        let state: State = match std::fs::read(&state_file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{}: {e}", state_file.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(format!("{}: {e}", state_file.display())),
        };
        let keys = Keys::read(&keys_file)?;
        let configs = state
            .computers
            .iter()
            .map(|(computer, config)| {
                let mut config = config.clone();
                keys.give(keys::models_mut(&mut config));
                (computer.clone(), config)
            })
            .collect();
        Ok(Store {
            state_file,
            keys_file,
            inner: Mutex::new(Inner {
                state,
                keys,
                configs,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The computer that started the chat here.
    pub fn owner(&self, dialog_id: &str) -> Option<String> {
        self.lock()
            .state
            .chats
            .get(dialog_id)
            .map(|c| c.computer.clone())
    }

    /// A start from `computer` (6.2): its keys go to keys.json, its
    /// settings and configuration, without them, to state.json, and the
    /// chat is marked running. Another computer's chat is refused.
    pub fn started(&self, computer: &str, start: &StartRequest) -> Result<(), Fail> {
        let mut inner = self.lock();
        let earlier = inner.state.chats.get(&start.dialog_id).cloned();
        if earlier.as_ref().is_some_and(|c| c.computer != computer) {
            return Err(Fail::new(
                FailCode::NotOwner,
                "Another computer moved this chat to the server.",
            ));
        }
        let mut keys_changed = inner.keys.take([&start.profile]);
        if let Some(config) = &start.config {
            keys_changed |= inner.keys.take(keys::models(config));
            inner.set_config(computer, config.clone());
        }
        inner.state.chats.insert(
            start.dialog_id.clone(),
            ChatState {
                computer: computer.to_string(),
                start: Some(StartSettings::of(start)),
                running: true,
                restarts: earlier.map(|c| c.restarts).unwrap_or_default(),
            },
        );
        if keys_changed {
            inner.keys.write(&self.keys_file).map_err(internal)?;
        }
        self.save(&inner).map_err(internal)
    }

    /// `computer`'s configuration from now on, and only its: the app sends
    /// it without keys, which come from keys.json.
    pub fn configure(&self, computer: &str, config: AgentConfig) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.keys.take(keys::models(&config)) {
            inner.keys.write(&self.keys_file)?;
        }
        inner.set_config(computer, config);
        self.save(&inner)
    }

    /// The configuration the chat's runs follow: its computer's.
    pub fn config_of(&self, dialog_id: &str) -> Option<AgentConfig> {
        let inner = self.lock();
        let computer = &inner.state.chats.get(dialog_id)?.computer;
        inner.configs.get(computer).cloned()
    }

    /// The chat's run ended.
    pub fn ended(&self, dialog_id: &str) {
        let mut inner = self.lock();
        match inner.state.chats.get_mut(dialog_id) {
            Some(chat) if chat.running => chat.running = false,
            _ => return,
        }
        if let Err(e) = self.save(&inner) {
            log::error!("state.json: {e}");
        }
    }

    fn save(&self, inner: &Inner) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&inner.state).map_err(|e| e.to_string())?;
        rovik_core::atomic_file::write_atomic_private(&self.state_file, &bytes)
            .map_err(|e| format!("{}: {e}", self.state_file.display()))
    }
}

impl Inner {
    /// The computer's configuration: with keys here, without in state.json.
    fn set_config(&mut self, computer: &str, mut config: AgentConfig) {
        self.keys.give(keys::models_mut(&mut config));
        self.state
            .computers
            .insert(computer.to_string(), keys::without_keys(&config));
        self.configs.insert(computer.to_string(), config);
    }
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

#[cfg(test)]
mod tests;
