//! Watching what running commands actually cost.
//!
//! Twice a second, while any job has a process, the sampler reads the
//! process table (sysinfo: /proc on Linux, libproc on macOS, the toolhelp
//! snapshot on Windows) and adds up, per job, the CPU and memory of its
//! process tree: the descendants of the process it started, and on Unix
//! everything in its process group (command_tool.rs gives each command its
//! own), which also catches children that were re-parented.
//!
//! A job counts as heavy when it averaged more than 1.5 cores over three
//! seconds, holds more than 1.5 GB, or runs a compiler or bundler (then at
//! once). Then the rules in queue.rs apply:
//! - a slot whose job has been idle for 5 s is freed (a dev server that was
//!   busy starting up doesn't hold one forever);
//! - when the terminal's command turns heavy and takes a slot that was in
//!   use, the chat work holding it gives it up;
//! - chat work that's heavy without a slot gets one if it may, otherwise it
//!   runs at the lowest priority, and is paused only when memory runs out.
//!
//! Lowered priority only matters when something else wants the CPU, so it
//! isn't raised again when a slot frees up (an unprivileged process can't
//! raise its priority on Unix anyway); a pause is undone at once.

use super::machine;
use super::queue::{lock, JobKind, JobState, State, IDLE_RELEASE, SHARED};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Once;
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const INTERVAL: Duration = Duration::from_millis(500);
/// How often to sample when no chat command is running or waiting (only
/// the terminal is watched): its heaviness matters less, so cheaper.
const SLOW_INTERVAL: Duration = Duration::from_secs(2);
/// A paused job resumes after this long whatever memory says, and isn't
/// paused again for as long: a pause must not freeze it for good.
const MAX_PAUSE: Duration = Duration::from_secs(180);
/// Pausing does nothing on Windows (see `pause`): there it isn't counted.
const CAN_PAUSE: bool = cfg!(unix);
/// Averages are over this long.
const WINDOW: Duration = Duration::from_secs(3);
pub(super) const HEAVY_CORES: f64 = 1.5;
pub(super) const HEAVY_MEMORY: u64 = 1536 << 20;
/// Under this many cores on average, a job is idle.
const IDLE_CORES: f64 = 0.3;
/// Paused below this much free memory; resumed above twice that.
const LOW_MEMORY: u64 = 1 << 30;

/// Processes that only run while there's real compiling or bundling to do.
const COMPILERS: &[&str] = &[
    "rustc",
    "cc1",
    "cc1plus",
    "cc1obj",
    "clang",
    "clang++",
    "ld",
    "ld.lld",
    "lld",
    "ld64",
    "ld64.lld",
    "link",
    "javac",
    "kotlinc",
    "swiftc",
    "swift-frontend",
    "esbuild",
    "compile",
    "nvcc",
    "ghc",
];
/// Node tools that do the same, recognised by their command line.
const NODE_TOOLS: &[&str] = &[
    "tsc",
    "webpack",
    "jest",
    "vitest",
    "rollup",
    "next build",
    "vite build",
];

#[derive(Default)]
pub(super) struct Sampling {
    pub(super) samples: u32,
    /// CPU time measured so far, in milliseconds.
    pub(super) cpu_ms: u64,
    last_cpu: HashMap<u32, u64>,
    last_at: Option<Instant>,
    window: VecDeque<(Instant, f64)>,
    /// Cores in use, averaged over WINDOW.
    pub(super) cores: f64,
    pub(super) memory: u64,
    pub(super) peak_memory: u64,
    pub(super) heavy: bool,
    idle_since: Option<Instant>,
    /// The terminal's current command.
    pub(super) foreground: Option<String>,
    throttled: bool,
    paused_since: Option<Instant>,
    paused_ms: u64,
    /// Resumed after MAX_PAUSE: not paused again before this.
    no_pause_until: Option<Instant>,
    /// The tree at the last sample.
    pids: Vec<u32>,
}

impl Sampling {
    pub(super) fn idle_for(&self, at_least: Duration) -> bool {
        self.idle_since.is_some_and(|t| t.elapsed() >= at_least)
    }

    pub(super) fn paused_total(&self) -> Duration {
        let now = self.paused_since.map(|t| t.elapsed()).unwrap_or_default();
        Duration::from_millis(self.paused_ms) + now
    }
}

static STARTED: Once = Once::new();

pub(super) fn ensure_running() {
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("workload-sampler".into())
            .spawn(run);
    });
}

fn run() {
    let mut sys = System::new();
    loop {
        {
            let state = lock();
            if !state.jobs.values().any(|j| j.root_pid.is_some()) {
                let _ = SHARED.changed.wait_timeout(state, Duration::from_secs(1));
                continue;
            }
        }
        let busy = {
            let state = lock();
            state
                .jobs
                .values()
                .any(|j| (j.is_chat_work() && j.root_pid.is_some()) || j.waiting.is_some())
        };
        std::thread::sleep(if busy { INTERVAL } else { SLOW_INTERVAL });
        sample(&mut sys);
    }
}

/// What a process looks like to the rules: its name, command line.
struct Proc {
    name: String,
    cmd: String,
    cpu_ms: u64,
    memory: u64,
    start: u64,
}

fn snapshot(sys: &mut System) -> (HashMap<u32, Proc>, HashMap<u32, Vec<u32>>) {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .without_tasks(),
    );
    let mut procs = HashMap::new();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, p) in sys.processes() {
        let pid = pid.as_u32();
        if let Some(parent) = p.parent() {
            children.entry(parent.as_u32()).or_default().push(pid);
        }
        let cmd = p
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        procs.insert(
            pid,
            Proc {
                name: p.name().to_string_lossy().into_owned(),
                cmd,
                cpu_ms: p.accumulated_cpu_time(),
                memory: p.memory(),
                start: p.start_time(),
            },
        );
    }
    (procs, children)
}

/// Processes by process group, looked up once per sample for all jobs.
#[cfg(unix)]
fn groups(procs: &HashMap<u32, Proc>) -> HashMap<u32, Vec<u32>> {
    let mut out: HashMap<u32, Vec<u32>> = HashMap::new();
    for pid in procs.keys() {
        let pgid = unsafe { libc::getpgid(*pid as libc::pid_t) };
        if pgid > 0 {
            out.entry(pgid as u32).or_default().push(*pid);
        }
    }
    out
}

/// Windows has no process groups.
#[cfg(not(unix))]
fn groups(_procs: &HashMap<u32, Proc>) -> HashMap<u32, Vec<u32>> {
    HashMap::new()
}

/// `root` and everything it started; on Unix also its process group.
fn tree(
    root: u32,
    procs: &HashMap<u32, Proc>,
    children: &HashMap<u32, Vec<u32>>,
    group: Option<&HashMap<u32, Vec<u32>>>,
) -> Vec<u32> {
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    if let Some(members) = group.and_then(|g| g.get(&root)) {
        seen.extend(members.iter().copied());
    }
    seen.into_iter().filter(|p| procs.contains_key(p)).collect()
}

fn program(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_suffix(".exe")
        .map(str::to_string)
        .unwrap_or(lower)
}

fn runs_a_compiler(p: &Proc) -> bool {
    let prog = program(&p.name);
    if COMPILERS.contains(&prog.as_str()) {
        return true;
    }
    (prog == "node" || prog == "bun" || prog == "deno")
        && NODE_TOOLS.iter().any(|t| p.cmd.contains(t))
}

/// Updates one job's measurements from a new snapshot.
#[allow(clippy::too_many_arguments)]
fn measure(
    s: &mut Sampling,
    pids: Vec<u32>,
    procs: &HashMap<u32, Proc>,
    now: Instant,
    terminal: bool,
    shell_children: &[u32],
) {
    let mut delta = 0;
    let mut memory = 0;
    let mut compiler = false;
    let mut cpu = HashMap::with_capacity(pids.len());
    for pid in &pids {
        let p = &procs[pid];
        delta += p
            .cpu_ms
            .saturating_sub(s.last_cpu.get(pid).copied().unwrap_or(0));
        cpu.insert(*pid, p.cpu_ms);
        memory += p.memory;
        compiler |= runs_a_compiler(p);
    }
    let elapsed = s
        .last_at
        .map(|t| now.duration_since(t))
        .unwrap_or(INTERVAL)
        .max(Duration::from_millis(1));
    s.last_cpu = cpu;
    s.last_at = Some(now);
    s.cpu_ms += delta;
    s.samples += 1;
    s.window
        .push_back((now, delta as f64 / elapsed.as_millis() as f64));
    while s
        .window
        .front()
        .is_some_and(|(t, _)| now.duration_since(*t) > WINDOW)
    {
        s.window.pop_front();
    }
    s.cores = s.window.iter().map(|(_, c)| c).sum::<f64>() / s.window.len() as f64;
    s.memory = memory;
    s.peak_memory = s.peak_memory.max(memory);
    // Full once its oldest sample is about WINDOW old: by the spacing
    // samples actually have (2 s apart when only the terminal is watched,
    // where `WINDOW - INTERVAL` would never be reached).
    let full_window = s
        .window
        .front()
        .is_some_and(|(t, _)| now.duration_since(*t) >= WINDOW.saturating_sub(elapsed));
    // A compiler counts only while it works: a watcher (`tsc -w`, Vite's
    // esbuild service) sits idle most of the time and mustn't hold a slot.
    let compiling = compiler && s.cores >= IDLE_CORES;
    // Paused, it uses no CPU: that says nothing about the job, so its
    // state stays as it was until it runs again.
    if s.paused_since.is_none() {
        // Little CPU but much memory (a build stalled on swap) isn't idle:
        // its slot must not go to another heavy job.
        if s.cores < IDLE_CORES && memory <= HEAVY_MEMORY {
            s.idle_since.get_or_insert(now);
        } else {
            s.idle_since = None;
        }
        if compiling || memory > HEAVY_MEMORY || (full_window && s.cores >= HEAVY_CORES) {
            s.heavy = true;
        } else if s.idle_for(IDLE_RELEASE) {
            s.heavy = false;
        }
    }
    if terminal {
        // The shell's newest child is what the user ran last.
        s.foreground = shell_children
            .iter()
            .filter_map(|p| procs.get(p))
            .max_by_key(|proc| proc.start)
            .map(|proc| {
                super::short_command(if proc.cmd.is_empty() {
                    &proc.name
                } else {
                    &proc.cmd
                })
            });
    }
    s.pids = pids;
}

fn sample(sys: &mut System) {
    let (procs, children) = snapshot(sys);
    sys.refresh_memory();
    sys.refresh_cpu_usage();
    // The other windows' slots, looked at before the queue is held.
    let elsewhere = machine::held_elsewhere();
    let now = Instant::now();
    let mut state = lock();
    state.elsewhere = elsewhere;
    let mut tracked_cores = 0.0;
    let groups = state
        .jobs
        .values()
        .any(|j| j.root_pid.is_some() && j.kind != JobKind::Terminal)
        .then(|| groups(&procs));
    for job in state.jobs.values_mut() {
        let Some(root) = job.root_pid else { continue };
        let terminal = job.kind == JobKind::Terminal;
        let pids = tree(
            root,
            &procs,
            &children,
            if terminal { None } else { groups.as_ref() },
        );
        let shell_children = if terminal {
            children.get(&root).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        measure(
            &mut job.sampling,
            pids,
            &procs,
            now,
            terminal,
            &shell_children,
        );
        tracked_cores += job.sampling.cores;
    }
    if !state.fixed_machine {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let used = sys.global_cpu_usage() as f64 / 100.0 * cores as f64;
        state.machine.cores = cores;
        state.machine.total_memory = sys.total_memory();
        state.machine.free_memory = sys.available_memory();
        state.machine.untracked_load = ((used - tracked_cores) / cores as f64).max(0.0);
    }
    apply_rules(&mut state);
    drop(state);
    SHARED.changed.notify_all();
}

/// The queue's rules after a sample (see the module comment).
pub(super) fn apply_rules(state: &mut State) {
    // Idle slots are freed.
    for (id, job) in state.jobs.iter_mut() {
        if job.has_slot && job.root_pid.is_some() && job.sampling.idle_for(IDLE_RELEASE) {
            job.has_slot = false;
            machine::release(*id);
        }
    }
    // A busy terminal holds a slot in every window's count, beyond the
    // slots there are if it must; an idle one lets go of it.
    for (id, job) in state.jobs.iter() {
        if job.kind != JobKind::Terminal {
            continue;
        }
        match (job.terminal_busy(), machine::holds(*id)) {
            (true, false) => {
                machine::take(*id, machine::ANY);
            }
            (false, true) => machine::release(*id),
            _ => {}
        }
    }
    // The terminal took a slot that was in use (here or in another
    // window): chat work gives it up.
    let demote = state.holders_to_demote();
    let mut over = -state.available();
    for id in demote {
        if over <= 0 {
            break;
        }
        state.jobs.get_mut(&id).unwrap().has_slot = false;
        machine::release(id);
        over -= 1;
    }
    // Chat work that turned heavy: a slot if it may, otherwise slowed down.
    let running: Vec<u64> = state
        .jobs
        .iter()
        .filter(|(_, j)| {
            j.is_chat_work()
                && j.root_pid.is_some()
                && !j.has_slot
                && j.sampling.heavy
                && !j.sampling.idle_for(IDLE_RELEASE)
        })
        .map(|(id, _)| *id)
        .collect();
    for id in running {
        if state.may_take_slot(id) && machine::take(id, state.capacity()) {
            state.jobs.get_mut(&id).unwrap().has_slot = true;
        }
    }
    let low_memory = state.machine.free_memory > 0 && state.machine.free_memory < LOW_MEMORY;
    let memory_back = state.machine.free_memory == 0 || state.machine.free_memory > 2 * LOW_MEMORY;
    for job in state.jobs.values_mut().filter(|j| j.is_chat_work()) {
        let slow = !job.has_slot && job.sampling.heavy && !job.sampling.idle_for(IDLE_RELEASE);
        throttle(job, slow, low_memory, memory_back);
    }
}

fn throttle(job: &mut JobState, slow: bool, low_memory: bool, memory_back: bool) {
    let s = &mut job.sampling;
    if slow {
        // New processes in the tree get it too.
        for pid in &s.pids {
            set_low_priority(*pid);
        }
        s.throttled = true;
    } else {
        s.throttled = false;
    }
    let may_pause = CAN_PAUSE && s.no_pause_until.is_none_or(|t| Instant::now() >= t);
    match s.paused_since {
        None if slow
            && low_memory
            && may_pause
            && !SHUTTING_DOWN.load(std::sync::atomic::Ordering::Relaxed) =>
        {
            for pid in &s.pids {
                pause(*pid, true);
            }
            s.paused_since = Some(Instant::now());
            // Its CPU dipping before the pause doesn't count as idle.
            s.idle_since = None;
        }
        Some(since) if !slow || memory_back || since.elapsed() >= MAX_PAUSE => {
            for pid in &s.pids {
                pause(*pid, false);
            }
            if since.elapsed() >= MAX_PAUSE {
                s.no_pause_until = Some(Instant::now() + MAX_PAUSE);
            }
            s.paused_ms += since.elapsed().as_millis() as u64;
            s.paused_since = None;
            // The paused time's samples (no CPU) say nothing: start afresh.
            s.window.clear();
        }
        _ => {}
    }
}

/// Set at exit: nothing is paused any more.
static SHUTTING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// At exit: nothing stays paused (its commands run on, unwatched), and
/// nothing is paused from here on.
pub(super) fn release_all() {
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::Relaxed);
    let state = lock();
    for job in state.jobs.values() {
        release(job);
    }
}

/// A job that ended or was dropped: nothing of it stays paused.
pub(super) fn release(job: &JobState) {
    if job.sampling.paused_since.is_some() {
        for pid in &job.sampling.pids {
            pause(*pid, false);
        }
    }
}

#[cfg(unix)]
fn set_low_priority(pid: u32) {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS as _, pid as libc::id_t, 19);
    }
}

#[cfg(unix)]
fn pause(pid: u32, stop: bool) {
    unsafe {
        libc::kill(
            pid as libc::pid_t,
            if stop { libc::SIGSTOP } else { libc::SIGCONT },
        );
    }
}

#[cfg(windows)]
fn set_low_priority(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, SetPriorityClass, IDLE_PRIORITY_CLASS, PROCESS_SET_INFORMATION,
    };
    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_SET_INFORMATION, false, pid) {
            let _ = SetPriorityClass(handle, IDLE_PRIORITY_CLASS);
            let _ = CloseHandle(handle);
        }
    }
}

/// Windows has no plain way to pause a process from outside; low memory
/// there only lowers priority.
#[cfg(windows)]
fn pause(_pid: u32, _stop: bool) {}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::super::queue::{self, test_reset, test_support::SERIAL, JobKind};
    use super::*;
    #[cfg(unix)]
    use std::process::{Command, Stdio};

    #[test]
    fn a_watching_compiler_is_heavy_only_while_it_works() {
        let proc_with = |cpu_ms| Proc {
            name: "esbuild".into(),
            cmd: "esbuild --service".into(),
            cpu_ms,
            memory: 50 << 20,
            start: 1,
        };
        let t0 = Instant::now();
        // Idle: Vite's esbuild service between rebuilds.
        let mut idle = Sampling::default();
        for i in 0..10 {
            let procs = HashMap::from([(10, proc_with(5))]);
            measure(&mut idle, vec![10], &procs, t0 + INTERVAL * i, false, &[]);
        }
        assert!(!idle.heavy, "an idle watcher holds no slot");
        // Working: a core's worth of CPU every interval.
        let mut busy = Sampling::default();
        for i in 0..10 {
            let procs = HashMap::from([(10, proc_with(500 * i as u64))]);
            measure(&mut busy, vec![10], &procs, t0 + INTERVAL * i, false, &[]);
        }
        assert!(busy.heavy, "a compiler at work is heavy at once");
    }

    #[cfg(unix)]
    fn busy_loop() -> std::process::Child {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "while :; do :; done"])
            .stdout(Stdio::null())
            .process_group(0);
        cmd.spawn().unwrap()
    }

    #[cfg(unix)]
    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        for _ in 0..120 {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}");
    }

    #[cfg(unix)]
    fn sampling<T>(job_root: u32, f: impl Fn(&Sampling) -> T) -> T {
        let state = lock();
        let job = state
            .jobs
            .values()
            .find(|j| j.root_pid == Some(job_root))
            .unwrap();
        f(&job.sampling)
    }

    #[cfg(unix)]
    #[test]
    fn a_busy_loop_is_measured_and_its_tree_found() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        test_reset(8, 16 << 30, 8 << 30);
        let mut child = busy_loop();
        let pid = child.id();
        let job = queue::begin(JobKind::Agent { owner: "a".into() }, "sh -c loop", None);
        job.spawned(pid);
        wait_until("a measurement", || {
            sampling(pid, |s| s.samples >= 3 && s.cores > 0.5)
        });
        assert!(sampling(pid, |s| s.pids.contains(&pid)));
        let _ = child.kill();
        let _ = child.wait();
        drop(job);
    }

    #[cfg(unix)]
    #[test]
    fn idle_slots_are_freed_and_heavy_work_without_one_is_slowed() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        test_reset(8, 16 << 30, 8 << 30);
        // An idle command holding the only slot.
        let mut idle = Command::new("sleep").arg("30").spawn().unwrap();
        let holder = queue::begin(JobKind::Agent { owner: "a".into() }, "cargo build", None);
        assert!(holder.wait_for_slot(&|| false, &|_| {}).is_some());
        holder.spawned(idle.id());
        wait_until("the idle slot to be freed", || {
            !lock().jobs.values().any(|j| j.has_slot)
        });

        // Two busy loops, unknown to the rules: the first to turn heavy
        // takes the slot, the other is slowed down.
        let mut a = busy_loop();
        let mut b = busy_loop();
        let ja = queue::begin(JobKind::Agent { owner: "b".into() }, "./a.sh", None);
        let jb = queue::begin(JobKind::Agent { owner: "c".into() }, "./b.sh", None);
        ja.spawned(a.id());
        jb.spawned(b.id());
        // A busy loop uses one core: mark them heavy as if they used more.
        wait_until("both measured", || {
            let mut state = lock();
            for j in state.jobs.values_mut() {
                if j.sampling.samples > 0 {
                    j.sampling.heavy = true;
                }
            }
            let ready = state
                .jobs
                .values()
                .filter(|j| j.sampling.heavy && j.root_pid.is_some())
                .count()
                >= 2;
            if ready {
                apply_rules(&mut state);
            }
            ready
        });
        let state = lock();
        let holders = state.jobs.values().filter(|j| j.has_slot).count();
        let slowed = state.jobs.values().filter(|j| j.sampling.throttled).count();
        drop(state);
        assert_eq!(holders, 1);
        assert_eq!(slowed, 1);
        for c in [&mut idle, &mut a, &mut b] {
            let _ = c.kill();
            let _ = c.wait();
        }
        drop((holder, ja, jb));
    }

    #[cfg(unix)]
    #[test]
    fn a_heavy_terminal_takes_the_slot_and_never_waits() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        test_reset(8, 16 << 30, 8 << 30);
        let chat = queue::begin(JobKind::Agent { owner: "a".into() }, "cargo build", None);
        assert!(chat.wait_for_slot(&|| false, &|_| {}).is_some());
        let mut busy = busy_loop();
        let chat_pid = busy.id();
        chat.spawned(chat_pid);

        // A shell running a command, as in the terminal panel.
        let mut shell = Command::new("sh")
            .args(["-c", "yes > /dev/null; true"])
            .spawn()
            .unwrap();
        let terminal = queue::begin(JobKind::Terminal, "", None);
        terminal.spawned(shell.id());
        wait_until("the terminal to count as busy", || {
            let mut state = lock();
            for j in state.jobs.values_mut() {
                // `yes` uses one core: count it as heavy, as a build would be.
                if j.kind == JobKind::Terminal && j.sampling.foreground.is_some() {
                    j.sampling.heavy = true;
                }
            }
            let busy = state.jobs.values().any(|j| j.terminal_busy());
            if busy {
                apply_rules(&mut state);
            }
            busy
        });
        // The chat gave up its slot; a new chat command waits, naming the terminal.
        assert!(!lock().jobs.values().any(|j| j.has_slot));
        let next = queue::begin(JobKind::Agent { owner: "b".into() }, "cargo test", None);
        let notes = std::sync::Mutex::new(Vec::new());
        let gave_up = std::sync::atomic::AtomicBool::new(false);
        let waited = next.wait_for_slot(
            &|| gave_up.load(std::sync::atomic::Ordering::Relaxed),
            &|n| {
                if let Some(n) = n {
                    notes.lock().unwrap().push(n);
                    gave_up.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
        );
        assert!(
            waited.is_none(),
            "it should still be waiting when it gave up"
        );
        let notes = notes.into_inner().unwrap();
        assert!(
            notes[0].starts_with("Waiting for a CPU slot: your terminal is running `yes`"),
            "{notes:?}"
        );
        let _ = Command::new("pkill")
            .args(["-P", &shell.id().to_string()])
            .status();
        for c in [&mut busy, &mut shell] {
            let _ = c.kill();
            let _ = c.wait();
        }
        drop((chat, terminal, next));
    }
}
