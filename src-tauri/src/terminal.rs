// Terminal backend: any number of shells, each on its own pseudo-terminal:
// the user's shell on this computer, or theirs on a cloud host (a remote
// terminal).
//
// `portable-pty` wraps ConPTY on Windows and a real pty on macOS/Linux, so
// the same code spawns a working interactive shell everywhere.
//
// Each terminal is addressed by an id. Its output and its exit are streamed
// to the frontend over a Tauri `Channel` handed in at spawn time, not over
// global events:
//   - nothing is lost before the frontend subscribes (the shell's first
//     prompt used to race the `listen()` call after `spawn_terminal`);
//   - each terminal's output goes to its own view only, instead of every
//     view receiving every terminal's events and filtering by id.
//
// Per terminal there are two plain OS threads:
//   - a reader that pumps pty output to the channel. A blocking read must
//     never run on the async runtime: one terminal used to pin one tokio
//     worker for its whole lifetime, and a few terminals starved everything
//     else on the runtime (file watcher, chat requests);
//   - a waiter that blocks on the child, reaps it (no zombies), drops the
//     pty (which on Windows is what lets the reader see EOF), and sends the
//     exit code once the reader has drained the remaining output.
//
// A remote terminal reaches its host as the host's settings say
// (`HostsPage::shell_access`). The user's own ssh runs on a pseudo-terminal
// here, as a shell does. Rovik's SSH makes a connection of its own, with
// the login shell on a pseudo-terminal there (rovik-client's `Ssh::shell`);
// its reads are async, so a task pumps its output, and no thread waits.
// Through Rovik's SSH and in WSL, a bash 4.4 to 5.4 starts as it does here,
// with the file of ours it reads in place of a login shell's (`bash`).
//
// Closing a terminal, or quitting the app, hangs up on its program, as any
// terminal does, and then ends what the program left running: whatever is
// still in its session (its job on Windows) once the hangup has had its
// time, a job run with `nohup` or `disown`ed included
// (rovik_core::process::SessionTree). A shell that exits on its own, its
// tab closing with it, leaves its jobs running, as in other terminals.

use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::util::{blocking, ErrText, Registry};
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use rovik_client::hosts_page::{HostsPage, ShellAccess};
use rovik_client::ssh::{Shell, ShellEvent, Ssh};
use rovik_core::process::SessionTree;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

mod bash;

/// What the terminals take: xterm.js is xterm-compatible.
const TERM: &str = "xterm-256color";

/// A program on a pseudo-terminal here. Each part has its own lock, so a
/// write that blocks on a full pty buffer (a big paste) never holds up
/// other terminals or a resize.
pub(crate) struct PtyInstance {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// What the program starts, ended with the terminal
    /// (`TerminalState::end`); None when its id couldn't be read.
    tree: Option<Arc<SessionTree>>,
}

/// One terminal: a program on a pseudo-terminal here (the user's shell, or
/// their ssh to a host), or a shell on a host through Rovik's SSH.
pub(crate) enum Session {
    Pty(PtyInstance),
    Ssh(Shell),
}

impl Session {
    fn write(&self, data: &[u8]) -> Result<(), String> {
        match self {
            Session::Pty(pty) => {
                let mut writer = pty.writer.lock().unwrap_or_else(|e| e.into_inner());
                writer.write_all(data).err_text()?;
                writer.flush().err_text()
            }
            Session::Ssh(shell) => shell.write(data).err_text(),
        }
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        match self {
            Session::Pty(pty) => {
                let master = pty.master.lock().unwrap_or_else(|e| e.into_inner());
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .err_text()
            }
            Session::Ssh(shell) => shell.resize(cols.into(), rows.into()).err_text(),
        }
    }

    /// Hangs up on it: the program here gets SIGHUP (on Windows it's
    /// ended), the shell on a host is hung up on. Returns what the program
    /// started, to end once the hangup has had its time.
    fn hang_up(&self) -> Option<Arc<SessionTree>> {
        match self {
            Session::Pty(pty) => {
                if let Ok(mut killer) = pty.killer.lock() {
                    let _ = killer.kill();
                }
                pty.tree.clone()
            }
            Session::Ssh(shell) => {
                shell.close();
                None
            }
        }
    }
}

/// The app's terminals: the live ones by id, and the ends under way of
/// those closed.
pub(crate) struct TerminalState {
    sessions: Registry<Session>,
    /// The threads ending what closed terminals left running: the app's
    /// exit waits for them (`wait_ended`).
    ending: Mutex<Vec<JoinHandle<()>>>,
}

/// The app's terminals, none yet.
pub fn terminals() -> TerminalState {
    TerminalState {
        sessions: Registry::new("terminal"),
        ending: Mutex::new(Vec::new()),
    }
}

impl TerminalState {
    /// Closes terminal `id` if it's open (`end_session`).
    fn end(&self, id: &str) {
        if let Some(session) = self.sessions.remove(id) {
            self.end_session(session);
        }
    }

    /// Closes every open terminal, as the app exits: what they leave
    /// running is ended by the time `wait_ended` returns.
    pub fn end_all(&self) {
        for session in self.sessions.take_all() {
            self.end_session(session);
        }
    }

    /// Waits until what the closed terminals left running is ended: up to
    /// the two seconds and a half `SessionTree::end` takes.
    pub fn wait_ended(&self) {
        // Again while there are more: one that began meanwhile (a view gone
        // as the app closed it) is waited for too.
        loop {
            let ending = std::mem::take(&mut *self.ending());
            if ending.is_empty() {
                return;
            }
            for thread in ending {
                if thread.join().is_err() {
                    log::warn!(
                        "terminal: ending what one left running failed (its thread panicked)"
                    );
                }
            }
        }
    }

    /// Hangs up on `session`, and on a thread of its own ends what its
    /// program leaves running: nothing here waits out the hangup's time.
    fn end_session(&self, session: Arc<Session>) {
        if let Some(tree) = session.hang_up() {
            let ended = std::thread::Builder::new()
                .name("terminal-end".into())
                .spawn(move || tree.end());
            match ended {
                Ok(thread) => {
                    let mut ending = self.ending();
                    // Those done need no waiting for.
                    ending.retain(|t| !t.is_finished());
                    ending.push(thread);
                }
                Err(e) => log::warn!(
                    "terminal: hung up on, but what it left running runs on: no thread to end it on ({e})"
                ),
            }
        }
        // Its pseudo-terminal closes now, unless a write still holds it: on
        // Windows that closes its console, which ends the console programs
        // still in it.
        drop(session);
    }

    fn ending(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.ending.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Streamed to the frontend over the terminal's channel.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    Output {
        data: String,
    },
    /// `code` is None when the exit status couldn't be read.
    Exit {
        code: Option<u32>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    id: String,
    /// Shell program name for the tab label, e.g. "bash", "zsh", "cmd".
    shell: String,
}

/// Picks a sensible interactive shell per platform: the user's configured
/// shell on macOS/Linux, cmd.exe (via COMSPEC) on Windows.
fn default_shell() -> (String, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        (shell, vec![])
    }
    #[cfg(not(target_os = "windows"))]
    {
        use rovik_core::process::{shell_kind, ShellKind};
        let shell = rovik_core::process::user_shell().unwrap_or_else(|| "/bin/bash".into());
        // `-l` starts a login shell so PATH/rc files behave like a normal
        // terminal; only for shells known to take it.
        let args = match shell_kind(&shell) {
            ShellKind::Other => vec![],
            _ => vec!["-l".into()],
        };
        (shell, args)
    }
}

fn shell_name(shell: &str) -> String {
    Path::new(shell)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| shell.to_string())
}

/// Bytes at the end of `bytes` that start a UTF-8 sequence not yet complete.
fn incomplete_utf8_tail(bytes: &[u8]) -> usize {
    let n = bytes.len();
    for back in 1..=n.min(3) {
        let b = bytes[n - back];
        if b & 0b1100_0000 == 0b1000_0000 {
            continue; // continuation byte: keep looking for the lead byte
        }
        let needed = match b {
            0xF0..=0xF7 => 4,
            0xE0..=0xEF => 3,
            0xC0..=0xDF => 2,
            _ => 1,
        };
        return if needed > back { back } else { 0 };
    }
    0
}

/// A terminal's output as text. A character split across two reads is held
/// back until the rest of it comes: decoding each read on its own turned it
/// into two "�".
#[derive(Default)]
struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    /// The text `bytes` complete; None while they complete none.
    fn push(&mut self, bytes: &[u8]) -> Option<String> {
        self.pending.extend_from_slice(bytes);
        let ready = self.pending.len() - incomplete_utf8_tail(&self.pending);
        if ready == 0 {
            return None;
        }
        let data = String::from_utf8_lossy(&self.pending[..ready]).into_owned();
        self.pending.drain(..ready);
        Some(data)
    }

    /// What was held back, once the output has ended.
    fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let data = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        Some(data)
    }
}

/// The terminal's size, as the view asked; 80×24 when it couldn't say.
fn size(cols: Option<u16>, rows: Option<u16>) -> PtySize {
    PtySize {
        rows: rows.unwrap_or(24).max(1),
        cols: cols.unwrap_or(80).max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Starts `cmd` in the user's home folder.
fn in_home(app: &AppHandle, cmd: &mut CommandBuilder) {
    if let Ok(home) = app.path().home_dir() {
        cmd.cwd(home);
    }
}

#[tauri::command]
pub async fn spawn_terminal(
    app: AppHandle,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    on_event: Channel<TerminalEvent>,
) -> Result<TerminalInfo, String> {
    // On a worker thread: starting a shell (and finding it, writing bash's
    // init file) blocks, and as a plain command it held the window's main
    // thread.
    blocking(move || start_local_terminal(&app, cwd, cols, rows, on_event)).await
}

fn start_local_terminal(
    app: &AppHandle,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    on_event: Channel<TerminalEvent>,
) -> Result<TerminalInfo, String> {
    let state = app.state::<TerminalState>();
    let (shell, args) = default_shell();
    let mut cmd = CommandBuilder::new(&shell);
    // Bash 4.4 to 5.4 reads a file of ours in place of a login shell's
    // (`bash`).
    #[cfg(unix)]
    let args: Vec<std::ffi::OsString> = match bash::init_file(app, &shell) {
        Some(file) => vec!["--init-file".into(), file.into()],
        None => args.into_iter().map(Into::into).collect(),
    };
    cmd.args(args);
    // A project folder that was moved or deleted shouldn't make spawning fail.
    match cwd.as_deref().filter(|dir| Path::new(dir).is_dir()) {
        Some(dir) => cmd.cwd(dir),
        None => in_home(app, &mut cmd),
    }
    let id = spawn_pty(app, &state, cmd, size(cols, rows), on_event, true)?;
    Ok(TerminalInfo {
        id,
        shell: shell_name(&shell),
    })
}

/// A shell on cloud host `host` (its id), reached as the host's settings
/// say: the user's own ssh, run on a pseudo-terminal here, or Rovik's SSH,
/// on a connection of its own.
#[tauri::command]
pub async fn spawn_remote_terminal(
    app: AppHandle,
    state: State<'_, TerminalState>,
    page: State<'_, Arc<HostsPage>>,
    host: String,
    cols: Option<u16>,
    rows: Option<u16>,
    on_event: Channel<TerminalEvent>,
) -> Result<TerminalInfo, String> {
    let size = size(cols, rows);
    let target = match page.shell_access(&host)? {
        ShellAccess::Ssh(target) => target,
        ShellAccess::Wsl(wsl) => {
            let start = bash::on_host(&wsl.shell()).await;
            let mut cmd = CommandBuilder::new(rovik_client::wsl::PROGRAM);
            match start.as_ref().and_then(bash::HostStart::argv) {
                Some(argv) => cmd.args(wsl.terminal_running(&argv)),
                None => cmd.args(wsl.terminal_args()),
            }
            in_home(&app, &mut cmd);
            // What runs in it runs in the distribution: nothing counts here.
            let id = spawn_pty(&app, &state, cmd, size, on_event, false)?;
            return Ok(TerminalInfo {
                id,
                shell: wsl.distro,
            });
        }
        ShellAccess::System(ssh) => {
            let mut cmd = CommandBuilder::new(&ssh.program);
            cmd.args(ssh.terminal_args());
            in_home(&app, &mut cmd);
            // What runs in it runs on the host: nothing counts here.
            let id = spawn_pty(&app, &state, cmd, size, on_event, false)?;
            return Ok(TerminalInfo {
                id,
                shell: shell_name(&ssh.program.to_string_lossy()),
            });
        }
    };
    let ssh = Arc::new(Ssh::connect(&target).await.err_text()?);
    // How its shell starts, first: what the shell prints waits unread until
    // its output is pumped, and enough of it would hold up the answer.
    let start = bash::on_host(&*ssh).await;
    let (cols, rows) = (size.cols.into(), size.rows.into());
    let opened = match start.as_ref().and_then(bash::HostStart::command) {
        Some(command) => ssh.shell_running(TERM, cols, rows, &command).await,
        None => ssh.shell(TERM, cols, rows).await,
    };
    let (shell, mut output) = opened.err_text()?;
    let id = Uuid::new_v4().to_string();
    state.sessions.insert(id.clone(), Session::Ssh(shell));
    let exit_id = id.clone();
    rovik_core::rt::spawn(async move {
        let mut text = Utf8Stream::default();
        while let Some(event) = output.next().await {
            match event {
                ShellEvent::Output(bytes) => {
                    let Some(data) = text.push(&bytes) else {
                        continue;
                    };
                    if on_event.send(TerminalEvent::Output { data }).is_err() {
                        // The view is gone, and nothing else could end it.
                        app.state::<TerminalState>().end(&exit_id);
                        return;
                    }
                }
                ShellEvent::Exit(code) => {
                    // Hangs up, if the host hasn't.
                    app.state::<TerminalState>().sessions.remove(&exit_id);
                    if let Some(data) = text.finish() {
                        let _ = on_event.send(TerminalEvent::Output { data });
                    }
                    let _ = on_event.send(TerminalEvent::Exit { code });
                }
            }
        }
    });
    Ok(TerminalInfo {
        id,
        shell: start.map_or_else(|| "shell".into(), |start| start.name()),
    })
}

/// Runs `cmd` on a new pseudo-terminal of `size`, its output and its exit
/// on `on_event`. `counted`: its commands run on this computer, and count
/// in the queue for heavy work. Returns the terminal's id.
fn spawn_pty(
    app: &AppHandle,
    state: &TerminalState,
    mut cmd: CommandBuilder,
    size: PtySize,
    on_event: Channel<TerminalEvent>,
    counted: bool,
) -> Result<String, String> {
    // The user's environment, run from an AppImage, not the AppImage's: a
    // login shell's profile runs flatpak, python…
    for (key, value) in rovik_core::process::child_env() {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        }
    }
    // Launched from a desktop entry, the app may have no TERM at all, and
    // shells then fall back to a dumb terminal.
    cmd.env("TERM", TERM);
    cmd.env("COLORTERM", "truecolor");

    let (pty, mut reader, mut child) = open_pty(cmd, size)?;
    let id = Uuid::new_v4().to_string();
    state.sessions.insert(id.clone(), Session::Pty(pty));

    let (drained_tx, drained_rx) = mpsc::channel::<()>();

    // Reader: pty output -> channel.
    let output = on_event.clone();
    let (reader_app, reader_id) = (app.clone(), id.clone());
    std::thread::Builder::new()
        .name(format!("pty-read-{id}"))
        .spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            let mut text = Utf8Stream::default();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let Some(data) = text.push(&buf[..n]) else {
                            continue;
                        };
                        if output.send(TerminalEvent::Output { data }).is_err() {
                            // The view is gone: so is its shell, as when
                            // the terminal is closed, rather than run on
                            // unseen with nothing reading its output.
                            reader_app.state::<TerminalState>().end(&reader_id);
                            break;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    // Linux reports the child's exit as EIO, not EOF.
                    Err(_) => break,
                }
            }
            if let Some(data) = text.finish() {
                let _ = output.send(TerminalEvent::Output { data });
            }
            let _ = drained_tx.send(());
        })
        .err_text()?;

    // The shell's commands count in the queue for heavy work: a build run
    // here takes a CPU slot (chats wait, with a note naming it), but the
    // terminal itself never waits and is never slowed (src/workload).
    let job = child.process_id().filter(|_| counted).map(|pid| {
        let job = rovik_core::workload::begin(rovik_core::workload::JobKind::Terminal, "", None);
        job.spawned(pid);
        job
    });

    // Waiter: reap the child, release the pty, report the exit.
    let app = app.clone();
    let exit_id = id.clone();
    std::thread::Builder::new()
        .name(format!("pty-wait-{id}"))
        .spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code());
            drop(job);
            // Drops writer and master. On Windows that closes the
            // pseudoconsole, which is what ends the reader's blocking read.
            app.state::<TerminalState>().sessions.remove(&exit_id);
            // Let the last output arrive first. Bounded: a background job
            // that inherited the pty can keep the reader open indefinitely.
            let _ = drained_rx.recv_timeout(Duration::from_millis(500));
            let _ = on_event.send(TerminalEvent::Exit { code });
        })
        .err_text()?;

    Ok(id)
}

/// A program started on a pseudo-terminal (`open_pty`): the terminal, what
/// reads its output, and the program.
type Opened = (
    PtyInstance,
    Box<dyn Read + Send>,
    Box<dyn Child + Send + Sync>,
);

/// `cmd` started on a new pseudo-terminal of `size`.
fn open_pty(cmd: CommandBuilder, size: PtySize) -> Result<Opened, String> {
    let pair = native_pty_system().openpty(size).err_text()?;
    let child = pair.slave.spawn_command(cmd).err_text()?;
    // Drop our copy of the slave end now that the child owns it, or the
    // master's reader never sees EOF when the child exits.
    drop(pair.slave);
    let reader = pair.master.try_clone_reader().err_text()?;
    let writer = pair.master.take_writer().err_text()?;
    let pty = PtyInstance {
        writer: Mutex::new(writer),
        master: Mutex::new(pair.master),
        killer: Mutex::new(child.clone_killer()),
        // At once: on Windows the program goes into its job now, before it
        // has started anything.
        tree: child.process_id().map(|pid| Arc::new(SessionTree::of(pid))),
    };
    Ok((pty, reader, child))
}

/// Off the async threads: a write to a full pty buffer (a big paste into a
/// program that isn't reading) blocks until the program reads.
#[tauri::command]
pub async fn write_to_terminal(
    state: State<'_, TerminalState>,
    id: String,
    data: String,
) -> Result<(), String> {
    let session = state.sessions.get(&id)?;
    blocking(move || session.write(data.as_bytes())).await
}

#[tauri::command]
pub fn resize_terminal(
    state: State<'_, TerminalState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state.sessions.get(&id)?.resize(cols, rows)
}

/// Closes the terminal: hangs up on its shell, or on the one on a host,
/// and ends what the shell left running (`TerminalState::end`). Its waiter
/// thread, or its output's task, then cleans up; closing a terminal that
/// already exited is a no-op.
#[tauri::command]
pub fn close_terminal(state: State<'_, TerminalState>, id: String) -> Result<(), String> {
    state.end(&id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_split_across_reads_comes_out_whole() {
        let snowman = "☃".as_bytes();
        let mut text = Utf8Stream::default();
        assert_eq!(text.push(b"a"), Some("a".into()));
        assert_eq!(text.push(&snowman[..1]), None);
        assert_eq!(text.push(&snowman[1..2]), None);
        assert_eq!(text.push(&snowman[2..]), Some("☃".into()));
        assert_eq!(text.push(&[b'b', snowman[0]]), Some("b".into()));
        // Cut short where the output ended: shown as it is.
        assert_eq!(text.finish(), Some("\u{FFFD}".into()));
        assert_eq!(text.finish(), None);
    }

    #[test]
    fn a_terminal_is_as_big_as_its_view_says() {
        let asked = size(Some(132), Some(43));
        assert_eq!((asked.cols, asked.rows), (132, 43));
        let unsaid = size(None, Some(0));
        assert_eq!((unsaid.cols, unsaid.rows), (80, 1));
    }

    /// A shell whose job ignores both the hangup and the request to end
    /// (SIGTERM), so that only a kill ends it: it writes "up" to `$1` once
    /// the job runs.
    #[cfg(unix)]
    const LEFT_RUNNING: &str = r#"trap '' HUP TERM; sleep 60 & echo up > "$1"; wait"#;

    /// A terminal running `script` (sh, with `arg` its `$1`), open in
    /// `state` as `spawn_pty` opens one: its output read, its program
    /// reaped. Its id and tree, once the script has written to `arg`.
    #[cfg(unix)]
    fn open_in(state: &TerminalState, script: &str, arg: &Path) -> (String, Arc<SessionTree>) {
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", script, "sh"]);
        cmd.arg(arg);
        let (pty, mut reader, mut child) = open_pty(cmd, size(None, None)).unwrap();
        let tree = pty.tree.clone().expect("its tree");
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
        });
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let id = Uuid::new_v4().to_string();
        state.sessions.insert(id.clone(), Session::Pty(pty));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::fs::read_to_string(arg).unwrap_or_default().trim() != "up" {
            assert!(
                std::time::Instant::now() < deadline,
                "the script didn't start"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        (id, tree)
    }

    /// Closing a terminal ends what its shell left running, what ignores
    /// the hangup and the request to end included.
    #[cfg(unix)]
    #[test]
    fn closing_a_terminal_ends_what_its_shell_left_running() {
        let state = terminals();
        let dir = rovik_core::testing::temp_dir();
        let (id, tree) = open_in(&state, LEFT_RUNNING, &dir.0.join("up"));
        assert!(!tree.is_empty());

        state.end(&id);
        assert!(state.sessions.get(&id).is_err(), "closed at once");
        state.wait_ended();
        assert!(tree.is_empty(), "nothing it ran is left");
    }

    /// Quitting closes every terminal and waits until what they left
    /// running is ended, that of one closed just before included.
    #[cfg(unix)]
    #[test]
    fn quitting_ends_what_every_terminal_left_running() {
        let state = terminals();
        let dir = rovik_core::testing::temp_dir();
        let (closed, closed_tree) = open_in(&state, LEFT_RUNNING, &dir.0.join("closed"));
        let (_, open_tree) = open_in(&state, LEFT_RUNNING, &dir.0.join("open"));

        state.end(&closed);
        state.end_all();
        assert!(state.sessions.take_all().is_empty(), "every one closed");
        state.wait_ended();
        assert!(closed_tree.is_empty() && open_tree.is_empty());
        // Closing them again, as a second exit would, finds nothing to do.
        state.end_all();
        state.wait_ended();
    }
}
