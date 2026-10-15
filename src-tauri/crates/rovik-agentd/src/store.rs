//! state.json (5.1, 6.2): each chat's computer and how its run last
//! started, whether that run is going and how the last one ended, its
//! project and worktree (6.4), the move that brought it and whether its
//! computer took it back (6.8), each computer's configuration and each
//! project's settings, all without keys (keys.json has them, D11) or the
//! message's text. Read at start-up, so a chat's events still find their
//! computer, its runs their computer's configuration (D16), and an
//! interrupted run its way back (5.4).

use crate::keys::{self, Keys};
use rovik_protocol::agent::{
    AgentConfig, ModelProfile, OpenAIChatOptions, StartKind, StartRequest,
};
use rovik_protocol::host::KeyInfo;
use rovik_protocol::sync::Ownership;
use rovik_protocol::work::{ChatResult, Project};
use rovik_protocol::{Fail, FailCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// The projects a computer carried here, by id (6.4).
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectState>,
}

/// A project a computer carried here, as its last `ProjectEnsure` said.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectState {
    pub name: String,
    /// Its folder in its repository: "" at its root (K31).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subpath: String,
    /// Its settings (K03), for each worktree of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatState {
    /// The computer that started or moved it here: only it gets the chat's
    /// events.
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
    /// Its project, once it has a worktree (6.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Its worktree's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// The commit its worktree is based on: the landing base (6.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Its newest result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ChatResult>,
    /// Files a sync or the user put conflict markers in for the chat to
    /// settle, until they're gone (6.7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marked: Vec<String>,
    /// Its next message's start is to tell the chat of them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tell_marked: bool,
    /// Files a merge or conflict markers replaced, by their paths from the
    /// worktree's root: what Undo the merge puts back (6.1).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kept: BTreeMap<String, Kept>,
    /// The move that brought it here, if one did (6.8): sent again, its
    /// import changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_id: Option<String>,
    /// The daemon's revision once that move's rows were in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_rev: Option<i64>,
    /// Why its changes waited to land when it moved here, as its overlay
    /// said: its results say it until a run ends here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<String>,
    /// When its computer took it back (`ChatRelease`), in epoch ms: its
    /// rows are only read from then on, for that computer's last pull,
    /// until `ChatForget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<i64>,
}

/// A chat a computer moved here (6.8), as its import left it.
pub struct Moved {
    pub move_id: String,
    pub rev: i64,
    pub place: Place,
    /// The snapshot its worktree started from: its landing base.
    pub base: String,
    pub pending: Option<String>,
}

/// What a merge or conflict markers replaced in a chat's file, as blobs in
/// its project's repository, the way a local chat's overlay keeps them
/// (`overlay::chats::undo_merge`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Kept {
    /// The chat's own version.
    pub chat: String,
    /// Its landing base's, when it had the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub base_exec: bool,
    /// What the last merge wrote: a file changed since stays as it is.
    pub written: String,
}

/// Where a chat works: its project and its worktree's folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub project: String,
    pub worktree: PathBuf,
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
        inner.check_owner(computer, &start.dialog_id)?;
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

    /// The providers whose key keys.json keeps, and their base URLs: never
    /// the keys (`KeysList`).
    pub fn keys(&self) -> Vec<KeyInfo> {
        let providers = self.lock().keys.providers();
        providers
            .into_iter()
            .map(|(provider, base_url)| KeyInfo { provider, base_url })
            .collect()
    }

    /// Forgets `provider`'s key, or every key (`KeysRemove`): from
    /// keys.json, and from each computer's configuration, so the runs that
    /// start from now on go without it until a start brings it again.
    /// Runs going now finish with theirs. How many keys went.
    pub fn remove_keys(&self, provider: Option<&str>) -> Result<usize, String> {
        let mut inner = self.lock();
        let removed = inner.keys.remove(provider);
        for config in inner.configs.values_mut() {
            for model in config.models_mut() {
                if provider.is_none_or(|p| keys::provider(model) == p) {
                    model.api_key.clear();
                }
            }
        }
        inner.keys.write(&self.keys_file)?;
        Ok(removed)
    }

    /// Refuses another computer's chat.
    pub fn check_owner(&self, computer: &str, dialog_id: &str) -> Result<(), Fail> {
        self.lock().check_owner(computer, dialog_id)
    }

    /// The project as its computer last described it (6.4).
    pub fn ensure_project(&self, project: &Project, settings: Option<Value>) -> Result<(), String> {
        let mut inner = self.lock();
        let state = ProjectState {
            name: project.name.clone(),
            subpath: project.subpath.clone(),
            settings,
        };
        if inner.state.projects.get(&project.id) == Some(&state) {
            return Ok(());
        }
        inner.state.projects.insert(project.id.clone(), state);
        self.save(&inner)
    }

    pub fn project(&self, id: &str) -> Option<ProjectState> {
        self.lock().state.projects.get(id).cloned()
    }

    /// `computer`'s chat has its worktree at `worktree`, based on `base`.
    pub fn worktree_made(
        &self,
        computer: &str,
        dialog_id: &str,
        place: Place,
        base: &str,
    ) -> Result<(), Fail> {
        let mut inner = self.lock();
        inner.check_owner(computer, dialog_id)?;
        let chat = inner
            .state
            .chats
            .entry(dialog_id.to_string())
            .or_insert_with(|| ChatState {
                computer: computer.to_string(),
                ..ChatState::default()
            });
        chat.project = Some(place.project);
        chat.worktree = Some(place.worktree);
        chat.base = Some(base.to_string());
        self.save(&inner).map_err(internal)
    }

    /// Where the chat works, once it has a worktree.
    pub fn place(&self, dialog_id: &str) -> Option<Place> {
        let inner = self.lock();
        let chat = inner.state.chats.get(dialog_id)?;
        Some(Place {
            project: chat.project.clone()?,
            worktree: chat.worktree.clone()?,
        })
    }

    /// The commit the chat's worktree is based on.
    pub fn base_of(&self, dialog_id: &str) -> Option<String> {
        self.lock().state.chats.get(dialog_id)?.base.clone()
    }

    /// The chat's landing base moved: a result landed on its computer.
    pub fn base_moved(&self, dialog_id: &str, base: &str) {
        self.change(dialog_id, |chat| chat.base = Some(base.to_string()));
    }

    /// How the chat's last run ended, when it ran here.
    pub fn outcome_of(&self, dialog_id: &str) -> Option<String> {
        self.lock().state.chats.get(dialog_id)?.outcome.clone()
    }

    /// The chat's newest result, numbered after the one before it: the
    /// same result again keeps its number.
    pub fn result_made(&self, dialog_id: &str, mut result: ChatResult) -> ChatResult {
        self.change(dialog_id, |chat| {
            let unnumbered = |r: &ChatResult| ChatResult {
                seq: 0,
                ..r.clone()
            };
            result.seq = match &chat.result {
                Some(last) if unnumbered(last) == unnumbered(&result) => last.seq,
                Some(last) => last.seq + 1,
                None => 1,
            };
            chat.result = Some(result.clone());
        });
        result
    }

    /// Conflict markers went into `files` for the chat to settle: they're
    /// kept, and with `tell`, the chat's next message tells it (a sync's).
    pub fn mark(&self, dialog_id: &str, files: &[String], tell: bool) {
        self.change(dialog_id, |chat| {
            chat.marked.extend(files.iter().cloned());
            chat.marked.sort();
            chat.marked.dedup();
            chat.tell_marked |= tell;
        });
    }

    /// The files a sync marked that had their markers still, last looked.
    pub fn marked_of(&self, dialog_id: &str) -> Vec<String> {
        let inner = self.lock();
        let chat = inner.state.chats.get(dialog_id);
        chat.map(|chat| chat.marked.clone()).unwrap_or_default()
    }

    /// Of the files a sync marked, `left` still have their markers.
    pub fn marks_left(&self, dialog_id: &str, left: Vec<String>) {
        self.change(dialog_id, |chat| {
            chat.tell_marked &= !left.is_empty();
            chat.marked = left;
        });
    }

    /// The marked files the chat is to be told of, once.
    pub fn marks_to_tell(&self, dialog_id: &str) -> Option<Vec<String>> {
        let mut told = None;
        self.change(dialog_id, |chat| {
            if std::mem::take(&mut chat.tell_marked) {
                told = Some(chat.marked.clone());
            }
        });
        told
    }

    /// The project's folder in the chat's worktree, from its root: "" when
    /// the project is its repository's root folder (K31).
    pub fn subpath_of(&self, dialog_id: &str) -> String {
        let inner = self.lock();
        let chat = inner.state.chats.get(dialog_id);
        let project = chat.and_then(|chat| inner.state.projects.get(chat.project.as_ref()?));
        project.map(|p| p.subpath.clone()).unwrap_or_default()
    }

    /// What merges or markers replaced in the chat's files: the earliest
    /// version of a file stays, with what the last merge wrote.
    pub fn keep(&self, dialog_id: &str, kept: Vec<(String, Kept)>) {
        if kept.is_empty() {
            return;
        }
        self.change(dialog_id, |chat| {
            for (path, new) in kept {
                let written = new.written.clone();
                let entry = chat.kept.entry(path);
                entry.and_modify(|k| k.written = written).or_insert(new);
            }
        });
    }

    /// The files merges or markers replaced, and what they had before.
    pub fn kept_of(&self, dialog_id: &str) -> BTreeMap<String, Kept> {
        let inner = self.lock();
        let chat = inner.state.chats.get(dialog_id);
        chat.map(|chat| chat.kept.clone()).unwrap_or_default()
    }

    /// Forgets what was kept of `paths`, or of every file: taken back,
    /// landed, or thrown away.
    pub fn forget_kept(&self, dialog_id: &str, paths: Option<&[String]>) {
        self.change(dialog_id, |chat| match paths {
            Some(paths) => chat.kept.retain(|path, _| !paths.contains(path)),
            None => chat.kept.clear(),
        });
    }

    /// `computer` moved the chat here (6.8): the import's state replaces
    /// all an earlier stay left.
    pub fn imported(&self, computer: &str, dialog_id: &str, moved: Moved) -> Result<(), Fail> {
        let mut inner = self.lock();
        inner.check_owner(computer, dialog_id)?;
        let chat = ChatState {
            computer: computer.to_string(),
            project: Some(moved.place.project),
            worktree: Some(moved.place.worktree),
            base: Some(moved.base),
            move_id: Some(moved.move_id),
            imported_rev: Some(moved.rev),
            pending: moved.pending,
            ..ChatState::default()
        };
        inner.state.chats.insert(dialog_id.to_string(), chat);
        self.save(&inner).map_err(internal)
    }

    /// The chat as this daemon has it (`ChatOwner`): none when it doesn't
    /// know it.
    pub fn ownership(&self, dialog_id: &str) -> Option<Ownership> {
        let inner = self.lock();
        let chat = inner.state.chats.get(dialog_id)?;
        Some(Ownership {
            move_id: chat.move_id.clone(),
            released: chat.released.is_some(),
            rev: chat.imported_rev.unwrap_or(0),
        })
    }

    /// The chat's computer took it back at `now` (`ChatRelease`).
    pub fn release(&self, dialog_id: &str, now: i64) {
        self.change(dialog_id, |chat| {
            chat.released.get_or_insert(now);
        });
    }

    /// Whether the chat's computer took it back.
    pub fn is_released(&self, dialog_id: &str) -> bool {
        let inner = self.lock();
        let chat = inner.state.chats.get(dialog_id);
        chat.is_some_and(|chat| chat.released.is_some())
    }

    /// Why the chat's changes waited when it moved here, while no run has
    /// ended here since.
    pub fn pending_of(&self, dialog_id: &str) -> Option<String> {
        self.lock().state.chats.get(dialog_id)?.pending.clone()
    }

    /// The chat's newest result.
    pub fn result_of(&self, dialog_id: &str) -> Option<ChatResult> {
        self.lock().state.chats.get(dialog_id)?.result.clone()
    }

    /// The newest result of each of `computer`'s chats here: not of those
    /// it took back, whose results it has.
    pub fn results_of(&self, computer: &str) -> Vec<ChatResult> {
        let inner = self.lock();
        let chats = inner.state.chats.values();
        chats
            .filter(|chat| chat.computer == computer && chat.released.is_none())
            .filter_map(|chat| chat.result.clone())
            .collect()
    }

    /// The chats that have a worktree.
    pub fn chats_with_worktrees(&self) -> Vec<String> {
        let inner = self.lock();
        let chats = inner.state.chats.iter();
        chats
            .filter(|(_, chat)| chat.worktree.is_some())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// A deleted chat: nothing of it is kept.
    pub fn forget(&self, dialog_id: &str) {
        let mut inner = self.lock();
        if inner.state.chats.remove(dialog_id).is_some() {
            if let Err(e) = self.save(&inner) {
                log::error!("state.json: {e}");
            }
        }
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

    /// The chat's run ended, with this outcome if it's known: its results
    /// say why its changes wait from it now.
    pub fn ended(&self, dialog_id: &str, outcome: Option<&str>) {
        self.change(dialog_id, |chat| {
            chat.running = false;
            if let Some(outcome) = outcome {
                chat.outcome = Some(outcome.to_string());
                chat.pending = None;
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
    fn check_owner(&self, computer: &str, dialog_id: &str) -> Result<(), Fail> {
        let chat = self.state.chats.get(dialog_id);
        if chat.is_some_and(|c| c.computer != computer) {
            return Err(Fail::new(
                FailCode::NotOwner,
                "Another computer moved this chat to the server.",
            ));
        }
        Ok(())
    }

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
