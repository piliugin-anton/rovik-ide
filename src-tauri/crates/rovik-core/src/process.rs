// src-tauri/crates/rovik-core/src/process.rs
//
// What every child process the app starts needs, whatever the platform:
// no console window popping up on Windows, on macOS/Linux the PATH the
// user's shell would have, not the bare one a desktop launcher hands a GUI
// app, and run from an AppImage, the user's environment rather than the
// AppImage's (process/appimage.rs).

use std::ffi::{OsStr, OsString};
use std::process::Command;

#[cfg(target_os = "linux")]
mod appimage;

/// A command for `program`. When the app runs from an AppImage, it starts
/// with the user's environment ([`child_env`]) and in the folder the
/// AppImage was started from, not the AppImage's; anywhere else, as
/// `Command::new` does. What's set on it afterwards (`env`, `current_dir`)
/// comes on top. A console window on Windows is each caller's to hide
/// ([`no_window`]): the user's ssh asks for a password in one.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    for (key, value) in child_env() {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    #[cfg(target_os = "linux")]
    if let Some(dir) = appimage::current().and_then(|a| a.start_dir.as_ref()) {
        cmd.current_dir(dir);
    }
    cmd
}

/// What a program the app starts needs changed in the environment it
/// inherits: the login shell's PATH when it was read again
/// ([`refresh_login_path`]), and, when the app runs from an AppImage, its
/// launcher's changes undone. Each variable to set, or to remove (None);
/// for a command that isn't a [`Command`] (a pseudo-terminal's).
pub fn child_env() -> Vec<(OsString, Option<OsString>)> {
    let refreshed = refreshed_path();
    #[cfg(target_os = "linux")]
    if let Some(image) = appimage::current() {
        let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        if let Some(path) = &refreshed {
            match vars.iter_mut().find(|(key, _)| key == "PATH") {
                Some((_, value)) => *value = path.clone(),
                None => vars.push(("PATH".into(), path.clone())),
            }
        }
        let mut changes = image.undo(&vars);
        // One with none of the AppImage's folders is left as it is there.
        if let Some(path) = refreshed {
            if !changes.iter().any(|(key, _)| key == "PATH") {
                changes.push(("PATH".into(), Some(path)));
            }
        }
        return changes;
    }
    refreshed
        .map(|path| vec![("PATH".into(), Some(path))])
        .unwrap_or_default()
}

/// The login shell's PATH as read again since the process started
/// ([`refresh_login_path`]), with ours after it: the PATH of what the app
/// starts from then on. The process's own can't change once threads run.
#[cfg(unix)]
static REFRESHED_PATH: std::sync::RwLock<Option<OsString>> = std::sync::RwLock::new(None);

#[cfg(unix)]
fn refreshed_path() -> Option<OsString> {
    REFRESHED_PATH
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(not(unix))]
fn refreshed_path() -> Option<OsString> {
    None
}

/// Reads the login shell's PATH again, for the programs the app starts
/// from now on: a toolchain installed since the process started is found.
/// The PATH it read as it started (`adopt_login_shell_path`) stays the
/// process's own: a server's daemon starts before its host's setup script
/// installs the toolchains, and runs for weeks. Blocking: up to 3 s, for a
/// slow rc file. False when the login shell didn't say.
#[cfg(unix)]
pub fn refresh_login_path() -> bool {
    let Some(login) = login_shell_path() else {
        return false;
    };
    let ours = std::env::var_os("PATH").unwrap_or_default();
    *REFRESHED_PATH.write().unwrap_or_else(|e| e.into_inner()) = merge_paths(&login, &ours);
    true
}

/// Off Unix the PATH is the system's: nothing to read again.
#[cfg(not(unix))]
pub fn refresh_login_path() -> bool {
    false
}

/// Whether `program` (a name, as a command line starts with it) is a file
/// in a folder of the PATH the app's programs get ([`child_var`]).
pub fn on_path(program: &str) -> bool {
    let Some(path) = child_var("PATH") else {
        return false;
    };
    let names: Vec<String> = if cfg!(windows) {
        ["", ".exe", ".cmd", ".bat"]
            .iter()
            .map(|ext| format!("{program}{ext}"))
            .collect()
    } else {
        vec![program.to_string()]
    };
    std::env::split_paths(&path).any(|dir| names.iter().any(|name| dir.join(name).is_file()))
}

/// `key` as a program the app starts gets it ([`child_env`]): the PATH to
/// look its programs up on, or to give it.
pub fn child_var(key: &str) -> Option<OsString> {
    match child_env().into_iter().find(|(k, _)| k == key) {
        Some((_, value)) => value,
        None => std::env::var_os(key),
    }
}

/// Keeps a console program from opening a console window. A release build is
/// a GUI-subsystem app on Windows (main.rs), so without this every `cmd`,
/// `git` or language server it starts flashes a window. No-op elsewhere.
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Starts the command as the leader of a process group of its own, so that
/// [`kill_group`] ends what it started too (a shell's pipeline, a harness's
/// tools). No-op off Unix.
pub fn own_group(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Ends a process started with [`own_group`] and everything it started:
/// SIGTERM to its group, or SIGKILL when `force`. Off Unix, the process
/// alone is killed.
pub fn kill_group(pid: Option<u32>, child: &mut tokio::process::Child, force: bool) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|id| libc::pid_t::try_from(id).ok()) {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: a signal to a process group; no memory is involved.
        unsafe { libc::kill(-pid, signal) };
        if !force {
            return;
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.start_kill();
}

/// Starts the command with an ordinary OOM score (0), whatever its
/// parent's. On a team server the daemon runs at -500, so the kernel picks
/// it last when memory runs out, and the commands it starts go back to 0,
/// picked like anything else (5.3); raising a score needs no privilege.
/// No-op off Linux, and where the score can't be set.
pub fn ordinary_oom_score(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: between fork and exec it makes only async-signal-safe
        // calls (open, write, close), on a path and bytes in static memory.
        unsafe {
            cmd.pre_exec(|| {
                let path = c"/proc/self/oom_score_adj";
                let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                if fd >= 0 {
                    libc::write(fd, b"0".as_ptr().cast(), 1);
                    libc::close(fd);
                }
                Ok(())
            });
        }
    }
    cmd
}

/// Starts the command in a session of its own, with no controlling
/// terminal. Started from a terminal, the app is in that terminal's
/// session, and so is what it starts: an interactive shell there that isn't
/// the terminal's foreground job stops its whole process group (SIGTTIN)
/// until it is. The login shell `login_shell_path` asks for the PATH (`-i`,
/// for the rc files that set it) stopped a new window that way, its own
/// process group in the background, before it had a window; and the app
/// itself, run as a background job (`rovik-ide &`). Out of the terminal's
/// session, the command runs as it does under a desktop launcher, and
/// doesn't end with that terminal either. Not with `process_group`, which
/// makes the child a group leader, whom `setsid` refuses. No-op off Unix.
pub fn own_session(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: between fork and exec it makes one async-signal-safe call,
        // which can't fail there: a forked child leads no process group.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd
}

/// What `cmd` starts holds none of the files this process has open, but its
/// standard input and outputs: they're closed as it starts. For another
/// Rovik (the app's "New window", an update's restart): an AppImage's
/// runtime keeps its mount while any process holds a pipe it gave the app,
/// which a program the app starts inherits. Before Linux 5.11, which can't
/// do it this way, and off Linux, they're inherited as before.
pub fn close_inherited(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: between fork and exec it makes one async-signal-safe
        // system call. It marks the files to close on exec rather than
        // closing them: std's own pipe, which says when exec failed, is one.
        unsafe {
            cmd.pre_exec(|| {
                libc::syscall(
                    libc::SYS_close_range,
                    3 as libc::c_uint,
                    libc::c_uint::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                );
                Ok(())
            });
        }
    }
    cmd
}

/// The user's login shell (`$SHELL`), if set.
#[cfg(unix)]
pub fn user_shell() -> Option<String> {
    std::env::var("SHELL").ok().filter(|s| !s.trim().is_empty())
}

/// A shell's family, by its file name.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ShellKind {
    /// sh, bash, zsh, dash, ksh: run POSIX shell scripts as they are.
    Posix,
    Fish,
    /// Takes `-l` for a login shell, but not POSIX syntax (nu, pwsh, …).
    OtherLogin,
    /// Anything else: no flags assumed.
    Other,
}

#[cfg(unix)]
pub fn shell_kind(shell: &str) -> ShellKind {
    let name = std::path::Path::new(shell)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.as_str() {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" => ShellKind::Posix,
        "fish" => ShellKind::Fish,
        "nu" | "pwsh" | "xonsh" | "tcsh" | "csh" => ShellKind::OtherLogin,
        _ => ShellKind::Other,
    }
}

/// Started from Finder, the Dock or a desktop entry, the app gets a minimal
/// PATH (on macOS `/usr/bin:/bin:/usr/sbin:/sbin`): no Homebrew, nvm,
/// `~/.cargo/bin`, … The terminal panel starts a login shell and is fine,
/// but the agent's commands and language servers would not find `npm` or
/// `cargo`. This asks the user's login shell for its PATH once and puts it in
/// front of ours, so every child inherits it.
///
/// Call before anything else runs: it sets an environment variable, which is
/// only sound while the process is still single-threaded.
#[cfg(unix)]
pub fn adopt_login_shell_path() {
    if let Some(login) = login_shell_path() {
        let current = std::env::var_os("PATH").unwrap_or_default();
        if let Some(merged) = merge_paths(&login, &current) {
            std::env::set_var("PATH", merged);
        }
    }
}

#[cfg(not(unix))]
pub fn adopt_login_shell_path() {}

#[cfg(unix)]
const MARKER: &str = "__ROVIK_PATH__";

/// `$SHELL -l -i -c` printing PATH between markers: rc files may print
/// banners of their own. Gives up after a few seconds (a slow or prompting
/// rc file), leaving PATH as it was.
#[cfg(unix)]
fn login_shell_path() -> Option<String> {
    login_shell_path_within(std::time::Duration::from_secs(3))
}

/// [`login_shell_path`], giving the shell up to `limit`.
#[cfg(unix)]
fn login_shell_path_within(limit: std::time::Duration) -> Option<String> {
    // A POSIX shell prints "$PATH" as it is; fish keeps PATH as a list
    // ("$PATH" joins it with spaces); any other shell has a syntax of its
    // own, so `sh -l` reads the profile files instead.
    let shell = user_shell()?;
    let posix = format!("printf '{MARKER}%s{MARKER}' \"$PATH\"");
    let (program, args) = match shell_kind(&shell) {
        ShellKind::Posix => (
            shell,
            vec!["-l".to_string(), "-i".into(), "-c".into(), posix],
        ),
        ShellKind::Fish => (
            shell,
            vec![
                "-l".to_string(),
                "-i".into(),
                "-c".into(),
                format!("printf '{MARKER}%s{MARKER}' (string join : $PATH)"),
            ],
        ),
        ShellKind::OtherLogin | ShellKind::Other => (
            "/bin/sh".to_string(),
            vec!["-l".to_string(), "-c".into(), posix],
        ),
    };
    // Out of the terminal the app may have been started from: an
    // interactive shell would stop the app's process group otherwise.
    let mut cmd = command(&program);
    own_session(&mut cmd).args(&args);
    read_marked(cmd, limit)
}

/// Runs `cmd` and reads what it prints between the markers, giving up after
/// `limit`. The output is read as it comes, and the wait ends once it holds
/// the marked text: the pipe stays open after the shell exits while a
/// program an rc file started in the background keeps it, and waiting for
/// its end would hang the app's start for as long as that program runs.
#[cfg(unix)]
fn read_marked(mut cmd: std::process::Command, limit: std::time::Duration) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Instant;

    let deadline = Instant::now() + limit;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    // Ends at the pipe's end, or at the next read once nobody listens.
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut out = Vec::new();
    let found = loop {
        if let Some(path) = parse_marked(&String::from_utf8_lossy(&out)) {
            break Some(path);
        }
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(chunk) => out.extend(chunk),
            // The pipe's end, or out of time.
            Err(_) => break None,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    found
}

#[cfg(unix)]
fn parse_marked(out: &str) -> Option<String> {
    let start = out.find(MARKER)? + MARKER.len();
    let len = out[start..].find(MARKER)?;
    let path = out[start..start + len].trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// The login shell's entries first, then any of ours it doesn't have.
/// None when nothing would change.
#[cfg(unix)]
fn merge_paths(login: &str, current: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::split_paths(login).collect();
    for dir in std::env::split_paths(current) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    let merged = std::env::join_paths(dirs).ok()?;
    (merged != current).then_some(merged)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// A program an rc file starts in the background keeps the output
    /// pipe open after the shell exits: the PATH is read all the same, and
    /// the wait doesn't last as long as that program.
    #[test]
    fn a_background_program_holding_the_output_doesnt_hold_the_start() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            &format!("sleep 8 & printf '{MARKER}/x/bin:/usr/bin{MARKER}'"),
        ]);
        let started = std::time::Instant::now();
        let path = read_marked(cmd, std::time::Duration::from_secs(10));
        assert_eq!(path.as_deref(), Some("/x/bin:/usr/bin"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));

        // Nothing marked, and the pipe held: out of time, PATH left as it was.
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 8 & echo banner"]);
        let started = std::time::Instant::now();
        assert_eq!(
            read_marked(cmd, std::time::Duration::from_millis(500)),
            None
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn finds_path_between_markers_despite_banners() {
        let out = format!("Welcome!\n{MARKER}/opt/homebrew/bin:/usr/bin{MARKER}bye\n");
        assert_eq!(
            parse_marked(&out).as_deref(),
            Some("/opt/homebrew/bin:/usr/bin")
        );
        assert_eq!(parse_marked("no markers"), None);
        assert_eq!(parse_marked(&format!("{MARKER}{MARKER}")), None);
    }

    #[test]
    fn login_entries_come_first_and_ours_are_kept() {
        let merged =
            merge_paths("/opt/homebrew/bin:/usr/bin", OsStr::new("/usr/bin:/bin")).unwrap();
        assert_eq!(merged, "/opt/homebrew/bin:/usr/bin:/bin");
        assert_eq!(
            merge_paths("/usr/bin:/bin", OsStr::new("/usr/bin:/bin")),
            None
        );
    }

    #[test]
    fn shells_by_family() {
        assert_eq!(shell_kind("/bin/zsh"), ShellKind::Posix);
        assert_eq!(shell_kind("/usr/local/bin/bash"), ShellKind::Posix);
        assert_eq!(shell_kind("/opt/homebrew/bin/fish"), ShellKind::Fish);
        assert_eq!(shell_kind("/usr/bin/nu"), ShellKind::OtherLogin);
        assert_eq!(shell_kind("/usr/bin/elvish"), ShellKind::Other);
    }

    /// A command of its own session leads it: out of ours, and out of the
    /// terminal the app was started from, whose foreground it would wait
    /// for (and stop the app's process group meanwhile) were it an
    /// interactive shell in that terminal's session.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_command_in_a_session_of_its_own_leads_it() {
        let out = own_session(&mut Command::new("sh"))
            .args([
                "-c",
                "read -r _ _ _ _ _ sid _ < /proc/self/stat; echo \"$$ $sid\"",
            ])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let ids: Vec<i32> = text
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect();
        let [pid, sid] = ids[..] else {
            panic!("{text}");
        };
        assert_eq!(sid, pid, "it leads its session");
        // SAFETY: getsid(0) only reads this process's session.
        assert_ne!(sid, unsafe { libc::getsid(0) }, "not ours");
    }

    /// Another Rovik holds none of this process's files: a pipe left open
    /// across exec, as an AppImage's runtime leaves the one its mount stays
    /// for, is open in a program started as usual, and not in one started
    /// with `close_inherited`.
    #[cfg(target_os = "linux")]
    #[test]
    fn another_rovik_holds_none_of_this_ones_files() {
        // A kernel before 5.11 has no close_range: there's nothing to see.
        // SAFETY: marks no file, a range far above any open one.
        let supported = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                900_000 as libc::c_uint,
                900_001 as libc::c_uint,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        } == 0;
        if !supported {
            return;
        }
        let mut ends = [0; 2];
        // SAFETY: a pipe whose ends a child inherits (no O_CLOEXEC), closed
        // below.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        let open_in = |cmd: &mut Command| {
            let out = cmd
                .args(["-c", &format!("test -e /proc/self/fd/{}", ends[0])])
                .status()
                .unwrap();
            out.success()
        };
        let inherited = open_in(&mut Command::new("sh"));
        let closed = !open_in(close_inherited(&mut Command::new("sh")));
        // SAFETY: the pipe's own ends, closed once.
        unsafe {
            libc::close(ends[0]);
            libc::close(ends[1]);
        }
        assert!(inherited, "a program the app starts inherits it");
        assert!(closed, "another Rovik doesn't");
    }

    /// A program is on the PATH the app's programs get when a folder of it
    /// has the file: `sh` is, a name nothing has isn't.
    #[test]
    fn a_program_is_found_on_the_path() {
        assert!(on_path("sh"));
        assert!(!on_path("rovik-no-such-program"));
    }

    /// The shell is asked and its PATH read. Not how quickly: in a full
    /// test run on a CI machine, a login shell's rc files can take longer
    /// than the 3 s the app gives them.
    #[test]
    fn asks_the_login_shell() {
        if std::env::var("SHELL").map_or(true, |s| s.is_empty()) {
            return;
        }
        let path = login_shell_path_within(std::time::Duration::from_secs(60))
            .expect("the login shell's PATH");
        assert!(
            std::env::split_paths(&path).any(|d| d.is_absolute()),
            "{path}"
        );
    }
}
