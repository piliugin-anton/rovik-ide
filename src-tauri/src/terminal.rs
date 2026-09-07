// Cross-platform terminal backend for the Tauri app.
//
// Uses `portable-pty`, which wraps ConPTY on Windows and a real pty on
// macOS/Linux, so the same code path spawns a working interactive shell on
// every platform.
//
// Register in main.rs / lib.rs:
//
//   mod terminal;
//   use terminal::{TerminalState, spawn_terminal, write_to_terminal, resize_terminal, close_terminal};
//
//   tauri::Builder::default()
//       .manage(TerminalState::default())
//       .invoke_handler(tauri::generate_handler![
//           spawn_terminal,
//           write_to_terminal,
//           resize_terminal,
//           close_terminal,
//           // ...your existing commands
//       ])
//
// Cargo.toml:
//   portable-pty = "0.8"
//   uuid = { version = "1", features = ["v4"] }

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

struct PtyInstance {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

#[derive(Default)]
pub struct TerminalState(Mutex<HashMap<String, PtyInstance>>);

#[derive(Clone, Serialize)]
struct TerminalDataPayload<'a> {
    id: &'a str,
    data: String,
}

#[derive(Clone, Serialize)]
struct TerminalExitPayload<'a> {
    id: &'a str,
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

#[tauri::command]
pub fn spawn_terminal(
    app: AppHandle,
    state: State<TerminalState>,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<String, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.unwrap_or(24),
            cols: cols.unwrap_or(80),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let (shell, args) = default_shell();
    let mut cmd = CommandBuilder::new(shell);
    cmd.args(args);
    if let Some(dir) = &cwd {
        cmd.cwd(dir);
    }

    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    // Drop our copy of the slave end now that the child owns it, or the
    // master's reader never sees EOF when the child exits.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    let id = Uuid::new_v4().to_string();
    state
        .0
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?
        .insert(
            id.clone(),
            PtyInstance {
                writer,
                master: pair.master,
                child,
            },
        );

    // Pump PTY output -> frontend events on a dedicated thread so we never
    // block the async command runtime on blocking reads.
    let emit_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if app
                        .emit("terminal:data", TerminalDataPayload { id: &emit_id, data })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = app.emit("terminal:exit", TerminalExitPayload { id: &emit_id });
    });

    Ok(id)
}

#[tauri::command]
pub fn write_to_terminal(
    state: State<TerminalState>,
    id: String,
    data: String,
) -> Result<(), String> {
    let mut instances = state
        .0
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    let instance = instances.get_mut(&id).ok_or("terminal not found")?;
    instance
        .writer
        .write_all(data.as_bytes())
        .map_err(|e| e.to_string())?;
    instance.writer.flush().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn resize_terminal(
    state: State<TerminalState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let instances = state
        .0
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    let instance = instances.get(&id).ok_or("terminal not found")?;
    instance
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn close_terminal(state: State<TerminalState>, id: String) -> Result<(), String> {
    let mut instances = state
        .0
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    if let Some(mut instance) = instances.remove(&id) {
        let _ = instance.child.kill();
    }
    Ok(())
}
