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

use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::util::{blocking, ErrText, Registry};
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
pub(crate) struct PtyInstance {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
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

/// Live terminals by id.
pub(crate) type TerminalState = Registry<Session>;

/// The app's terminals, none yet.
pub fn terminals() -> TerminalState {
    Registry::new("terminal")
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

/// What readline, bash's line editor, reads first in a terminal here
/// (INPUTRC), before the user's own settings.
#[cfg(unix)]
mod readline {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use tauri::{AppHandle, Manager};

    /// Before readline 8.3 (bash 5.2 and older) bash can't redraw an input
    /// line taller than the terminal: holding Backspace through a long
    /// paste, each row the caret moved up to went blank, and a pasted block
    /// of lines came out garbled. A line kept on one row that scrolls
    /// sideways (`horizontal-scroll-mode`) is never taller. From 8.3 on a
    /// long line wraps, as it did. Bash 5.0's and 5.1's readline compare
    /// versions the wrong way round: there `version >= 8.3` holds and
    /// `version >= 8.0` doesn't, which keeps the line scrolling. One older
    /// than 8.0 takes `version` for a program's name, not bash's, and skips
    /// both.
    const DEFAULTS: &str = "\
# Rovik IDE's terminals read this first (INPUTRC), then your own readline
# settings, which win. Bash before 5.3 garbles an input line taller than the
# terminal: there the line stays on one row that scrolls sideways.
$if Bash
set horizontal-scroll-mode on
$if version >= 8.3
$if version >= 8.0
set horizontal-scroll-mode off
$endif
$endif
$endif
";

    /// The init file readline would read for the user, found as readline
    /// finds it: their INPUTRC, else ~/.inputrc if there is one, else
    /// /etc/inputrc. `ours` is the file that includes it: when INPUTRC names
    /// that already (a Rovik started from one of its terminals), the search
    /// goes on, as readline includes without a limit and bash would crash
    /// including ours in itself.
    fn users_inputrc(
        inputrc: Option<OsString>,
        home: Option<PathBuf>,
        ours: &Path,
    ) -> Option<PathBuf> {
        let named = inputrc.filter(|name| !name.is_empty()).map(PathBuf::from);
        if let Some(named) = named.filter(|name| name != ours) {
            return Some(named);
        }
        home.map(|home| home.join(".inputrc"))
            .filter(|file| file.is_file())
            .or_else(|| Some(PathBuf::from("/etc/inputrc")).filter(|file| file.is_file()))
    }

    /// The file: the defaults, then `users`, the user's own init file. None
    /// when its name can't go on an `$include` line (a line break in it, or
    /// space before it): the user's settings are never left out.
    fn text(users: Option<&Path>) -> Option<Vec<u8>> {
        let mut text = DEFAULTS.as_bytes().to_vec();
        if let Some(users) = users {
            let name = users.as_os_str().as_bytes();
            if name.contains(&b'\n') || name.first().is_some_and(u8::is_ascii_whitespace) {
                return None;
            }
            text.extend_from_slice(b"$include ");
            text.extend_from_slice(name);
            text.push(b'\n');
        }
        Some(text)
    }

    /// Writes `path` for a user whose INPUTRC is `inputrc` and whose home is
    /// `home`. Left alone when it says that already: a write waits for the
    /// disk, and a terminal opens on the app's main thread. False when it
    /// can't be written: readline then reads the user's own settings, as
    /// anywhere else.
    fn write(path: &Path, inputrc: Option<OsString>, home: Option<PathBuf>) -> bool {
        let Some(text) = text(users_inputrc(inputrc, home, path).as_deref()) else {
            return false;
        };
        if std::fs::read(path).is_ok_and(|now| now == text) {
            return true;
        }
        // Whole or not at all: a shell starting meanwhile reads one or the other.
        match rovik_core::atomic_file::write_atomic(path, &text) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("terminal: couldn't write {}: {e}", path.display());
                false
            }
        }
    }

    /// The file, in the app's data, for the user a terminal's shell runs
    /// as; None when there's none to give.
    pub(super) fn init_file(app: &AppHandle) -> Option<PathBuf> {
        use rovik_core::process::child_var;
        let path = app
            .path()
            .app_data_dir()
            .ok()?
            .join("terminal")
            .join("inputrc");
        let home = child_var("HOME")
            .map(PathBuf::from)
            .or_else(|| app.path().home_dir().ok());
        write(&path, child_var("INPUTRC"), home).then_some(path)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::{Command, Stdio};

        fn temp_dir(tag: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("terminal-inputrc-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn the_users_own_settings_are_found_as_readline_finds_them() {
            let dir = temp_dir("find");
            let ours = dir.join("ours");
            let home = dir.join("home");
            std::fs::create_dir_all(&home).unwrap();
            let system = Some(PathBuf::from("/etc/inputrc")).filter(|f| f.is_file());
            // No INPUTRC and no ~/.inputrc: the system's, if it has one.
            assert_eq!(users_inputrc(None, Some(home.clone()), &ours), system);
            assert_eq!(
                users_inputrc(Some("".into()), Some(home.clone()), &ours),
                system
            );
            // ~/.inputrc once there is one.
            std::fs::write(home.join(".inputrc"), "set bell-style none\n").unwrap();
            assert_eq!(
                users_inputrc(None, Some(home.clone()), &ours),
                Some(home.join(".inputrc"))
            );
            // INPUTRC first, as it is, whether or not its file is there (readline
            // reads nothing else then either).
            let named = users_inputrc(
                Some("~/.config/readline/inputrc".into()),
                Some(home.clone()),
                &ours,
            );
            assert_eq!(named, Some(PathBuf::from("~/.config/readline/inputrc")));
            // Ours already (Rovik started in one of its terminals): never itself.
            assert_eq!(
                users_inputrc(Some(ours.clone().into()), Some(home.clone()), &ours),
                Some(home.join(".inputrc"))
            );
        }

        #[test]
        fn the_file_says_the_setting_then_includes_the_users_own() {
            let with =
                String::from_utf8(text(Some(Path::new("/home/a b/.inputrc"))).unwrap()).unwrap();
            assert!(with.starts_with(DEFAULTS), "{with}");
            assert!(
                with.ends_with("$endif\n$include /home/a b/.inputrc\n"),
                "{with}"
            );
            assert_eq!(text(None).unwrap(), DEFAULTS.as_bytes());
            // A name no `$include` line can say: no file, so readline reads theirs.
            assert_eq!(text(Some(Path::new("/home/a\nb/.inputrc"))), None);
            assert_eq!(text(Some(Path::new(" inputrc"))), None);
        }

        #[test]
        fn it_is_written_once_and_again_when_the_users_settings_move() {
            let dir = temp_dir("write");
            let path = dir.join("terminal").join("inputrc");
            let home = dir.join("home");
            std::fs::create_dir_all(&home).unwrap();
            assert!(write(&path, Some("/a/inputrc".into()), Some(home.clone())));
            let first = std::fs::read_to_string(&path).unwrap();
            assert!(first.ends_with("$include /a/inputrc\n"), "{first}");
            assert!(write(&path, Some("/b/inputrc".into()), Some(home)));
            let second = std::fs::read_to_string(&path).unwrap();
            assert!(second.ends_with("$include /b/inputrc\n"), "{second}");
        }

        /// Bash's own answer, where there's a bash: what `bind -v` says with
        /// the file, which reads without an error and keeps the user's own
        /// settings. Its readline's version is taken to be bash's (5.2 has
        /// 8.2), as bash builds its own. One bash says both, the one on PATH
        /// (macOS has two: its own 3.2, and Homebrew's).
        #[test]
        fn bash_scrolls_a_long_line_before_readline_8_3_and_keeps_the_users_settings() {
            let dir = temp_dir("bash");
            let users = dir.join("users-inputrc");
            std::fs::write(&users, "set bell-style visible\n").unwrap();
            let path = dir.join("inputrc");
            assert!(write(&path, Some(users.into()), Some(dir.clone())));
            let out = Command::new("bash")
                .args([
                    "--norc",
                    "--noprofile",
                    "-c",
                    "bind -v; echo \"bash=${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}\"",
                ])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", &dir)
                .env("TERM", "dumb")
                .env("INPUTRC", &path)
                .stdin(Stdio::null())
                .output();
            let Ok(out) = out else {
                eprintln!("no bash here: skipped");
                return;
            };
            let said = String::from_utf8_lossy(&out.stdout);
            let version = said.lines().find_map(|line| line.strip_prefix("bash="));
            let mut parts = version
                .unwrap_or_default()
                .split_whitespace()
                .map(|n| n.parse::<u32>().unwrap_or(0));
            let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
            let errors = String::from_utf8_lossy(&out.stderr);
            assert!(
                !errors.contains("readline:"),
                "readline couldn't read it: {errors}"
            );
            assert!(
                said.contains("set bell-style visible"),
                "the user's settings were left out:\n{said}"
            );
            let scrolls = (major, minor) < (5, 3);
            let want = format!(
                "set horizontal-scroll-mode {}",
                if scrolls { "on" } else { "off" }
            );
            assert!(
                said.contains(&want),
                "bash {major}.{minor}: not `{want}`:\n{said}"
            );
        }
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
    // Readline's settings (`readline`), then the user's own: for the shell,
    // and for a bash run in it.
    #[cfg(unix)]
    if let Some(inputrc) = readline::init_file(&app) {
        cmd.env("INPUTRC", inputrc);
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
    let ssh = Arc::new(Ssh::connect(&target).await.err_text()?);
    // Its name first: what the shell prints waits unread until its output
    // is pumped, and enough of it would hold up the answer.
    let name = ssh.login_shell().await;
    let opened = ssh.shell(TERM, size.cols.into(), size.rows.into()).await;
    let (shell, mut output) = opened.err_text()?;
    let id = Uuid::new_v4().to_string();
    state.insert(id.clone(), Session::Ssh(shell));
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
    let pair = native_pty_system().openpty(size).err_text()?;

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

    let mut child = pair.slave.spawn_command(cmd).err_text()?;
    // Drop our copy of the slave end now that the child owns it, or the
    // master's reader never sees EOF when the child exits.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().err_text()?;
    let writer = pair.master.take_writer().err_text()?;
    let killer = child.clone_killer();

    let id = Uuid::new_v4().to_string();
    state.insert(
        id.clone(),
        Session::Pty(PtyInstance {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
        }),
    );

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
            app.state::<TerminalState>().remove(&exit_id);
            // Let the last output arrive first. Bounded: a background job
            // that inherited the pty can keep the reader open indefinitely.
            let _ = drained_rx.recv_timeout(Duration::from_millis(500));
            let _ = on_event.send(TerminalEvent::Exit { code });
        })
        .err_text()?;

    Ok(id)
}

/// Off the async threads: a write to a full pty buffer (a big paste into a
/// program that isn't reading) blocks until the program reads.
#[tauri::command]
pub async fn write_to_terminal(
    state: State<'_, TerminalState>,
    id: String,
    data: String,
) -> Result<(), String> {
    let session = state.get(&id)?;
    blocking(move || session.write(data.as_bytes())).await
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
