//! Language servers (rust-analyzer today), bridged to the editor.
//!
//! A server is a child process speaking the Language Server Protocol over
//! stdio: JSON-RPC messages, each preceded by a `Content-Length` header.
//! The backend only frames and forwards; the protocol itself is handled by
//! the frontend's client (src/lib/lsp/). Like the terminal, each server gets
//! plain OS threads (never the async runtime) and a Tauri channel handed in
//! at start, so nothing it sends can be missed.

pub mod rust_analyzer;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

/// Bigger messages aren't legitimate LSP traffic.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

struct Server {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
}

#[derive(Default)]
pub struct LspState(Mutex<HashMap<String, Arc<Server>>>);

impl LspState {
    fn get(&self, id: &str) -> Result<Arc<Server>, String> {
        self.0
            .lock()
            .map_err(|_| "language server state poisoned".to_string())?
            .get(id)
            .cloned()
            .ok_or_else(|| format!("language server {id} isn't running"))
    }

    fn remove(&self, id: &str) -> Option<Arc<Server>> {
        self.0.lock().ok()?.remove(id)
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LspEvent {
    /// One JSON-RPC message from the server.
    Message {
        data: String,
    },
    /// A line the server logged (rust-analyzer logs to stderr).
    Log {
        line: String,
    },
    Exit {
        code: Option<i32>,
    },
}

/// The PATH a program the app starts gets, with the cargo bin folder
/// added. Launched from a desktop entry, the app often doesn't have it, and
/// rust-analyzer needs `cargo`/`rustc`.
pub fn tool_path() -> std::ffi::OsString {
    let mut dirs: Vec<std::path::PathBuf> = rovik_core::process::child_var("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| std::path::Path::new(&h).join(".cargo"))
        })
        .map(|c| c.join("bin"));
    if let Some(bin) = cargo_bin.filter(|b| b.is_dir() && !dirs.contains(b)) {
        dirs.push(bin);
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

/// The program to start for `command`. On Windows, a bare name is looked up
/// on `path` with each of PATHEXT's extensions: `Command` only tries `.exe`,
/// and servers installed by npm (typescript-language-server, …) are `.cmd`
/// scripts. Everything else is returned as given.
pub fn find_program(command: &str, path: &std::ffi::OsStr) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let given = std::path::Path::new(command);
        if given.extension().is_none() && given.components().count() == 1 {
            let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
            for dir in std::env::split_paths(path) {
                for ext in exts.split(';').filter(|e| !e.is_empty()) {
                    let candidate = dir.join(format!("{command}{}", ext.to_ascii_lowercase()));
                    if candidate.is_file() {
                        return candidate;
                    }
                }
            }
        }
    }
    let _ = path;
    std::path::PathBuf::from(command)
}

/// Reads one framed message: headers up to a blank line, then the body.
/// None at end of stream.
fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue; // stray blank line between messages
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().ok();
            }
        }
    }
    let length = length.unwrap_or(0);
    if length > MAX_MESSAGE_BYTES {
        return Err(std::io::Error::other(format!("message of {length} bytes")));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

pub fn frame(message: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", message.len()).into_bytes();
    out.extend_from_slice(message.as_bytes());
    out
}

#[tauri::command]
pub fn lsp_start(
    app: AppHandle,
    state: State<'_, LspState>,
    command: String,
    args: Vec<String>,
    cwd: String,
    on_event: Channel<LspEvent>,
) -> Result<String, String> {
    let path = tool_path();
    let mut child = rovik_core::process::command(find_program(&command, &path))
        .args(&args)
        .current_dir(&cwd)
        .env("PATH", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{command}: {e}"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;

    let id = Uuid::new_v4().to_string();
    state
        .0
        .lock()
        .map_err(|_| "language server state poisoned".to_string())?
        .insert(
            id.clone(),
            Arc::new(Server {
                stdin: Mutex::new(stdin),
                child: Mutex::new(child),
            }),
        );

    let messages = on_event.clone();
    std::thread::Builder::new()
        .name(format!("lsp-out-{id}"))
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(data)) = read_message(&mut reader) {
                if messages.send(LspEvent::Message { data }).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let logs = on_event.clone();
    std::thread::Builder::new()
        .name(format!("lsp-err-{id}"))
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = logs.send(LspEvent::Log { line });
            }
        })
        .map_err(|e| e.to_string())?;

    // Waiter: polls rather than blocking on wait(), so lsp_stop can still
    // take the lock to kill it.
    let waiter_id = id.clone();
    std::thread::Builder::new()
        .name(format!("lsp-wait-{id}"))
        .spawn(move || {
            let code = loop {
                let Some(server) = app.state::<LspState>().get(&waiter_id).ok() else {
                    break None;
                };
                let status = server
                    .child
                    .lock()
                    .ok()
                    .and_then(|mut c| c.try_wait().ok().flatten());
                if let Some(status) = status {
                    break status.code();
                }
                drop(server);
                std::thread::sleep(Duration::from_millis(250));
            };
            app.state::<LspState>().remove(&waiter_id);
            let _ = on_event.send(LspEvent::Exit { code });
        })
        .map_err(|e| e.to_string())?;

    Ok(id)
}

#[tauri::command]
pub fn lsp_send(state: State<'_, LspState>, id: String, message: String) -> Result<(), String> {
    let server = state.get(&id)?;
    let mut stdin = server
        .stdin
        .lock()
        .map_err(|_| "stdin poisoned".to_string())?;
    stdin
        .write_all(&frame(&message))
        .map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())
}

/// Stops a server: the frontend sends `shutdown` and `exit` first; this
/// makes sure it's gone (waiting briefly for a clean exit).
#[tauri::command]
pub fn lsp_stop(state: State<'_, LspState>, id: String) -> Result<(), String> {
    let Some(server) = state.remove(&id) else {
        return Ok(());
    };
    for _ in 0..20 {
        if server
            .child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten())
            .is_some()
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if let Ok(mut child) = server.child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn finds_npm_cmd_shims_on_path() {
        let dir = std::env::temp_dir().join(format!("find-program-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("some-language-server.cmd"), "@echo off\r\n").unwrap();
        let path = std::env::join_paths([&dir]).unwrap();
        assert_eq!(
            find_program("some-language-server", &path),
            dir.join("some-language-server.cmd")
        );
        assert_eq!(
            find_program("not-there", &path),
            std::path::PathBuf::from("not-there")
        );
        assert_eq!(
            find_program("node.exe", &path),
            std::path::PathBuf::from("node.exe")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn leaves_names_alone_elsewhere_or_with_an_extension() {
        let path = std::ffi::OsString::new();
        assert_eq!(
            find_program("rust-analyzer", &path),
            std::path::PathBuf::from("rust-analyzer")
        );
        assert_eq!(
            find_program("/usr/bin/x", &path),
            std::path::PathBuf::from("/usr/bin/x")
        );
    }

    #[test]
    fn frames_round_trip() {
        let a = r#"{"jsonrpc":"2.0","id":1,"result":"ж"}"#;
        let b = r#"{"jsonrpc":"2.0","method":"x"}"#;
        let mut stream = frame(a);
        stream.extend(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n");
        stream.extend(frame(b));
        let mut reader = BufReader::new(&stream[..]);
        assert_eq!(read_message(&mut reader).unwrap().as_deref(), Some(a));
        assert_eq!(read_message(&mut reader).unwrap().as_deref(), Some(b));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }
}
