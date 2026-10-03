// Terminal backend: any number of shells, each on its own pseudo-terminal.
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

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

/// One terminal. Each part has its own lock, so a write that blocks on a
/// full pty buffer (a big paste) never holds up other terminals or a resize.
struct PtyInstance {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
}

/// Live terminals by id. The map lock is only held to look an entry up.
#[derive(Default)]
pub struct TerminalState(Mutex<HashMap<String, Arc<PtyInstance>>>);

impl TerminalState {
    fn get(&self, id: &str) -> Result<Arc<PtyInstance>, String> {
        self.0
            .lock()
            .map_err(|_| "terminal state poisoned".to_string())?
            .get(id)
            .cloned()
            .ok_or_else(|| format!("terminal {id} not found"))
    }

    fn insert(&self, id: String, instance: PtyInstance) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "terminal state poisoned".to_string())?
            .insert(id, Arc::new(instance));
        Ok(())
    }

    fn remove(&self, id: &str) -> Option<Arc<PtyInstance>> {
        self.0.lock().ok()?.remove(id)
    }
}

/// Streamed to the frontend over the terminal's channel.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    Output { data: String },
    /// `code` is None when the exit status couldn't be read.
    Exit { code: Option<u32> },
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
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        // `-l` starts a login shell so PATH/rc files behave like a normal terminal.
        (shell, vec!["-l".into()])
    }
}

fn shell_name(shell: &str) -> String {
    Path::new(shell)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| shell.to_string())
}

/// Bytes at the end of `bytes` that start a UTF-8 sequence not yet complete.
/// They're held back until the next read: decoding each read on its own
/// turned a character split across two reads into two "�".
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

#[tauri::command]
pub fn spawn_terminal(
    app: AppHandle,
    state: State<'_, TerminalState>,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    on_event: Channel<TerminalEvent>,
) -> Result<TerminalInfo, String> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: rows.unwrap_or(24).max(1),
            cols: cols.unwrap_or(80).max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let (shell, args) = default_shell();
    let mut cmd = CommandBuilder::new(&shell);
    cmd.args(args);
    // Launched from a desktop entry, the app may have no TERM at all, and
    // shells then fall back to a dumb terminal. xterm.js is xterm-compatible.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // A project folder that was moved or deleted shouldn't make spawning fail.
    match cwd.as_deref().filter(|dir| Path::new(dir).is_dir()) {
        Some(dir) => cmd.cwd(dir),
        None => {
            if let Some(home) = app.path().home_dir().ok() {
                cmd.cwd(home);
            }
        }
    }

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
        PtyInstance {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
        },
    )?;

    let (drained_tx, drained_rx) = mpsc::channel::<()>();

    // Reader: pty output -> channel.
    let output = on_event.clone();
    std::thread::Builder::new()
        .name(format!("pty-read-{id}"))
        .spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            let mut pending: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        let ready = pending.len() - incomplete_utf8_tail(&pending);
                        if ready == 0 {
                            continue;
                        }
                        let data = String::from_utf8_lossy(&pending[..ready]).into_owned();
                        pending.drain(..ready);
                        if output.send(TerminalEvent::Output { data }).is_err() {
                            break; // the view is gone
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    // Linux reports the child's exit as EIO, not EOF.
                    Err(_) => break,
                }
            }
            if !pending.is_empty() {
                let data = String::from_utf8_lossy(&pending).into_owned();
                let _ = output.send(TerminalEvent::Output { data });
            }
            let _ = drained_tx.send(());
        })
        .map_err(|e| e.to_string())?;

    // Waiter: reap the child, release the pty, report the exit.
    let exit_id = id.clone();
    std::thread::Builder::new()
        .name(format!("pty-wait-{id}"))
        .spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code());
            // Drops writer and master. On Windows that closes the
            // pseudoconsole, which is what ends the reader's blocking read.
            app.state::<TerminalState>().remove(&exit_id);
            // Let the last output arrive first. Bounded: a background job
            // that inherited the pty can keep the reader open indefinitely.
            let _ = drained_rx.recv_timeout(Duration::from_millis(500));
            let _ = on_event.send(TerminalEvent::Exit { code });
        })
        .map_err(|e| e.to_string())?;

    Ok(TerminalInfo {
        id,
        shell: shell_name(&shell),
    })
}

#[tauri::command]
pub fn write_to_terminal(
    state: State<'_, TerminalState>,
    id: String,
    data: String,
) -> Result<(), String> {
    let instance = state.get(&id)?;
    let mut writer = instance
        .writer
        .lock()
        .map_err(|_| "terminal writer poisoned".to_string())?;
    writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resize_terminal(
    state: State<'_, TerminalState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let instance = state.get(&id)?;
    let master = instance
        .master
        .lock()
        .map_err(|_| "terminal pty poisoned".to_string())?;
    master
        .resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
}

/// Kills the shell. Its waiter thread then reaps it and cleans up; closing
/// a terminal that already exited is a no-op.
#[tauri::command]
pub fn close_terminal(state: State<'_, TerminalState>, id: String) -> Result<(), String> {
    if let Some(instance) = state.remove(&id) {
        if let Ok(mut killer) = instance.killer.lock() {
            let _ = killer.kill();
        }
    }
    Ok(())
}
