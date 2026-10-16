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

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use rovik_client::hosts_page::{HostsPage, ShellAccess};
use rovik_client::ssh::{Shell, ShellEvent, Ssh};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

/// What the terminals take: xterm.js is xterm-compatible.
const TERM: &str = "xterm-256color";

/// A program on a pseudo-terminal here. Each part has its own lock, so a
/// write that blocks on a full pty buffer (a big paste) never holds up
/// other terminals or a resize.
struct PtyInstance {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
}

/// One terminal: a program on a pseudo-terminal here (the user's shell, or
/// their ssh to a host), or a shell on a host through Rovik's SSH.
enum Session {
    Pty(PtyInstance),
    Ssh(Shell),
}

impl Session {
    fn write(&self, data: &[u8]) -> Result<(), String> {
        match self {
            Session::Pty(pty) => {
                let mut writer = pty
                    .writer
                    .lock()
                    .map_err(|_| "terminal writer poisoned".to_string())?;
                writer.write_all(data).map_err(|e| e.to_string())?;
                writer.flush().map_err(|e| e.to_string())
            }
            Session::Ssh(shell) => shell.write(data).map_err(|e| e.to_string()),
        }
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        match self {
            Session::Pty(pty) => {
                let master = pty
                    .master
                    .lock()
                    .map_err(|_| "terminal pty poisoned".to_string())?;
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|e| e.to_string())
            }
            Session::Ssh(shell) => shell
                .resize(cols.into(), rows.into())
                .map_err(|e| e.to_string()),
        }
    }

    /// Ends it: the program here is killed, the shell on a host hung up on.
    fn kill(&self) {
        match self {
            Session::Pty(pty) => {
                if let Ok(mut killer) = pty.killer.lock() {
                    let _ = killer.kill();
                }
            }
            Session::Ssh(shell) => shell.close(),
        }
    }
}

/// Live terminals by id. The map lock is only held to look an entry up.
#[derive(Default)]
pub struct TerminalState(Mutex<HashMap<String, Arc<Session>>>);

impl TerminalState {
    fn get(&self, id: &str) -> Result<Arc<Session>, String> {
        self.0
            .lock()
            .map_err(|_| "terminal state poisoned".to_string())?
            .get(id)
            .cloned()
            .ok_or_else(|| format!("terminal {id} not found"))
    }

    fn insert(&self, id: String, session: Session) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "terminal state poisoned".to_string())?
            .insert(id, Arc::new(session));
        Ok(())
    }

    fn remove(&self, id: &str) -> Option<Arc<Session>> {
        self.0.lock().ok()?.remove(id)
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
pub fn spawn_terminal(
    app: AppHandle,
    state: State<'_, TerminalState>,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    on_event: Channel<TerminalEvent>,
) -> Result<TerminalInfo, String> {
    let (shell, args) = default_shell();
    let mut cmd = CommandBuilder::new(&shell);
    cmd.args(args);
    // A project folder that was moved or deleted shouldn't make spawning fail.
    match cwd.as_deref().filter(|dir| Path::new(dir).is_dir()) {
        Some(dir) => cmd.cwd(dir),
        None => in_home(&app, &mut cmd),
    }
    let id = spawn_pty(&app, &state, cmd, size(cols, rows), on_event, true)?;
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
    let ssh = Arc::new(Ssh::connect(&target).await.map_err(|e| e.to_string())?);
    // Its name first: what the shell prints waits unread until its output
    // is pumped, and enough of it would hold up the answer.
    let name = ssh.login_shell().await;
    let opened = ssh.shell(TERM, size.cols.into(), size.rows.into()).await;
    let (shell, mut output) = opened.map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    state.insert(id.clone(), Session::Ssh(shell))?;
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
                        if let Some(session) = app.state::<TerminalState>().remove(&exit_id) {
                            session.kill();
                        }
                        return;
                    }
                }
                ShellEvent::Exit(code) => {
                    // Hangs up, if the host hasn't.
                    app.state::<TerminalState>().remove(&exit_id);
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
        shell: name.unwrap_or_else(|| "shell".into()),
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
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|e| e.to_string())?;

    // Launched from a desktop entry, the app may have no TERM at all, and
    // shells then fall back to a dumb terminal.
    cmd.env("TERM", TERM);
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    // Drop our copy of the slave end now that the child owns it, or the
    // master's reader never sees EOF when the child exits.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let killer = child.clone_killer();

    let id = Uuid::new_v4().to_string();
    state.insert(
        id.clone(),
        Session::Pty(PtyInstance {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
        }),
    )?;

    let (drained_tx, drained_rx) = mpsc::channel::<()>();

    // Reader: pty output -> channel.
    let output = on_event.clone();
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
                            break; // the view is gone
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
        .map_err(|e| e.to_string())?;

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
            app.state::<TerminalState>().remove(&exit_id);
            // Let the last output arrive first. Bounded: a background job
            // that inherited the pty can keep the reader open indefinitely.
            let _ = drained_rx.recv_timeout(Duration::from_millis(500));
            let _ = on_event.send(TerminalEvent::Exit { code });
        })
        .map_err(|e| e.to_string())?;

    Ok(id)
}

#[tauri::command]
pub fn write_to_terminal(
    state: State<'_, TerminalState>,
    id: String,
    data: String,
) -> Result<(), String> {
    state.get(&id)?.write(data.as_bytes())
}

#[tauri::command]
pub fn resize_terminal(
    state: State<'_, TerminalState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state.get(&id)?.resize(cols, rows)
}

/// Kills the shell, or hangs up on the one on a host. Its waiter thread, or
/// its output's task, then cleans up; closing a terminal that already
/// exited is a no-op.
#[tauri::command]
pub fn close_terminal(state: State<'_, TerminalState>, id: String) -> Result<(), String> {
    if let Some(session) = state.remove(&id) {
        session.kill();
    }
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
}
