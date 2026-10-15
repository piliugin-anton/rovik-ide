//! state.json (5.1, 6.2): each chat's computer and how its run last
//! started, whether that run is going and how the last one ended, and each
//! computer's configuration, all without keys (keys.json has them, D11) or
//! the message's text. Read at start-up, so a chat's events still find
//! their computer, its runs their computer's configuration (D16), and an
//! interrupted run its way back (5.4).

use crate::keys::Keys;
use rovik_protocol::agent::{
    AgentConfig, ModelProfile, OpenAIChatOptions, StartKind, StartRequest,
};
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

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatState {
    /// The computer that started it here: only it gets the chat's events.
    pub computer: String,
    /// How its last run started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<StartSettings>,
    /// From the run's `RunStarted` to its `RunEnded`.
    #[serde(default)]
    pub running: bool,
    /// The id of its last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// How its last run ended: its `RunEnded` outcome, or `interrupted`
    /// when the daemon stopped where no start picks it up (5.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// When the daemon restarted its run after a crash, in epoch ms (5.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restarts: Vec<i64>,
}

/// A chat restarted this many times within `RESTART_WINDOW_MS` isn't
/// restarted again: it waits for the user (5.4).
const MAX_RESTARTS: usize = 2;
const RESTART_WINDOW_MS: i64 = 60 * 60 * 1000;

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
                keys.give(config.models_mut());
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

    /// A start from `computer` (6.2): its keys go to keys.json, and its
    /// settings and configuration, without them, to state.json. Another
    /// computer's chat is refused.
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
            keys_changed |= inner.keys.take(config.models());
            inner.set_config(computer, config.clone());
        }
        let mut chat = earlier.unwrap_or_else(|| ChatState {
            computer: computer.to_string(),
            ..ChatState::default()
        });
        chat.start = Some(StartSettings::of(start));
        inner.state.chats.insert(start.dialog_id.clone(), chat);
        if keys_changed {
            inner.keys.write(&self.keys_file).map_err(internal)?;
        }
        self.save(&inner).map_err(internal)
    }

    /// `computer`'s configuration from now on, and only its: the app sends
    /// it without keys, which come from keys.json.
    pub fn configure(&self, computer: &str, config: AgentConfig) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.keys.take(config.models()) {
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

    /// The chat's run started.
    pub fn run_started(&self, dialog_id: &str, run_id: &str) {
        self.change(dialog_id, |chat| {
            chat.running = true;
            chat.run_id = Some(run_id.to_string());
        });
    }

    /// The chat's run ended, with this outcome if it's known.
    pub fn ended(&self, dialog_id: &str, outcome: Option<&str>) {
        self.change(dialog_id, |chat| {
            chat.running = false;
            if let Some(outcome) = outcome {
                chat.outcome = Some(outcome.to_string());
            }
        });
    }

    /// The chats whose run was going when the daemon stopped.
    pub fn running(&self) -> Vec<String> {
        let inner = self.lock();
        inner
            .state
            .chats
            .iter()
            .filter(|(_, chat)| chat.running)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The id of the chat's last run.
    pub fn run_id(&self, dialog_id: &str) -> Option<String> {
        self.lock().state.chats.get(dialog_id)?.run_id.clone()
    }

    /// Counts a restart of the chat's run at `now` (5.4): false, with
    /// nothing counted, when it was restarted twice within the hour.
    pub fn count_restart(&self, dialog_id: &str, now: i64) -> bool {
        let mut counted = false;
        self.change(dialog_id, |chat| {
            chat.restarts.retain(|at| now - at < RESTART_WINDOW_MS);
            if chat.restarts.len() < MAX_RESTARTS {
                chat.restarts.push(now);
                counted = true;
            }
        });
        counted
    }

    /// The start that picks up the chat's interrupted run (5.4): its last
    /// start's settings, with its key from keys.json. The run follows its
    /// computer's configuration, as every run here does.
    pub fn restart_request(&self, dialog_id: &str, start: StartKind) -> Option<StartRequest> {
        let inner = self.lock();
        let settings = inner.state.chats.get(dialog_id)?.start.clone()?;
        let mut profile = settings.profile;
        inner.keys.give([&mut profile]);
        Some(StartRequest {
            dialog_id: dialog_id.to_string(),
            start,
            profile,
            options: settings.options,
            mode: settings.mode,
            context_mode: settings.context_mode,
            root: settings.root,
            // The stored message, as it was first sent (`meta.sentContent`).
            current_user_content: None,
            local_date: settings.local_date,
            config: None,
            isolate: None,
            // Kept here since the start that brought them.
            project_settings: None,
        })
    }

    /// Changes a chat known here, and saves what changed.
    fn change(&self, dialog_id: &str, f: impl FnOnce(&mut ChatState)) {
        let mut inner = self.lock();
        let Some(chat) = inner.state.chats.get_mut(dialog_id) else {
            return;
        };
        let before = chat.clone();
        f(chat);
        if *chat == before {
            return;
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
        self.keys.give(config.models_mut());
        self.state
            .computers
            .insert(computer.to_string(), config.without_keys());
        self.configs.insert(computer.to_string(), config);
    }
}

fn internal(message: String) -> Fail {
    Fail::new(FailCode::Internal, message)
}

#[cfg(test)]
mod tests;
