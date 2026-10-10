// src-tauri/src/process.rs
//
// What every child process the app starts needs, whatever the platform:
// no console window popping up on Windows, and on macOS/Linux the PATH the
// user's shell would have, not the bare one a desktop launcher hands a GUI
// app.

use std::process::Command;

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
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

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
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = String::from_utf8_lossy(&reader.join().ok()?).into_owned();
    parse_marked(&out)
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

    #[test]
    fn asks_the_login_shell() {
        if std::env::var("SHELL").map_or(true, |s| s.is_empty()) {
            return;
        }
        let path = login_shell_path().expect("the login shell's PATH");
        assert!(
            std::env::split_paths(&path).any(|d| d.is_absolute()),
            "{path}"
        );
    }
}
