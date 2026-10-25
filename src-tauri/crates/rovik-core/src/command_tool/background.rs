//! Commands the agent leaves running (bash's `background`): a dev server, a
//! watcher, anything that runs until it's stopped.
//!
//! A command run the usual way holds its call until it ends or times out,
//! and what it starts ends with it: bubblewrap ends the rest of its sandbox
//! when its shell exits, and a timeout kills its process group. So a server
//! the agent starts with `&` is gone before the user's browser asks it for
//! a page, and one run in the foreground holds the agent until it's killed.
//!
//! A background command starts as any of the chat's commands does, in its
//! sandbox and network, with the same environment (`prepared`), and its
//! call returns once it has had [`FIRST_OUTPUT`] to say how it started. A
//! thread of its own starts it and waits for it: bubblewrap's
//! `--die-with-parent` ends a sandbox with the thread that started it. The
//! latest [`KEPT`] bytes of its output (stdout and stderr, as they came)
//! are kept for the agent to read. It runs until the agent stops it, or the
//! user does in the chat's row of background tasks ([`tasks`],
//! [`stop_for_user`]), its chat goes (deleted, moved to another computer,
//! its view let go: [`stop_chat`]), or the process exits ([`stop_all`]).
//! An ended one is kept, its end and last output unread, until the agent
//! reads them. An isolated chat's changes wait to land while one runs in
//! its view, which goes with the landing (agent_run/landing.rs).

use super::{admitted, may_run_in, policy, prepared, shell_exited, ProcessTree, Queued};
use crate::overlay::confine::Confine;
use crate::workload::queue::Job;
use rovik_protocol::events::BackgroundTask;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// The most commands a chat may leave running at once.
pub const MOST_PER_CHAT: usize = 4;

/// Of a command's output, the latest this many bytes are kept.
const KEPT: usize = 64 * 1024;

/// How long a start waits for what the command prints first, unless it
/// ends sooner: a server's address, or why it couldn't start.
#[cfg(not(any(test, feature = "testing")))]
const FIRST_OUTPUT: Duration = Duration::from_secs(3);
#[cfg(any(test, feature = "testing"))]
const FIRST_OUTPUT: Duration = Duration::from_millis(500);

/// How long a stop waits for what it killed to be gone.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// How often a command's thread looks whether it ended.
const POLL: Duration = Duration::from_millis(50);

/// A background command as the agent is told of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// What the agent names it by: `bg1`, `bg2`…
    pub id: String,
    pub command: String,
    /// How long it has run, or ran.
    pub ran: Duration,
    /// How it ended; None while it runs.
    pub end: Option<End>,
}

/// How a background command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// On its own, with its exit code (None: ended by a signal).
    Exited(Option<i32>),
    /// Stopped: by the agent, as its chat went, or as the app exited.
    Stopped,
}

/// Tells the chat's windows what it has running now ([`tasks`]): called
/// once a command has started and once it has ended, however it ended,
/// one call at a time.
pub type Told = Arc<dyn Fn() + Send + Sync>;

/// A background command's status, and what it printed since the agent
/// last read its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub status: Status,
    pub output: String,
    /// Bytes of that output that weren't kept: only the latest [`KEPT`]
    /// are, whole lines.
    pub dropped: u64,
}

/// Starts `command` in `root` for `chat`, as the chat's commands start
/// (`queued`: its sandbox, network and environment, and its queue for heavy
/// work, whose wait for a slot `stop` ends), and leaves it running. How it
/// started, once it has had [`FIRST_OUTPUT`] to say: what it printed, or
/// how it ended when it did meanwhile. Why not, when it can't start: the
/// chat has [`MOST_PER_CHAT`] running already, say. `told` hears when it
/// starts running and when it ends.
pub fn start(
    chat: &str,
    root: &str,
    command: &str,
    stop: &dyn Fn() -> bool,
    queued: Queued,
    told: Told,
) -> Result<Report, String> {
    let running: Vec<String> = all()
        .iter()
        .filter(|b| b.chat == chat && b.lock().end.is_none())
        .map(|b| format!("{} (`{}`)", b.id, b.command))
        .collect();
    if running.len() >= MOST_PER_CHAT {
        return Err(format!(
            "{MOST_PER_CHAT} of this chat's commands run in the background already: {}. Stop one \
             you no longer need first.",
            running.join(", ")
        ));
    }
    let root = Path::new(root);
    may_run_in(root)?;
    let Some((job, _)) = admitted(root, command, stop, Some(&queued)) else {
        return Err("stopped by the user before it started".into());
    };
    let id = format!("bg{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let (answer, heard) = mpsc::channel();
    let (chat, root, command) = (chat.to_string(), root.to_path_buf(), command.to_string());
    thread::Builder::new()
        .name(format!("background command {id}"))
        .spawn(move || run(id, chat, root, command, queued, job, told, answer))
        .map_err(|e| format!("couldn't start a thread for the command: {e}"))?;
    heard.recv().unwrap_or_else(|_| {
        Err("the command's thread ended before it said how the command started".into())
    })
}

/// What `chat`'s background command `id` printed since the agent last read
/// it, and whether it still runs. One that ended is forgotten once this has
/// told its end.
pub fn output(chat: &str, id: &str) -> Result<Report, String> {
    let background = find(chat, id)?;
    let report = background.report();
    if report.status.end.is_some() {
        forget(&background);
    }
    Ok(report)
}

/// Stops `chat`'s background command `id`, with everything it started:
/// what it printed since the agent last read it, and how it ended.
pub fn stop(chat: &str, id: &str) -> Result<Report, String> {
    let background = find(chat, id)?;
    background.kill();
    background.watch(Some(Instant::now() + STOP_WAIT));
    forget(&background);
    Ok(background.report())
}

/// Stops `chat`'s background command `id` for the user (the Stop in the
/// chat's row of background tasks), with everything it started: whether
/// it was running. The agent hears it was stopped when it next reads it.
pub fn stop_for_user(chat: &str, id: &str) -> bool {
    let Ok(background) = find(chat, id) else {
        return false;
    };
    let running = background.lock().end.is_none();
    background.kill();
    background.watch(Some(Instant::now() + STOP_WAIT));
    running
}

/// `chat`'s commands still running, oldest first, as the chat's row of
/// background tasks shows them.
pub fn tasks(chat: &str) -> Vec<BackgroundTask> {
    all()
        .iter()
        .filter(|b| b.chat == chat && b.lock().end.is_none())
        .map(|b| BackgroundTask {
            id: b.id.clone(),
            command: b.command.clone(),
            started_at: b.started_at,
        })
        .collect()
}

/// `chat`'s background commands, oldest first: those running, and those
/// that ended without the agent told yet.
pub fn list(chat: &str) -> Vec<Status> {
    all()
        .iter()
        .filter(|b| b.chat == chat)
        .map(|b| b.status(&b.lock()))
        .collect()
}

/// Stops `chat`'s background commands and forgets them: the chat was
/// deleted or moved to another computer, or its view is going.
pub fn stop_chat(chat: &str) {
    let mine: Vec<Arc<Background>> = {
        let mut all = all();
        let (mine, others) = std::mem::take(&mut *all)
            .into_iter()
            .partition(|b| b.chat == chat);
        *all = others;
        mine
    };
    stop_together(&mine);
}

/// Stops every chat's background commands: the process is exiting.
pub fn stop_all() {
    let every = std::mem::take(&mut *all());
    stop_together(&every);
}

/// Kills them all, then waits for each to be gone.
fn stop_together(commands: &[Arc<Background>]) {
    for background in commands {
        background.kill();
    }
    let deadline = Instant::now() + STOP_WAIT;
    for background in commands {
        background.watch(Some(deadline));
    }
}

/// Every chat's background commands, running or ended but unread.
static ALL: LazyLock<Mutex<Vec<Arc<Background>>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// The number the next one's id takes.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Taken before any one command's state, never while one is held.
fn all() -> MutexGuard<'static, Vec<Arc<Background>>> {
    ALL.lock().unwrap_or_else(|e| e.into_inner())
}

fn find(chat: &str, id: &str) -> Result<Arc<Background>, String> {
    let id = id.trim();
    let all = all();
    if let Some(found) = all.iter().find(|b| b.chat == chat && b.id == id) {
        return Ok(found.clone());
    }
    let ids: Vec<&str> = all
        .iter()
        .filter(|b| b.chat == chat)
        .map(|b| b.id.as_str())
        .collect();
    Err(match ids.as_slice() {
        [] => format!("no command of this chat runs in the background, so none is {id:?}"),
        ids => format!(
            "this chat has no background command {id:?}; its background commands are {}",
            ids.join(", ")
        ),
    })
}

fn forget(background: &Arc<Background>) {
    all().retain(|b| !Arc::ptr_eq(b, background));
}

/// A background command's own thread: starts it (again while its sandbox
/// can't be made, as a foreground command is: overlay/confine.rs
/// `until_made`), answers its start with how it started once it has had
/// [`FIRST_OUTPUT`] or ended, then waits for it to end; `told` hears when
/// it runs and when it has ended. Its job in the queue for heavy work goes
/// with it.
#[allow(clippy::too_many_arguments)]
fn run(
    id: String,
    chat: String,
    root: PathBuf,
    command: String,
    queued: Queued,
    job: Option<Job>,
    told: Told,
    answer: mpsc::Sender<Result<Report, String>>,
) {
    let confine = policy(&root, Some(&queued));
    // Listed from the start, so that an exit meanwhile stops it too.
    let attempt = || -> Result<Arc<Background>, String> {
        let background = launch(
            &id,
            &chat,
            &root,
            &command,
            confine.as_ref(),
            job.as_ref(),
            &queued,
        )?;
        all().push(background.clone());
        background.watch(Some(Instant::now() + FIRST_OUTPUT));
        Ok(background)
    };
    let unmade = |b: &Arc<Background>| b.never_started(confine.as_ref());
    let started = crate::overlay::confine::until_made(attempt, unmade);
    // The tries that never started are gone: only the last is kept.
    all().retain(|b| b.id != id || started.as_ref().is_ok_and(|s| Arc::ptr_eq(b, s)));
    let background = match started {
        Ok(background) => background,
        Err(why) => {
            let _ = answer.send(Err(why));
            return;
        }
    };
    let first = background.report();
    // Ended already: its start's answer tells the agent so, and the chat
    // never had it running.
    let ended = first.status.end.is_some();
    if ended {
        forget(&background);
    }
    let _ = answer.send(Ok(first));
    if !ended {
        tell(&told);
        background.watch(None);
        tell(&told);
    }
    let ended_on_its_own = matches!(background.lock().end, Some((End::Exited(_), _)));
    if let (true, Some(job)) = (ended_on_its_own, &job) {
        job.completed();
    }
}

/// Held while a chat is told what it has running. Each telling reads the
/// whole list as it is then, so they go out in the order they read it, and
/// the last one told is what runs now.
static TELLING: Mutex<()> = Mutex::new(());

/// Tells a chat what it has running now (`told`), one telling at a time.
fn tell(told: &Told) {
    let _one_at_a_time = TELLING.lock().unwrap_or_else(|e| e.into_inner());
    told();
}

/// Starts `command` once, as `chat`'s background command `id`: its stdout
/// and stderr into one pipe, read on a thread of its own.
fn launch(
    id: &str,
    chat: &str,
    root: &Path,
    command: &str,
    confine: Option<&Confine>,
    job: Option<&Job>,
    queued: &Queued,
) -> Result<Arc<Background>, String> {
    let piped = |e: std::io::Error| format!("couldn't make a pipe for the command's output: {e}");
    let (pipe, writer) = std::io::pipe().map_err(piped)?;
    let spawned = {
        // Gone once it has started, with this end of the pipe: the
        // command's are the only ones left, and its output ends with them.
        let mut cmd = prepared(root, command, confine, job, Some(queued));
        cmd.stdout(writer.try_clone().map_err(piped)?)
            .stderr(writer);
        cmd.spawn()
    };
    let child = spawned.map_err(|e| format!("failed to start the command: {e}"))?;
    let tree = ProcessTree::of(&child);
    if let Some(job) = job {
        job.spawned(child.id());
    }
    let background = Arc::new(Background {
        id: id.to_string(),
        chat: chat.to_string(),
        command: command.to_string(),
        started: Instant::now(),
        started_at: crate::chat_db::store::now_ms(),
        state: Mutex::new(State {
            child: Some(child),
            tree,
            output: Output::default(),
            closed: false,
            stopping: false,
            end: None,
        }),
        changed: Condvar::new(),
    });
    let reading = background.clone();
    let reader = thread::Builder::new()
        .name(format!("background command {id}'s output"))
        .spawn(move || reading.read(pipe));
    if let Err(e) = reader {
        background.kill();
        background.watch(Some(Instant::now() + STOP_WAIT));
        return Err(format!("couldn't read the command's output: {e}"));
    }
    Ok(background)
}

/// A command left running, as its threads and the agent's calls share it.
struct Background {
    id: String,
    chat: String,
    command: String,
    started: Instant,
    /// When it started, by the clock: what the chat shows.
    started_at: i64,
    state: Mutex<State>,
    /// Told whenever its output grows or it ends.
    changed: Condvar,
}

struct State {
    /// Its shell, unreaped until it has ended, so that its process group
    /// stays its own to kill (`shell_exited`); None once reaped.
    child: Option<Child>,
    tree: ProcessTree,
    output: Output,
    /// Nothing holds its output's pipe any more.
    closed: bool,
    /// It was killed, and is told as stopped once its shell is gone.
    stopping: bool,
    /// How it ended, and how long it had run.
    end: Option<(End, Duration)>,
}

impl Background {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn status(&self, state: &State) -> Status {
        let (end, ran) = match state.end {
            Some((end, ran)) => (Some(end), ran),
            None => (None, self.started.elapsed()),
        };
        Status {
            id: self.id.clone(),
            command: self.command.clone(),
            ran,
            end,
        }
    }

    /// Its status, and what it printed since the last report.
    fn report(&self) -> Report {
        let mut state = self.lock();
        let (output, dropped) = state.output.take();
        Report {
            status: self.status(&state),
            output,
            dropped,
        }
    }

    /// Waits on this thread until it has ended, or `until` comes.
    fn watch(&self, until: Option<Instant>) {
        let mut state = self.lock();
        loop {
            self.look(&mut state);
            if state.end.is_some() || until.is_some_and(|until| Instant::now() >= until) {
                return;
            }
            state = self
                .changed
                .wait_timeout(state, POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Notes its end once it has come: its shell has exited and nothing
    /// holds its output any more (a server it left running still does), or
    /// its shell has exited after a kill (a process that left the group may
    /// hold the pipe for good). Its shell is reaped then.
    fn look(&self, state: &mut State) {
        let Some(child) = state.child.as_mut() else {
            return;
        };
        let exited = shell_exited(child).unwrap_or(false);
        if !exited || !(state.closed || state.stopping) {
            return;
        }
        // Reaped only now, after any kill of its group: see shell_exited.
        let code = child.wait().ok().and_then(|status| status.code());
        state.child = None;
        let end = match state.stopping {
            true => End::Stopped,
            false => End::Exited(code),
        };
        state.end = Some((end, self.started.elapsed()));
        self.changed.notify_all();
    }

    /// Kills it with everything it started (its process group; on Windows
    /// its job object): it's told as stopped once its shell is gone.
    fn kill(&self) {
        let mut state = self.lock();
        if state.end.is_some() {
            return;
        }
        state.stopping = true;
        let State { child, tree, .. } = &mut *state;
        if let Some(child) = child {
            tree.kill(child);
        }
        self.changed.notify_all();
    }

    /// Reads its output until nothing holds the pipe any more.
    fn read(&self, mut pipe: std::io::PipeReader) {
        let mut buf = [0u8; 8192];
        loop {
            let read = loop {
                match pipe.read(&mut buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    read => break read.unwrap_or(0),
                }
            };
            let mut state = self.lock();
            match read {
                0 => state.closed = true,
                n => state.output.push(&buf[..n]),
            }
            self.changed.notify_all();
            if read == 0 {
                return;
            }
        }
    }

    /// Whether it ended before it started: its sandbox couldn't be made
    /// (overlay/confine.rs `never_started`), so it may start again.
    fn never_started(&self, confine: Option<&Confine>) -> bool {
        let state = self.lock();
        let Some((End::Exited(code), _)) = state.end else {
            return false;
        };
        let printed: Vec<u8> = state.output.kept.iter().copied().collect();
        let printed = String::from_utf8_lossy(&printed);
        confine.is_some_and(|c| crate::overlay::confine::never_started(c, code, "", &printed))
    }
}

/// The latest [`KEPT`] bytes a command printed, and how far the agent has
/// read them.
#[derive(Default)]
struct Output {
    kept: VecDeque<u8>,
    /// Bytes it printed in all.
    total: u64,
    /// Bytes the agent was given, or that were dropped before it read them.
    read: u64,
}

impl Output {
    fn push(&mut self, bytes: &[u8]) {
        self.kept.extend(bytes);
        let over = self.kept.len().saturating_sub(KEPT);
        self.kept.drain(..over);
        self.total += bytes.len() as u64;
    }

    /// What was printed since the last take, as text, and how many of those
    /// bytes weren't kept. After a drop it starts at a line's start, so no
    /// line (or character) is left half there.
    fn take(&mut self) -> (String, u64) {
        let first_kept = self.total - self.kept.len() as u64;
        let from = self.read.max(first_kept);
        let mut dropped = from - self.read;
        let skip = usize::try_from(from - first_kept).unwrap_or(usize::MAX);
        let mut bytes: Vec<u8> = self.kept.iter().skip(skip).copied().collect();
        self.read = self.total;
        if dropped > 0 {
            if let Some(newline) = bytes.iter().take(4096).position(|&b| b == b'\n') {
                bytes.drain(..=newline);
                dropped += newline as u64 + 1;
            }
        }
        (String::from_utf8_lossy(&bytes).into_owned(), dropped)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::beating;
    use crate::workload::JobKind;
    use std::sync::atomic::AtomicUsize;

    /// A chat's command, as the agent's run queues it, in no isolated view.
    fn queued(chat: &str) -> Queued {
        Queued::of(
            JobKind::Agent {
                owner: chat.to_string(),
            },
            None,
        )
    }

    /// A folder for a test's commands, and its path as text.
    fn folder() -> (crate::testing::TempDir, String) {
        let dir = crate::testing::temp_dir();
        let root = dir.0.to_string_lossy().into_owned();
        (dir, root)
    }

    /// What's told of a chat's commands: nothing here.
    fn untold() -> Told {
        Arc::new(|| {})
    }

    fn started(chat: &str, root: &str, command: &str) -> Report {
        start(chat, root, command, &|| false, queued(chat), untold()).unwrap()
    }

    /// Whether the loop of [`beating`] in `root` still runs.
    fn beats(root: &str) -> bool {
        crate::testing::beats(Path::new(root))
    }

    /// `check` true within a few seconds.
    fn until(check: impl FnMut() -> bool) -> bool {
        let mut check = check;
        (0..200).any(|_| {
            let done = check();
            if !done {
                thread::sleep(Duration::from_millis(25));
            }
            done
        })
    }

    #[test]
    fn a_background_command_runs_on_after_its_start_until_it_is_stopped() {
        let (_dir, root) = folder();
        let chat = "bg-runs-on";
        let first = started(chat, &root, &beating("started"));
        assert_eq!(first.status.end, None, "{first:?}");
        assert_eq!(first.output.trim(), "started");
        // Past its start's answer, it and what it started still run.
        assert!(beats(&root));
        assert_eq!(list(chat).len(), 1);
        assert_eq!(list(chat)[0].end, None);

        let stopped = stop(chat, &first.status.id).unwrap();
        assert_eq!(stopped.status.end, Some(End::Stopped));
        assert!(!beats(&root), "what it started is stopped too");
        assert!(list(chat).is_empty(), "a stopped command is forgotten");
        assert!(stop(chat, &first.status.id).is_err());
    }

    #[test]
    fn its_output_is_read_once_as_it_comes() {
        let (_dir, root) = folder();
        let chat = "bg-output";
        let first = started(
            chat,
            &root,
            "echo one; while [ ! -e go ]; do sleep 0.05; done; echo two >&2; sleep 30",
        );
        assert_eq!(first.output.trim(), "one");
        std::fs::write(Path::new(&root).join("go"), "").unwrap();
        let id = first.status.id;
        let mut later = String::new();
        assert!(until(|| {
            later = output(chat, &id).unwrap().output;
            !later.is_empty()
        }));
        assert_eq!(
            later.trim(),
            "two",
            "stderr too, and not what was read before"
        );
        assert_eq!(output(chat, &id).unwrap().output, "", "nothing new");
        stop(chat, &id).unwrap();
    }

    #[test]
    fn a_command_that_ends_at_once_says_how_in_its_start_and_is_not_kept() {
        let (_dir, root) = folder();
        let chat = "bg-ends-at-once";
        let first = started(chat, &root, "echo 'port 3000 is taken' >&2; exit 3");
        assert_eq!(first.status.end, Some(End::Exited(Some(3))));
        assert_eq!(first.output.trim(), "port 3000 is taken");
        assert!(list(chat).is_empty());
    }

    #[test]
    fn a_command_that_ends_later_is_told_once_with_its_last_output() {
        let (_dir, root) = folder();
        let chat = "bg-ends-later";
        let first = started(
            chat,
            &root,
            "echo up; while [ ! -e go ]; do sleep 0.05; done; echo crashed; exit 2",
        );
        assert_eq!(first.status.end, None);
        std::fs::write(Path::new(&root).join("go"), "").unwrap();
        let id = first.status.id;
        assert!(until(|| list(chat)
            .first()
            .is_some_and(|s| s.end == Some(End::Exited(Some(2))))));
        let last = output(chat, &id).unwrap();
        assert_eq!(last.status.end, Some(End::Exited(Some(2))));
        assert_eq!(last.output.trim(), "crashed");
        assert!(list(chat).is_empty(), "forgotten once its end was read");
        assert!(output(chat, &id).is_err());
    }

    #[test]
    fn a_chat_leaves_a_few_commands_running_at_most_and_its_own_alone() {
        let (_dir, root) = folder();
        let (chat, other) = ("bg-most", "bg-most-other");
        for _ in 0..MOST_PER_CHAT {
            started(chat, &root, "sleep 30");
        }
        let refused =
            start(chat, &root, "sleep 30", &|| false, queued(chat), untold()).unwrap_err();
        assert!(
            refused.contains("run in the background already"),
            "{refused}"
        );
        assert!(refused.contains("`sleep 30`"), "{refused}");
        // Another chat's count is its own, and so are its commands.
        let others = started(other, &root, "sleep 30");
        assert!(stop(chat, &others.status.id).is_err());

        stop_chat(chat);
        assert!(list(chat).is_empty());
        assert_eq!(list(other).len(), 1, "another chat's go on");
        stop_chat(other);
    }

    #[test]
    fn the_chat_lists_what_runs_and_its_user_may_stop_one_for_the_agent_to_hear() {
        let (_dir, root) = folder();
        let chat = "bg-user-stop";
        let heard = Arc::new(AtomicUsize::new(0));
        let counted = heard.clone();
        let told: Told = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let command = beating("up");
        let first = start(chat, &root, &command, &|| false, queued(chat), told).unwrap();
        assert!(until(|| heard.load(Ordering::SeqCst) == 1), "told it runs");
        let listed = tasks(chat);
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (&listed[0].id, &listed[0].command),
            (&first.status.id, &command)
        );
        let now = crate::chat_db::store::now_ms();
        assert!((now - 60_000..=now).contains(&listed[0].started_at));

        assert!(stop_for_user(chat, &first.status.id));
        assert!(!beats(&root), "what it started is stopped too");
        assert!(tasks(chat).is_empty(), "no longer running");
        assert!(until(|| heard.load(Ordering::SeqCst) == 2), "told it ended");
        // The agent hears it was stopped, and then it's forgotten.
        let read = output(chat, &first.status.id).unwrap();
        assert_eq!(read.status.end, Some(End::Stopped));
        assert!(output(chat, &first.status.id).is_err());
        assert!(!stop_for_user(chat, &first.status.id), "no longer there");
    }

    /// Each telling reads the chat's whole list: one read earlier but
    /// slower to go out mustn't go out last, or the chat would show a list
    /// that's no longer so.
    #[test]
    fn what_a_chat_is_told_last_is_what_it_runs_now() {
        let (_dir, root) = folder();
        let chat = "bg-told-in-order";
        let heard: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let running = move || -> Vec<String> { tasks(chat).into_iter().map(|t| t.id).collect() };
        // The first command's first telling is held up once it has read
        // what runs, until `go`.
        let (go, held) = mpsc::channel::<()>();
        let held = Mutex::new(Some(held));
        let slow: Told = {
            let heard = heard.clone();
            Arc::new(move || {
                let now = running();
                if let Some(held) = held.lock().unwrap().take() {
                    let _ = held.recv();
                }
                heard.lock().unwrap().push(now);
            })
        };
        let quick: Told = {
            let heard = heard.clone();
            Arc::new(move || heard.lock().unwrap().push(running()))
        };
        let first = start(chat, &root, "sleep 30", &|| false, queued(chat), slow).unwrap();
        let second = start(chat, &root, "sleep 30", &|| false, queued(chat), quick).unwrap();
        // Time for the second's telling to go out first, were it not held.
        thread::sleep(POLL * 4);
        go.send(()).unwrap();
        assert!(until(|| heard.lock().unwrap().len() == 2));
        let both = vec![first.status.id, second.status.id];
        assert_eq!(
            *heard.lock().unwrap(),
            vec![both[..1].to_vec(), both.clone()]
        );
        assert_eq!(running(), both);
        stop_chat(chat);
    }

    #[test]
    fn a_command_that_ends_at_once_is_never_told_as_running() {
        let (_dir, root) = folder();
        let chat = "bg-never-ran";
        let heard = Arc::new(AtomicUsize::new(0));
        let counted = heard.clone();
        let told: Told = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let first = start(chat, &root, "exit 1", &|| false, queued(chat), told).unwrap();
        assert_eq!(first.status.end, Some(End::Exited(Some(1))));
        thread::sleep(POLL * 4);
        assert_eq!(heard.load(Ordering::SeqCst), 0);
        assert!(tasks(chat).is_empty());
    }

    #[test]
    fn a_chat_that_goes_stops_what_it_left_running() {
        let (_dir, root) = folder();
        let chat = "bg-chat-goes";
        started(chat, &root, &beating("up"));
        assert!(beats(&root));
        stop_chat(chat);
        assert!(!beats(&root));
        assert!(list(chat).is_empty());
    }

    #[test]
    fn only_the_latest_output_is_kept_from_a_whole_line() {
        let mut output = Output::default();
        output.push(b"first\n");
        assert_eq!(output.take(), ("first\n".to_string(), 0));
        let line = format!("{}\n", "x".repeat(99));
        for _ in 0..(KEPT / 100 + 50) {
            output.push(line.as_bytes());
        }
        output.push(b"last\n");
        let (text, dropped) = output.take();
        assert!(text.ends_with("last\n"));
        assert!(text.starts_with(&line), "from a whole line");
        let printed = (KEPT / 100 + 50) * 100 + 5;
        assert_eq!(dropped as usize + text.len(), printed);
        assert_eq!(output.take(), (String::new(), 0));
    }
}
