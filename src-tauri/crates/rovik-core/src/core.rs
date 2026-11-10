//! What the core needs from the process hosting it, whichever that is: the
//! app, the headless runner, and later the daemon. The host builds one
//! `Core` and the core's code takes it in place of Tauri's `AppHandle`
//! (docs/design/cloud-agent-dev-plan.md, D2).

use crate::agent_run::events::AgentEvent;
use crate::agent_run::AgentState;
use crate::browser_tools::BrowserState;
use crate::chat_db::DbState;
use crate::mcp::McpState;
use crate::memory::MemoryState;
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
    /// The user's MCP servers (crate::mcp).
    pub mcp: McpState,
    /// The projects' long-term memory, one store per project (crate::memory).
    pub memory: MemoryState,
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
            mcp: McpState::default(),
            memory: MemoryState::default(),
            events,
        })))
    }

    /// Tells the host's frontends what a run did.
    pub fn emit(&self, event: AgentEvent) {
        self.events.agent(&event);
    }

    /// Why an automatic host replacement must retain this core. The caller must
    /// first prevent new requests and remote producers from starting work.
    pub fn replacement_blocker(&self) -> Option<&'static str> {
        self.replacement_blocker_inner(false)
    }

    /// Drains unused Linux virtual and top-level chat volumes for an upgrade.
    /// The host must exclude clients and pause producers throughout this call.
    /// A busy mount or any retained work leaves the existing owner in place.
    #[cfg(target_os = "linux")]
    pub fn prepare_replacement(&self) -> Result<(), String> {
        if let Some(reason) = self.replacement_blocker_inner(true) {
            return Err(reason.into());
        }
        self.vfs.unmount_if_unused()?;
        crate::overlay::chats::drain_for_upgrade()?;
        // A kernel writer could have created a file between the empty check
        // and unmount. Keep those bytes in this host even after detachment.
        self.replacement_blocker()
            .map_or(Ok(()), |reason| Err(reason.into()))
    }

    fn replacement_blocker_inner(&self, drain_mount: bool) -> Option<&'static str> {
        let activity = crate::rt::activity_epoch();
        if crate::rt::has_blocking_work() {
            return Some("Blocking operations are still finishing");
        }
        if crate::memory_semantic::active() {
            return Some("Memory indexing or model preparation is still running");
        }
        if crate::connectors::setup_active() {
            return Some("Connector setup is still running");
        }
        let status = crate::agent_run::agent_status(self);
        if !crate::chat_hold::held_here().is_empty()
            || !status.runs.is_empty()
            || !status.subagents.is_empty()
            || !status.driving.is_empty()
            || !status.approvals.is_empty()
            || !status.questions.is_empty()
        {
            return Some("Chats are still at work or waiting for input");
        }
        if crate::command_tool::background::retains_work() {
            return Some("Background commands or unread command results remain");
        }
        if !drain_mount && crate::overlay::chats::retains_views() {
            return Some("Chat filesystem views are still open");
        }
        if !drain_mount && self.vfs.mountpoint().is_some() {
            return Some("The virtual filesystem is still mounted");
        }
        match self.vfs.list("") {
            Ok(entries) if entries.is_empty() => {
                if activity == crate::rt::activity_epoch() {
                    None
                } else {
                    Some("Background activity changed while checking; retry the upgrade")
                }
            }
            _ => Some("Virtual files remain in memory or their state could not be checked"),
        }
    }

    /// As the process exits: no headless Chrome, MCP server or command the
    /// agent left running, no dead mount where the virtual files were,
    /// paused commands resumed and the workload's history written, and the
    /// database marked closed cleanly.
    pub async fn shutdown(&self) {
        crate::connectors::end_all();
        self.browser.shutdown().await;
        self.mcp.shutdown().await;
        // Before the chats' views go: they may run in them.
        let _ = crate::rt::spawn_blocking(crate::command_tool::background::stop_all).await;
        let _ = self.vfs.unmount();
        crate::overlay::chats::close_all();
        crate::workload::shutdown();
        // Detached writes share the same queue. Flush accepted persistence
        // before a retiring host releases its lifetime ownership lock.
        match self.db.write(|_| Ok(())).await {
            Ok(()) => self.db.mark_clean_exit(),
            Err(error) => log::error!("Could not flush chat persistence at shutdown: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{temp_dir, Recorder, TestCore};

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
        let dir = temp_dir();
        let data = dir.0.join("data");
        let paths = Paths::new(data.clone(), dir.0.join("config"));
        let core = Core::new(paths, Arc::new(Recorder(Default::default()))).unwrap();
        assert!(data.join("chats.db").is_file());
        assert_eq!(core.replacement_blocker(), None);
        let hold = crate::chat_hold::take("upgrade-held").unwrap();
        assert!(core.replacement_blocker().unwrap().contains("Chats"));
        #[cfg(target_os = "linux")]
        assert!(core.prepare_replacement().unwrap_err().contains("Chats"));
        drop(hold);
        core.vfs.write_unmounted("kept.txt", b"unsaved").unwrap();
        assert!(core
            .replacement_blocker()
            .unwrap()
            .contains("Virtual files"));
        #[cfg(target_os = "linux")]
        assert!(core
            .prepare_replacement()
            .unwrap_err()
            .contains("Virtual files"));
        core.vfs.remove("kept.txt").unwrap();
        assert_eq!(core.replacement_blocker(), None);

        let (release, blocked) = std::sync::mpsc::channel();
        let task = crate::rt::spawn_blocking(move || blocked.recv().unwrap());
        drop(task); // Losing the caller must not lose the upgrade blocker.
        assert!(core.replacement_blocker().unwrap().contains("Blocking"));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while crate::rt::has_blocking_work() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        core.vfs.pretend_mounted(dir.0.join("mounted"));
        assert!(core.replacement_blocker().unwrap().contains("mounted"));
        assert_eq!(crate::overlay::chats::data_dir(), Some(data.clone()));
        assert_eq!(core.paths.log_dir, data);
        let marker = || std::fs::read_to_string(data.join("chats.db.running")).unwrap();
        assert_eq!(marker(), "running");
        let (release, blocked) = std::sync::mpsc::channel();
        core.db.write_detached(move |_| {
            blocked.recv().unwrap();
            Ok(())
        });
        let closing = core.clone();
        let shutdown = crate::rt::spawn(async move { closing.shutdown().await });
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            !shutdown.is_finished(),
            "shutdown must join accepted database writes"
        );
        assert_eq!(marker(), "running");
        release.send(()).unwrap();
        crate::rt::block_on(shutdown).unwrap();
        assert_eq!(marker(), "clean", "a clean exit");
    }

    #[test]
    fn events_go_to_the_sink() {
        let test = TestCore::new();
        test.core.emit(AgentEvent::RunStarted {
            dialog_id: "d".into(),
            run_id: "r".into(),
            steerable: false,
        });
        let events = test.events.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "runStarted");
        assert_eq!(events[0]["dialogId"], "d");
    }
}
