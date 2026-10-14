//! Slots for heavy work.
//!
//! Every command the app runs for a chat (the agent's `bash`, the checks
//! after a run) and the terminal panel's shell is a [`Job`] here. A command
//! predicted heavy waits in [`Job::wait_for_slot`] before it starts; the
//! wait shows in the chat as a note naming what it waits for. Everything
//! else starts at once and is watched (tracker.rs), which also frees slots
//! whose command went idle and slows down commands that turned heavy
//! without one.
//!
//! Order: the chat the user is looking at first, then the others; first
//! come, first served within each. The terminal never waits: when its
//! command turns heavy it takes a slot even if all are in use, and the chat
//! work holding one is slowed down instead.
//!
//! The app runs as a single instance (tauri-plugin-single-instance), so the
//! slots live in this process; no lock files are needed between windows.

use super::classify::{classify, Class};
use super::history;
use super::tracker::{self, Sampling};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Machines that get a second slot: at least this many cores and this much
/// memory, and only while they have room (`ROOM_*`).
const BIG_CORES: usize = 16;
const BIG_MEMORY: u64 = 32 << 30;
const ROOM_FREE_MEMORY: u64 = 8 << 30;
const ROOM_UNTRACKED_LOAD: f64 = 0.5;
/// Memory kept free beyond a heavy job's usual peak before it starts.
const MEMORY_SPARE: u64 = 1 << 30;
/// A slot whose command has been idle this long is freed.
pub(super) const IDLE_RELEASE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Checks of a chat's changes before they land in the project.
    Landing,
    /// The chat the user is looking at.
    Watched,
    Background,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobKind {
    /// A chat's command (`bash`), by dialog id.
    Agent { owner: String },
    /// The checks after a chat's run (verify.rs).
    Check { owner: String },
    /// Checks before a chat's changes land.
    Landing { owner: String },
    /// The terminal panel's shell. Never waits, never slowed.
    Terminal,
}

impl JobKind {
    fn owner(&self) -> Option<&str> {
        match self {
            JobKind::Agent { owner } | JobKind::Check { owner } | JobKind::Landing { owner } => {
                Some(owner)
            }
            JobKind::Terminal => None,
        }
    }
}

pub(super) struct JobState {
    pub(super) kind: JobKind,
    pub(super) label: String,
    pub(super) class: Class,
    fingerprint: String,
    project: Option<String>,
    pub(super) root_pid: Option<u32>,
    started: Option<Instant>,
    /// The usual peak memory of past runs (history.rs), if known.
    expected_peak: u64,
    pub(super) has_slot: bool,
    /// Waiting for a slot: its place in line.
    pub(super) waiting: Option<u64>,
    pub(super) sampling: Sampling,
    /// Exited on its own: a run worth remembering (not stopped or timed out).
    completed: bool,
}

impl JobState {
    fn priority(&self, focus: Option<&str>) -> Priority {
        match &self.kind {
            JobKind::Landing { .. } => Priority::Landing,
            kind if kind.owner().is_some() && kind.owner() == focus => Priority::Watched,
            _ => Priority::Background,
        }
    }

    /// The terminal counts as a used slot while its command is heavy.
    pub(super) fn terminal_busy(&self) -> bool {
        self.kind == JobKind::Terminal
            && self.sampling.heavy
            && !self.sampling.idle_for(IDLE_RELEASE)
    }

    pub(super) fn is_chat_work(&self) -> bool {
        self.kind != JobKind::Terminal
    }
}

#[derive(Default)]
pub(super) struct Machine {
    pub(super) cores: usize,
    pub(super) total_memory: u64,
    pub(super) free_memory: u64,
    /// CPU in use by processes no job tracks, as a share of all cores.
    pub(super) untracked_load: f64,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) jobs: HashMap<u64, JobState>,
    next_id: u64,
    next_seq: u64,
    focus: Option<String>,
    manual_slots: Option<usize>,
    pub(super) machine: Machine,
    /// Tests set the machine; the sampler then leaves it alone.
    pub(super) fixed_machine: bool,
}

pub(super) struct Shared {
    pub(super) state: Mutex<State>,
    pub(super) changed: Condvar,
}

pub(super) static SHARED: LazyLock<Shared> = LazyLock::new(|| Shared {
    state: Mutex::new(State::default()),
    changed: Condvar::new(),
});

pub(super) fn lock() -> MutexGuard<'static, State> {
    SHARED.state.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    pub(super) fn capacity(&self) -> usize {
        if let Some(n) = self.manual_slots {
            return n.max(1);
        }
        let m = &self.machine;
        let big = m.cores >= BIG_CORES && m.total_memory >= BIG_MEMORY;
        let room = m.free_memory > ROOM_FREE_MEMORY && m.untracked_load < ROOM_UNTRACKED_LOAD;
        if big && room {
            2
        } else {
            1
        }
    }

    /// Free slots; negative when the terminal took one that was in use.
    pub(super) fn available(&self) -> isize {
        let used = self
            .jobs
            .values()
            .filter(|j| j.has_slot || j.terminal_busy())
            .count();
        self.capacity() as isize - used as isize
    }

    pub(super) fn priority(&self, id: u64) -> Priority {
        self.jobs[&id].priority(self.focus.as_deref())
    }

    /// Whether `id` is first in line: no waiting job comes before it.
    pub(super) fn first_in_line(&self, id: u64, seq: u64) -> bool {
        let mine = (self.priority(id), seq);
        !self.jobs.iter().any(|(other, j)| {
            *other != id && j.waiting.is_some_and(|s| (self.priority(*other), s) < mine)
        })
    }

    /// Whether a job that isn't waiting (it's running and turned heavy)
    /// may take a free slot: only if nobody of its priority or better waits.
    pub(super) fn may_take_slot(&self, id: u64) -> bool {
        let mine = self.priority(id);
        self.available() > 0
            && !self.jobs.iter().any(|(other, j)| {
                *other != id && j.waiting.is_some() && self.priority(*other) <= mine
            })
    }

    /// Chat work holding slots, the one to give one up first first: the
    /// lowest priority, the latest started.
    pub(super) fn holders_to_demote(&self) -> Vec<u64> {
        let mut holders: Vec<(Priority, Option<Instant>, u64)> = self
            .jobs
            .iter()
            .filter(|(_, j)| j.has_slot && j.is_chat_work())
            .map(|(id, j)| (self.priority(*id), j.started, *id))
            .collect();
        holders.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        holders.into_iter().map(|(_, _, id)| id).collect()
    }

    /// Memory admission: a heavy job starts only if its usual peak (from
    /// past runs) fits in free memory with 1 GB to spare, unless nothing
    /// else heavy runs (then waiting wouldn't free any).
    fn memory_allows(&self, id: u64) -> bool {
        let need = self.jobs[&id].expected_peak;
        let free = self.machine.free_memory;
        let others_heavy = self
            .jobs
            .iter()
            .any(|(o, j)| *o != id && (j.has_slot || j.terminal_busy()));
        need == 0 || free == 0 || !others_heavy || free >= need + MEMORY_SPARE
    }

    /// What `id` waits for, in words: "your terminal is running `cargo build`".
    fn blocker(&self, id: u64) -> String {
        let me = self.jobs[&id].kind.owner().map(str::to_string);
        if let Some(t) = self.jobs.values().find(|j| j.terminal_busy()) {
            return match &t.sampling.foreground {
                Some(cmd) => format!("your terminal is running `{cmd}`"),
                None => "your terminal is busy".to_string(),
            };
        }
        if let Some(h) = self.jobs.values().find(|j| j.has_slot) {
            let same = h.kind.owner().map(str::to_string) == me;
            let label = super::short_command(&h.label);
            return match (&h.kind, same) {
                (JobKind::Check { .. }, true) => {
                    format!("this chat's checks are running `{label}`")
                }
                (JobKind::Check { .. }, false) => {
                    format!("another chat's checks are running `{label}`")
                }
                (JobKind::Landing { .. }, _) => {
                    format!("a chat's changes are being checked before landing (`{label}`)")
                }
                (_, true) => format!("this chat is running `{label}`"),
                (_, false) => format!("another chat is running `{label}`"),
            };
        }
        if !self.memory_allows(id) {
            return "not enough free memory".to_string();
        }
        "another chat's command is ahead in the queue".to_string()
    }
}

/// How long a command waited for a slot, and for what.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Waited {
    pub ms: u64,
    /// What it waited for, last: "your terminal is running `cargo build`".
    pub reason: Option<String>,
}

impl Waited {
    /// For the model, and kept in the chat, when the wait was long enough
    /// to matter: "Waited 1 min 20 s for a CPU slot (your terminal is
    /// running `cargo build`)."
    pub fn describe(&self) -> Option<String> {
        if self.ms < 10_000 {
            return None;
        }
        let what = self
            .reason
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        Some(format!(
            "Waited {} for a CPU slot{what}.",
            super::format_duration(self.ms)
        ))
    }
}

/// A command (or the terminal's shell) known to the queue, from before it
/// starts until it ends. Dropping it frees its slot and records what it
/// cost (history.rs).
pub struct Job {
    id: u64,
}

/// Registers a command. `project`: where it runs, for its class and history.
pub fn begin(kind: JobKind, command: &str, project: Option<&Path>) -> Job {
    let fingerprint = history::fingerprint(command);
    // An isolated chat runs in its view: history belongs to the project.
    let project_key = project.map(|p| {
        crate::overlay::chats::project_for(p)
            .to_string_lossy()
            .into_owned()
    });
    let mut class = classify(command, project);
    let mut expected_peak = 0;
    if kind == JobKind::Terminal {
        class = Class::LongRunning;
    } else if let Some(p) = project_key
        .as_deref()
        .and_then(|p| history::predict(p, &fingerprint))
    {
        // Past runs know better than the rules; a server stays a server.
        if class != Class::LongRunning {
            class = p.class;
        }
        expected_peak = p.peak_memory;
    }
    let mut state = lock();
    state.next_id += 1;
    let id = state.next_id;
    state.jobs.insert(
        id,
        JobState {
            kind,
            label: command.to_string(),
            class,
            fingerprint,
            project: project_key,
            root_pid: None,
            started: None,
            expected_peak,
            has_slot: false,
            completed: false,
            waiting: None,
            sampling: Sampling::default(),
        },
    );
    Job { id }
}

pub fn set_focus(dialog_id: Option<String>) {
    lock().focus = dialog_id;
    SHARED.changed.notify_all();
}

pub fn set_manual_slots(slots: Option<usize>) {
    lock().manual_slots = slots;
    SHARED.changed.notify_all();
}

/// The slots now, for the settings page.
pub struct Slots {
    /// As the user set them; None: auto.
    pub manual: Option<usize>,
    /// What auto gives on this machine right now.
    pub auto: usize,
    /// Commands holding a slot, and heavy ones waiting for one.
    pub busy: usize,
    pub waiting: usize,
    /// The machine, as last sampled (0: not yet).
    pub cores: usize,
    pub total_memory: u64,
}

pub fn slots() -> Slots {
    let state = lock();
    if state.machine.cores > 0 {
        let m = &state.machine;
        let reading = (m.cores, m.total_memory, m.free_memory, m.untracked_load);
        return slots_with(&state, reading);
    }
    // Not sampled yet (no command has run): read the machine for this page
    // only, so auto isn't judged from an all-zero machine. It isn't kept:
    // the queue judges by its own samples.
    drop(state);
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    slots_with(
        &lock(),
        (cores, sys.total_memory(), sys.available_memory(), 0.0),
    )
}

fn slots_with(
    state: &State,
    (cores, total_memory, free_memory, untracked_load): (usize, u64, u64, f64),
) -> Slots {
    let big = cores >= BIG_CORES && total_memory >= BIG_MEMORY;
    let room = free_memory > ROOM_FREE_MEMORY && untracked_load < ROOM_UNTRACKED_LOAD;
    Slots {
        manual: state.manual_slots,
        auto: if big && room { 2 } else { 1 },
        busy: state
            .jobs
            .values()
            .filter(|j| j.has_slot || j.terminal_busy())
            .count(),
        waiting: state.jobs.values().filter(|j| j.waiting.is_some()).count(),
        cores,
        total_memory,
    }
}

impl Job {
    /// Waits until a slot is free when the command is predicted heavy;
    /// returns at once otherwise. `note` gets what it waits for while it
    /// waits (and `None` when it's done); `stop` gives up (`None`).
    pub fn wait_for_slot(
        &self,
        stop: &dyn Fn() -> bool,
        note: &dyn Fn(Option<String>),
    ) -> Option<Waited> {
        let started = Instant::now();
        let mut state = lock();
        if state
            .jobs
            .get(&self.id)
            .is_none_or(|j| j.class != Class::Heavy)
        {
            return Some(Waited::default());
        }
        tracker::ensure_running();
        state.next_seq += 1;
        let seq = state.next_seq;
        if let Some(job) = state.jobs.get_mut(&self.id) {
            job.waiting = Some(seq);
        }
        let mut last_note: Option<(String, u64)> = None;
        let mut reason = None;
        loop {
            if !state.jobs.contains_key(&self.id) {
                return Some(Waited::default());
            }
            if stop() {
                if let Some(job) = state.jobs.get_mut(&self.id) {
                    job.waiting = None;
                }
                drop(state);
                SHARED.changed.notify_all();
                note(None);
                return None;
            }
            if state.available() > 0
                && state.first_in_line(self.id, seq)
                && state.memory_allows(self.id)
            {
                let job = state.jobs.get_mut(&self.id).unwrap();
                job.waiting = None;
                job.has_slot = true;
                drop(state);
                SHARED.changed.notify_all();
                if last_note.is_some() {
                    note(None);
                }
                return Some(Waited {
                    ms: started.elapsed().as_millis() as u64,
                    reason,
                });
            }
            // Brief waits go unmentioned.
            let waited = started.elapsed().as_millis() as u64;
            if waited >= 500 {
                let why = state.blocker(self.id);
                let secs = waited / 1000;
                if last_note
                    .as_ref()
                    .is_none_or(|(w, s)| *w != why || *s != secs)
                {
                    last_note = Some((why.clone(), secs));
                    reason = Some(why.clone());
                    // Told outside the lock: the note reaches the UI.
                    drop(state);
                    note(Some(format!(
                        "Waiting for a CPU slot: {why} ({})",
                        super::format_duration(waited)
                    )));
                    state = lock();
                    continue;
                }
                reason = Some(why);
            }
            state = SHARED
                .changed
                .wait_timeout(state, Duration::from_millis(250))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// The command exited on its own: its cost is remembered.
    pub fn completed(&self) {
        if let Some(job) = lock().jobs.get_mut(&self.id) {
            job.completed = true;
        }
    }

    /// The command started: its process tree is watched from now on.
    pub fn spawned(&self, pid: u32) {
        {
            let mut state = lock();
            if let Some(job) = state.jobs.get_mut(&self.id) {
                job.root_pid = Some(pid);
                job.started = Some(Instant::now());
            }
        }
        tracker::ensure_running();
    }

    /// Time the command spent paused for lack of memory, which doesn't count
    /// toward its timeout.
    pub fn paused(&self) -> Duration {
        lock()
            .jobs
            .get(&self.id)
            .map(|j| j.sampling.paused_total())
            .unwrap_or_default()
    }

    /// Variables that split the cores between heavy jobs, when more than
    /// one slot runs at a time.
    pub fn cpu_split_env(&self) -> Vec<(&'static str, String)> {
        let state = lock();
        let holders = state.jobs.values().filter(|j| j.has_slot).count().max(1);
        if state.capacity() < 2 || holders < 2 {
            return Vec::new();
        }
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        let n = (cores / holders).max(1).to_string();
        vec![
            ("CARGO_BUILD_JOBS", n.clone()),
            ("MAKEFLAGS", format!("-j{n}")),
            ("GOMAXPROCS", n),
        ]
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let finished = lock().jobs.remove(&self.id);
        SHARED.changed.notify_all();
        let Some(job) = finished else { return };
        // Undo a pause or lowered priority on whatever is still running.
        tracker::release(&job);
        if !job.is_chat_work() {
            return;
        }
        if !job.completed {
            return;
        }
        if let (Some(project), Some(started)) = (job.project, job.started) {
            let sample = history::Sample {
                cpu_ms: job.sampling.cpu_ms,
                peak_memory: job.sampling.peak_memory,
                wall_ms: started.elapsed().as_millis() as u64,
            };
            // A command that ended before it was sampled says nothing.
            if job.sampling.samples > 0 {
                history::record(&project, &job.fingerprint, sample);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn test_reset(cores: usize, total_memory: u64, free_memory: u64) {
    let mut s = lock();
    s.jobs.clear();
    s.focus = None;
    s.manual_slots = None;
    s.machine = Machine {
        cores,
        total_memory,
        free_memory,
        untracked_load: 0.0,
    };
    s.fixed_machine = true;
}

#[cfg(test)]
pub(crate) mod test_support {
    /// Tests share the global queue: one at a time.
    pub static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
