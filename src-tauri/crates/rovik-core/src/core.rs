//! What the core needs from the process hosting it, whichever that is: the
//! app, the headless runner, and later the daemon. The host builds one
//! `Core` and the core's code takes it in place of Tauri's `AppHandle`
//! (docs/design/cloud-agent-dev-plan.md, D2).

use crate::agent_run::events::AgentEvent;
use crate::agent_run::AgentState;
use crate::browser_tools::BrowserState;
use crate::chat_db::DbState;
use crate::vfs::VfsState;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;

/// Where the core keeps its files.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The chat database and its attachments, and the overlays of isolated
    /// chats.
    pub data_dir: PathBuf,
    /// Settings the runs read: each project's (project-settings.json).
    pub config_dir: PathBuf,
    /// The agent log (agent_run/log.rs).
    pub log_dir: PathBuf,
}

impl Paths {
    /// The agent log goes in the data folder.
    pub fn new(data_dir: PathBuf, config_dir: PathBuf) -> Self {
        Paths {
            log_dir: data_dir.clone(),
            data_dir,
            config_dir,
        }
    }
}

/// Where the core's events go. `agent-event` is the only one it sends: the
/// file watcher's events belong to the editor, in the app.
pub trait EventSink: Send + Sync {
    fn agent(&self, event: &AgentEvent);
}

/// What a [`Core`] holds.
pub struct Parts {
    pub paths: Paths,
    pub db: DbState,
    pub agents: AgentState,
    pub vfs: VfsState,
    pub browser: BrowserState,
    pub events: Arc<dyn EventSink>,
}

/// Everything a run needs from its host. Cheap to clone: the clones share
/// one set of parts.
#[derive(Clone)]
pub struct Core(Arc<Parts>);

impl Deref for Core {
    type Target = Parts;

    fn deref(&self) -> &Parts {
        &self.0
    }
}

impl Core {
    /// The core of a process that hosts one: opens the chat database, and
    /// sets up what the process keeps once for its data folder, the
    /// workload's history and the isolated chats' overlays.
    pub fn new(paths: Paths, events: Arc<dyn EventSink>) -> Result<Core, String> {
        let core = Core::open(paths, events)?;
        crate::workload::init(&core.paths.data_dir);
        crate::overlay::chats::init(&core.paths.data_dir);
        Ok(core)
    }

    /// A core on its own chat database, leaving what the process keeps
    /// once as it is: tests build many cores in one process.
    pub fn open(paths: Paths, events: Arc<dyn EventSink>) -> Result<Core, String> {
        let db = DbState::open(&paths.data_dir).map_err(|e| format!("Chat database: {e}"))?;
        Ok(Core(Arc::new(Parts {
            paths,
            db,
            agents: AgentState::default(),
            vfs: VfsState::default(),
            browser: BrowserState::default(),
            events,
        })))
    }

    /// Tells the host's frontends what a run did.
    pub fn emit(&self, event: AgentEvent) {
        self.events.agent(&event);
    }

    /// As the process exits: no headless Chrome left running, no dead
    /// mount where the virtual files were, paused commands resumed and the
    /// workload's history written, and the database marked closed cleanly.
    pub async fn shutdown(&self) {
        self.browser.shutdown().await;
        let _ = self.vfs.unmount();
        crate::overlay::chats::close_all();
        crate::workload::shutdown();
        self.db.mark_clean_exit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorded(Mutex<Vec<Value>>);

    impl EventSink for Recorded {
        fn agent(&self, event: &AgentEvent) {
            let json = serde_json::to_value(event).unwrap();
            self.0.lock().unwrap().push(json);
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("core-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `Core::new` and `shutdown` set what the process keeps once, the
    /// overlays' and the workload history's folders, which other tests in
    /// this process use: they're checked in a process of their own, this
    /// test binary running only the ignored test below.
    #[test]
    fn new_opens_the_database_and_the_overlays_folder() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "core::tests::new_in_a_process_of_its_own"])
            .args(["--ignored", "--test-threads=1"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    #[test]
    #[ignore = "run in a process of its own by new_opens_the_database_and_the_overlays_folder"]
    fn new_in_a_process_of_its_own() {
        let dir = temp_dir("new");
        let data = dir.join("data");
        let paths = Paths::new(data.clone(), dir.join("config"));
        let core = Core::new(paths, Arc::new(Recorded::default())).unwrap();
        assert!(data.join("chats.db").is_file());
        assert_eq!(crate::overlay::chats::data_dir(), Some(data.clone()));
        assert_eq!(core.paths.log_dir, data);
        assert!(data.join("chats.db.running").is_file());
        crate::rt::block_on(core.shutdown());
        assert!(!data.join("chats.db.running").exists(), "a clean exit");
        drop(core);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_go_to_the_sink() {
        let dir = temp_dir("events");
        let sink = Arc::new(Recorded::default());
        let core = Core::open(Paths::new(dir.clone(), dir.clone()), sink.clone()).unwrap();
        core.emit(AgentEvent::RunStarted {
            dialog_id: "d".into(),
            run_id: "r".into(),
        });
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "runStarted");
        assert_eq!(events[0]["dialogId"], "d");
        drop(core);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
