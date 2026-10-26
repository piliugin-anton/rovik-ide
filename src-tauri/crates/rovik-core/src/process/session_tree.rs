// src-tauri/crates/rovik-core/src/process/session_tree.rs
//
// What a terminal's program started, ended along with it. Closing a
// terminal hangs up on its program, as every terminal does, and most of
// what runs in it ends then: the shell passes the hangup on to its jobs,
// and the command in the foreground gets it as the shell exits. What
// doesn't end (a job run with `nohup` or `disown`ed, a program that ignores
// SIGHUP or that a tool put in a process group of its own) is asked to end,
// then killed: a terminal of the app leaves nothing running behind it, but
// what left on purpose (on Unix, into a session of its own).

#[cfg(windows)]
use super::JobObject;
use std::time::{Duration, Instant};

/// How long what the hangup ends has to end: what's left then is asked to.
#[cfg(not(any(test, feature = "testing")))]
const HANGUP_GRACE: Duration = Duration::from_millis(500);
#[cfg(any(test, feature = "testing"))]
const HANGUP_GRACE: Duration = Duration::from_millis(200);

/// How long what's asked to end has to: what's left then is killed.
#[cfg(not(any(test, feature = "testing")))]
const TERM_GRACE: Duration = Duration::from_millis(1500);
#[cfg(any(test, feature = "testing"))]
const TERM_GRACE: Duration = Duration::from_millis(300);

/// How often it's looked whether anything is left.
const POLL: Duration = Duration::from_millis(20);

/// How long a kill has to take: what's still running then (a process stuck
/// in the kernel) is left to the system.
#[cfg(unix)]
const KILL_WAIT: Duration = Duration::from_millis(500);

/// What a program that leads a session of its own started, as a terminal's
/// does (portable-pty makes it the leader of a session on its
/// pseudo-terminal), to end with it ([`SessionTree::end`]). On Unix it's
/// everything still in that session: a process that left it (`setsid`, a
/// daemon such as tmux's server) is out of reach. On Windows it's
/// everything in the job the program goes into as it starts, even once
/// their parent has exited.
pub struct SessionTree {
    /// The session, None when the program doesn't lead one of its own.
    #[cfg(unix)]
    session: Option<u32>,
    /// None where no job could be made.
    #[cfg(windows)]
    job: Option<JobObject>,
}

impl SessionTree {
    /// The tree of the program `pid`, just started: on Windows it goes into
    /// a job at once, before it has had the time to start anything outside
    /// it. The tree of a program that doesn't lead a session of its own, or
    /// of this process, holds nothing: ending it would end what isn't the
    /// program's, maybe the app.
    pub fn of(pid: u32) -> SessionTree {
        SessionTree {
            #[cfg(unix)]
            session: session_led_by(pid),
            #[cfg(windows)]
            job: job_holding(pid),
        }
    }

    /// Ends what's left of it once its program was hung up on: what the
    /// hangup didn't end within half a second is asked to end (SIGTERM;
    /// Windows has no such request), and what's still running a second
    /// and a half later is killed. Returns as soon as nothing is left (on
    /// Windows, once the kill is under way): blocks up to two seconds and
    /// a half.
    pub fn end(&self) {
        self.end_after(HANGUP_GRACE, TERM_GRACE);
    }

    fn end_after(&self, hangup_grace: Duration, term_grace: Duration) {
        #[cfg(unix)]
        if let Some(session) = self.session {
            end_session(session, hangup_grace, term_grace);
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            end_job(job, hangup_grace + term_grace);
        }
    }

    /// Whether nothing of it is still running.
    #[cfg(any(test, feature = "testing"))]
    pub fn is_empty(&self) -> bool {
        #[cfg(unix)]
        return self
            .session
            .is_none_or(|sid| in_session(&mut sysinfo::System::new(), sid).is_empty());
        #[cfg(windows)]
        return self.job.as_ref().is_none_or(JobObject::is_empty);
    }
}

/// `pid` when it leads a session of its own that isn't this process's.
#[cfg(unix)]
fn session_led_by(pid: u32) -> Option<u32> {
    let id = libc::pid_t::try_from(pid).ok().filter(|&id| id > 1)?;
    // SAFETY: getsid only reads the session ids of a process and of this one.
    let (its, ours) = unsafe { (libc::getsid(id), libc::getsid(0)) };
    (its == id && its != ours).then_some(pid)
}

/// What the hangup of session `sid` didn't end within `hangup_grace` gets
/// SIGTERM, what's still running `term_grace` later SIGKILL.
#[cfg(unix)]
fn end_session(sid: u32, hangup_grace: Duration, term_grace: Duration) {
    let mut sys = sysinfo::System::new();
    let hung_up = Instant::now();
    let left = left_by(&mut sys, sid, hung_up + hangup_grace);
    if left.is_empty() {
        return;
    }
    signal(&left, libc::SIGTERM);
    let killed = hung_up + hangup_grace + term_grace;
    let mut left = left_by(&mut sys, sid, killed);
    // Killed until gone: what it started between a look and the kill is
    // killed at the next.
    while !left.is_empty() {
        signal(&left, libc::SIGKILL);
        if Instant::now() >= killed + KILL_WAIT {
            log::warn!(
                "terminal: {} of what it ran outlived SIGKILL (pids {left:?})",
                left.len()
            );
            return;
        }
        std::thread::sleep(POLL);
        left = in_session(&mut sys, sid);
    }
}

/// What's still running in session `sid` once nothing is, or once
/// `deadline` has passed.
#[cfg(unix)]
fn left_by(sys: &mut sysinfo::System, sid: u32, deadline: Instant) -> Vec<libc::pid_t> {
    loop {
        let left = in_session(sys, sid);
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(POLL);
    }
}

/// The processes of session `sid` still running. One that has exited is a
/// zombie until its parent reaps it, and no signal can end it. Never 0 or
/// 1 (macOS lists its kernel as 0): `kill` takes 0 for this process's own
/// group.
#[cfg(unix)]
fn in_session(sys: &mut sysinfo::System, sid: u32) -> Vec<libc::pid_t> {
    use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate};
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    sys.processes()
        .iter()
        .filter(|(_, p)| !matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead))
        .filter(|(_, p)| p.session_id().map(|s| s.as_u32()) == Some(sid))
        .filter_map(|(pid, _)| libc::pid_t::try_from(pid.as_u32()).ok())
        .filter(|&pid| pid > 1)
        .collect()
}

/// Sends `signal` to each of `pids`.
#[cfg(unix)]
fn signal(pids: &[libc::pid_t], signal: libc::c_int) {
    for &pid in pids {
        // SAFETY: a signal to a process; no memory is involved.
        unsafe { libc::kill(pid, signal) };
    }
}

/// A new job holding the program `pid`; None for this process, whose job
/// would end the app.
#[cfg(windows)]
fn job_holding(pid: u32) -> Option<JobObject> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
    if pid == std::process::id() {
        return None;
    }
    // SAFETY: a handle of our own, closed once below. The program was just
    // started, and what started it holds a handle to it, so `pid` is still
    // its id and no other process's.
    unsafe {
        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid).ok()?;
        let job = JobObject::of(process);
        let _ = CloseHandle(process);
        job
    }
}

/// What's still in `job` once `grace` has passed is killed. Windows has no
/// request to end that SIGTERM is: the hangup asked, closing the console.
#[cfg(windows)]
fn end_job(job: &JobObject, grace: Duration) {
    let deadline = Instant::now() + grace;
    while !job.is_empty() && Instant::now() < deadline {
        std::thread::sleep(POLL);
    }
    if !job.is_empty() && !job.kill() {
        log::warn!("terminal: what it ran couldn't be ended (its job object refused)");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::temp_dir;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    /// `script` run by bash as the leader of a session of its own, as a
    /// terminal's shell is, with `arg` its `$1`; and its tree.
    fn leader(script: &str, arg: &Path) -> (Child, SessionTree) {
        let mut cmd = Command::new("bash");
        crate::process::own_session(&mut cmd)
            .arg("-c")
            .arg(script)
            .arg("bash")
            .arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().unwrap();
        let tree = SessionTree::of(child.id());
        (child, tree)
    }

    /// The pids `file` lists, once the script has written `n` of them.
    fn pids_in(file: &Path, n: usize) -> Vec<libc::pid_t> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            let pids: Vec<libc::pid_t> = text.lines().filter_map(|l| l.parse().ok()).collect();
            if pids.len() >= n {
                return pids;
            }
            assert!(Instant::now() < deadline, "the script wrote {text:?}");
            std::thread::sleep(POLL);
        }
    }

    /// The hangup a closed terminal gives its program.
    fn hang_up(leader: &Child) {
        // SAFETY: a signal to a process; no memory is involved.
        unsafe { libc::kill(leader.id() as libc::pid_t, libc::SIGHUP) };
    }

    /// What the hangup doesn't end ends with the tree: a job that ignores
    /// SIGHUP as `nohup` makes it, one that ignores SIGTERM too, and one in
    /// a process group of its own, which ending a group would miss.
    #[test]
    fn what_a_terminal_left_running_ends_with_it() {
        let dir = temp_dir();
        let pids = dir.0.join("pids");
        let script = r#"
            trap '' HUP
            sleep 60 & echo $! >> "$1"
            (trap '' TERM; exec sleep 60) & echo $! >> "$1"
            set -m
            sleep 60 & echo $! >> "$1"
            wait
        "#;
        let (mut leader, tree) = leader(script, &pids);
        let started = pids_in(&pids, 3);
        // SAFETY: getpgid only reads the process groups of two processes.
        let (own, leaders) = unsafe { (libc::getpgid(started[2]), libc::getpgid(0)) };
        assert_eq!(own, started[2], "the last job leads a group of its own");
        assert_ne!(own, leaders);
        assert!(!tree.is_empty());

        hang_up(&leader);
        tree.end();
        assert!(tree.is_empty(), "nothing of the session is left");
        let _ = leader.wait();
    }

    /// What's left once the hangup's grace is over is asked to end before
    /// it's killed: a job that ignores SIGHUP but handles SIGTERM gets to.
    #[test]
    fn what_ignores_the_hangup_is_asked_to_end_before_it_is_killed() {
        let dir = temp_dir();
        let asked = dir.0.join("asked");
        let script = r#"
            trap '' HUP
            (trap 'echo asked > "$1"; exit' TERM; sleep 60 & echo $! > "$1.ready"; wait) &
            wait
        "#;
        let (mut leader, tree) = leader(script, &asked);
        pids_in(&dir.0.join("asked.ready"), 1);

        hang_up(&leader);
        // A kill that would come too late to race the job's answer.
        tree.end_after(HANGUP_GRACE, Duration::from_secs(60));
        assert!(tree.is_empty());
        let said = std::fs::read_to_string(&asked).unwrap_or_default();
        assert_eq!(said.trim(), "asked", "it was asked (SIGTERM), not killed");
        let _ = leader.wait();
    }

    /// When the hangup ended everything, the tree's end waits for no
    /// grace: closing a terminal or quitting isn't held up for nothing.
    #[test]
    fn a_tree_the_hangup_ended_ends_at_once() {
        let (mut leader, tree) = leader("exec sleep 60", Path::new(""));
        hang_up(&leader);
        let started = Instant::now();
        tree.end_after(Duration::from_secs(60), Duration::from_secs(60));
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(tree.is_empty());
        let _ = leader.wait();
    }

    /// Only a session a program leads, and not this process's, is a tree:
    /// the tree of a program in this process's session, of this process, or
    /// of no process ends nothing.
    #[test]
    fn a_tree_ends_nothing_but_the_session_its_program_leads() {
        let mut ours = Command::new("sleep").arg("60").spawn().unwrap();
        let tree = SessionTree::of(ours.id());
        assert!(tree.session.is_none());
        tree.end_after(Duration::ZERO, Duration::ZERO);
        assert!(ours.try_wait().unwrap().is_none(), "it still runs");
        ours.kill().unwrap();
        ours.wait().unwrap();

        for pid in [std::process::id(), 0, 1] {
            assert!(SessionTree::of(pid).session.is_none(), "{pid}");
        }
    }

    /// A process that left the session for one of its own (`setsid`, a
    /// daemon such as tmux's server) is out of the tree's reach.
    #[cfg(target_os = "linux")]
    #[test]
    fn what_left_the_session_is_out_of_reach() {
        let dir = temp_dir();
        let pids = dir.0.join("pids");
        let (mut leader, tree) = leader(r#"setsid sleep 60 & echo $! >> "$1"; wait"#, &pids);
        let away = pids_in(&pids, 1)[0];

        hang_up(&leader);
        tree.end();
        let still = !in_session(&mut sysinfo::System::new(), away as u32).is_empty();
        // SAFETY: a signal to a process; no memory is involved.
        unsafe { libc::kill(away, libc::SIGKILL) };
        assert!(still, "it runs on, in its own session");
        let _ = leader.wait();
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    /// What a program left running ends with its tree: cmd starts a ping
    /// that outlives it, in its job.
    #[test]
    fn what_a_terminal_left_running_ends_with_it() {
        let mut cmd = Command::new("cmd");
        // The first ping gives the tree the time to put cmd in its job
        // before cmd starts the one that outlives it.
        cmd.raw_arg("/d /c ping -n 2 127.0.0.1 >nul & start /b ping -n 60 127.0.0.1 >nul")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap();
        let tree = SessionTree::of(child.id());
        child.wait().unwrap();
        assert!(!tree.is_empty(), "the ping outlived cmd, in its job");

        tree.end_after(Duration::from_millis(100), Duration::from_millis(100));
        // Terminated, and gone moments later.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !tree.is_empty() && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        assert!(tree.is_empty());
    }

    /// This process never goes into a tree's job: ending it would end the
    /// app.
    #[test]
    fn a_tree_of_this_process_holds_nothing() {
        assert!(SessionTree::of(std::process::id()).job.is_none());
    }
}
