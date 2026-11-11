//! Backend-owned language servers with client-private event streams.
pub mod rust_analyzer;

use crate::desktop_session::Session;
use crate::util::{blocking, ErrText, Registry};
use rovik_core::process::SessionTree;
use serde::Serialize;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

/// Bigger messages aren't legitimate LSP traffic.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct Server {
    stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
    stopping: tokio::sync::watch::Sender<bool>,
    child: Mutex<Child>,
    tree: SessionTree,
}

impl Server {
    async fn send(&self, message: &str) -> Result<(), String> {
        let mut stopped = self.stopping.subscribe();
        if *stopped.borrow() {
            return Err("Language server stopped".into());
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => Err("Language server stopped".into()),
            result = async {
                let mut stdin = self.stdin.lock().await;
                stdin.write_all(&frame(message)).await.err_text()?;
                stdin.flush().await.err_text()
            } => result,
        }
    }

    fn kill(&self) {
        self.stopping.send_replace(true);
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
        self.tree.end();
        let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).wait();
    }
}

/// Backend process registry; client sessions own controls and cleanup registrations.
pub struct LspState {
    servers: Registry<Server>,
    waiters: Mutex<Vec<std::thread::JoinHandle<()>>>,
    closing: Mutex<bool>,
}

impl Default for LspState {
    fn default() -> Self {
        Self {
            servers: Registry::new("language server"),
            waiters: Mutex::new(Vec::new()),
            closing: Mutex::new(false),
        }
    }
}

impl LspState {
    /// Ends every process tree and joins its output workers before backend exit.
    /// Called on a blocking thread, once client sessions have closed.
    pub fn shutdown(&self) {
        let mut closing = self.closing.lock().unwrap_or_else(|e| e.into_inner());
        *closing = true;
        for server in self.servers.take_all() {
            server.kill();
        }
        drop(closing);
        let waiters = std::mem::take(&mut *self.waiters.lock().unwrap_or_else(|e| e.into_inner()));
        for waiter in waiters {
            if waiter.join().is_err() {
                log::error!("Language-server cleanup worker failed");
            }
        }
    }
}

/// Private output from one client's language server.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LspEvent {
    /// One JSON-RPC message from the server.
    Message { data: String },
    /// One diagnostic line from stderr.
    Log { line: String },
    /// The process exited, after its output has drained.
    Exit { code: Option<i32> },
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
async fn read_message(reader: &mut (impl AsyncBufRead + Unpin)) -> std::io::Result<Option<String>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
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
    reader.read_exact(&mut body).await?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

/// Frames one JSON-RPC message using its UTF-8 byte length.
pub fn frame(message: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", message.len()).into_bytes();
    out.extend_from_slice(message.as_bytes());
    out
}

/// Starts a process owned by this client; registration precedes all output.
pub(crate) async fn start(
    state: Arc<LspState>,
    owner: Arc<Session>,
    command: String,
    args: Vec<String>,
    cwd: String,
    sink: String,
) -> Result<String, String> {
    blocking(move || start_server(state, owner, command, args, cwd, sink)).await
}

fn start_server(
    state: Arc<LspState>,
    owner: Arc<Session>,
    command: String,
    args: Vec<String>,
    cwd: String,
    sink: String,
) -> Result<String, String> {
    let closing = state.closing.lock().unwrap_or_else(|e| e.into_inner());
    if *closing {
        return Err("The language-server backend is shutting down".into());
    }
    let path = tool_path();
    let mut cmd = rovik_core::process::command(find_program(&command, &path));
    rovik_core::process::own_session(&mut cmd);
    let mut child = rovik_core::process::no_window(&mut cmd)
        .args(args)
        .current_dir(cwd)
        .env("PATH", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{command}: {e}"))?;
    let tree = SessionTree::of(child.id());
    let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
        let _ = child.kill();
        tree.end();
        let _ = child.wait();
        return Err("Language server did not provide its standard streams".into());
    };
    let _runtime = rovik_core::rt::handle().enter();
    let pipes = (|| -> std::io::Result<_> {
        Ok((
            tokio::process::ChildStdin::from_std(stdin)?,
            tokio::process::ChildStdout::from_std(stdout)?,
            tokio::process::ChildStderr::from_std(stderr)?,
        ))
    })();
    let (stdin, stdout, stderr) = match pipes {
        Ok(pipes) => pipes,
        Err(error) => {
            let _ = child.kill();
            tree.end();
            let _ = child.wait();
            return Err(format!("Could not open language-server pipes: {error}"));
        }
    };
    let id = Uuid::new_v4().to_string();
    let server = state.servers.insert(
        id.clone(),
        Server {
            stdin: tokio::sync::Mutex::new(stdin),
            stopping: tokio::sync::watch::channel(false).0,
            tree,
            child: Mutex::new(child),
        },
    );
    let cleanup_state = state.clone();
    let cleanup_id = id.clone();
    owner.register(format!("lsp:{id}"), sink.clone(), move || {
        if let Some(server) = cleanup_state.servers.remove(&cleanup_id) {
            server.kill();
        }
    })?;
    let output = Output {
        owner: Arc::downgrade(&owner),
        sink,
    };
    let setup = (|| -> Result<(), String> {
        let messages = output.clone();
        let reader_server = server.clone();
        let mut reader = Reader(rovik_core::rt::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader).await {
                    Ok(Some(data)) => {
                        if !messages.send(LspEvent::Message { data }) {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        messages.send(LspEvent::Log {
                            line: format!("Invalid language-server stream: {e}"),
                        });
                        break;
                    }
                }
            }
            reader_server.kill();
        }));
        let logs = output.clone();
        let log_server = server.clone();
        let mut logger = Reader(rovik_core::rt::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if !logs.send(LspEvent::Log { line }) {
                            log_server.kill();
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        logs.send(LspEvent::Log {
                            line: format!("Could not read language-server logs: {error}"),
                        });
                        log_server.kill();
                        break;
                    }
                }
            }
        }));
        let waiter = server.clone();
        let waiter_state = state.clone();
        let waiter_id = id.clone();
        let worker = std::thread::Builder::new()
            .name(format!("lsp-wait-{id}"))
            .spawn(move || {
                let code = loop {
                    let status = waiter
                        .child
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .try_wait();
                    match status {
                        Ok(Some(status)) => break status.code(),
                        Err(e) => {
                            output.send(LspEvent::Log {
                                line: format!("Could not wait for language server: {e}"),
                            });
                            waiter.kill();
                            break None;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    }
                };
                waiter.kill();
                waiter.tree.end();
                rovik_core::rt::block_on(async {
                    tokio::join!(reader.drain(&output), logger.drain(&output));
                });
                output.send(LspEvent::Exit { code });
                waiter_state.servers.remove(&waiter_id);
                if let Some(owner) = output.owner.upgrade() {
                    owner.finished(&format!("lsp:{waiter_id}"));
                }
            })
            .err_text()?;
        let mut waiters = state.waiters.lock().unwrap_or_else(|e| e.into_inner());
        waiters.retain(|worker| !worker.is_finished());
        waiters.push(worker);
        Ok(())
    })();
    if let Err(error) = setup {
        owner.remove(&format!("lsp:{id}"));
        server.kill();
        server.tree.end();
        return Err(error);
    }
    Ok(id)
}

// Abort closes the async pipe even when a detached helper inherited its other
// end. The owning waiter drains normal output before publishing Exit.
struct Reader(tokio::task::JoinHandle<()>);
impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Reader {
    async fn drain(&mut self, output: &Output) {
        #[cfg(test)]
        let grace = Duration::from_millis(50);
        #[cfg(not(test))]
        let grace = Duration::from_secs(1);
        match tokio::time::timeout(grace, &mut self.0).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                output.send(LspEvent::Log {
                    line: format!("Language-server output worker failed: {error}"),
                });
            }
            Err(_) => {
                self.0.abort();
                let _ = (&mut self.0).await;
                output.send(LspEvent::Log {
                    line:
                        "Language server exited but an output pipe stayed open; stopped reading it"
                            .into(),
                });
            }
        }
    }
}

#[derive(Clone)]
struct Output {
    owner: std::sync::Weak<Session>,
    sink: String,
}
impl Output {
    fn send(&self, event: LspEvent) -> bool {
        self.owner
            .upgrade()
            .is_some_and(|owner| owner.emit(&self.sink, event))
    }
}

pub(crate) fn owned(state: &LspState, owner: &Session, id: &str) -> Result<Arc<Server>, String> {
    if !owner.owns(&format!("lsp:{id}")) {
        return Err("This language server is not open in this client session".into());
    }
    state.servers.get(id)
}

/// Writes are cancellable even if a detached helper keeps stdin open.
pub(crate) async fn send(server: Arc<Server>, message: String) -> Result<(), String> {
    server.send(&message).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_joins_active_servers_and_refuses_new_processes() {
        let state = Arc::new(LspState::default());
        let lease = crate::desktop_session::Sessions::default().create();
        lease.session.begin("server").unwrap();
        let mut events = lease.session.events().unwrap();
        let id = start(
            state.clone(),
            lease.session.clone(),
            "sh".into(),
            vec![
                "-c".into(),
                "printf 'Content-Length: 5\r\n\r\nready'; sleep 1000".into(),
            ],
            "/tmp".into(),
            "server".into(),
        )
        .await
        .unwrap();
        let event = events.next().await.unwrap();
        assert!(
            matches!(event.body, crate::desktop_session::EventBody::Data(value) if value["data"] == "ready")
        );
        let server = state.servers.get(&id).unwrap();
        let closing = state.clone();
        blocking(move || {
            closing.shutdown();
            Ok(())
        })
        .await
        .unwrap();
        assert!(server.child.lock().unwrap().try_wait().unwrap().is_some());
        assert!(server.tree.is_empty());
        assert!(state.servers.get(&id).is_err());
        lease.session.begin("late").unwrap();
        let error = start(
            state,
            lease.session.clone(),
            "sh".into(),
            vec![],
            "/tmp".into(),
            "late".into(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("shutting down"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn malformed_output_reports_failure_then_exit_and_reaps_the_server() {
        let state = Arc::new(LspState::default());
        let lease = crate::desktop_session::Sessions::default().create();
        lease.session.begin("bad").unwrap();
        let mut events = lease.session.events().unwrap();
        start(
            state.clone(),
            lease.session.clone(),
            "sh".into(),
            vec![
                "-c".into(),
                "printf 'Content-Length: 999999999\r\n\r\n'; sleep 1000".into(),
            ],
            "/tmp".into(),
            "bad".into(),
        )
        .await
        .unwrap();
        for expected in ["log", "exit"] {
            let event = tokio::time::timeout(Duration::from_secs(10), events.next())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(event.body, crate::desktop_session::EventBody::Data(value) if value["type"] == expected)
            );
        }
        blocking(move || {
            state.shutdown();
            Ok(())
        })
        .await
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_detached_helpers_open_pipe_cannot_hold_shutdown_open() {
        struct Helper(u32);
        impl Drop for Helper {
            fn drop(&mut self) {
                unsafe {
                    libc::kill(self.0 as i32, libc::SIGKILL);
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("helper");
        let state = Arc::new(LspState::default());
        let lease = crate::desktop_session::Sessions::default().create();
        lease.session.begin("detached").unwrap();
        let mut events = lease.session.events().unwrap();
        let script = r#"setsid sh -c 'echo $$ > "$1"; exec sleep 1000' helper "$1" &
while [ ! -s "$1" ]; do sleep 0.01; done
printf 'Content-Length: 5\r\n\r\nready'
"#;
        start(
            state.clone(),
            lease.session.clone(),
            "sh".into(),
            vec![
                "-c".into(),
                script.into(),
                "fixture".into(),
                pid_file.to_string_lossy().into_owned(),
            ],
            "/tmp".into(),
            "detached".into(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .unwrap()
            .unwrap();
        let helper = Helper(
            std::fs::read_to_string(pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
        // It intentionally left the server's session; it remains alive with
        // stdout open. Its pipe must be closed by cancelling the reader task.
        assert_eq!(unsafe { libc::kill(helper.0 as i32, 0) }, 0);
        tokio::time::timeout(
            Duration::from_secs(5),
            blocking(move || {
                state.shutdown();
                Ok(())
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let mut warned = false;
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .unwrap()
                .unwrap();
            if let crate::desktop_session::EventBody::Data(value) = event.body {
                if value["type"] == "log" {
                    warned |= value["line"].as_str().unwrap().contains("pipe stayed open");
                }
                if value["type"] == "exit" {
                    break;
                }
            }
        }
        assert!(warned);
    }

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

    #[tokio::test]
    async fn frames_round_trip() {
        let a = r#"{"jsonrpc":"2.0","id":1,"result":"ж"}"#;
        let b = r#"{"jsonrpc":"2.0","method":"x"}"#;
        let mut stream = frame(a);
        stream.extend(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n");
        stream.extend(frame(b));
        let mut reader = BufReader::new(&stream[..]);
        assert_eq!(read_message(&mut reader).await.unwrap().as_deref(), Some(a));
        assert_eq!(read_message(&mut reader).await.unwrap().as_deref(), Some(b));
        assert_eq!(read_message(&mut reader).await.unwrap(), None);
        // A frame no server would send ends the stream with an error.
        let mut huge = BufReader::new(&b"Content-Length: 99999999999\r\n\r\n"[..]);
        assert!(read_message(&mut huge).await.is_err());
    }
}
