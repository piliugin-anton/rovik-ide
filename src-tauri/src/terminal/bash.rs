//! Bash in a terminal here. Before readline 8.3 (bash 5.2 and older) bash
//! can't redraw an input line taller than the terminal: holding Backspace
//! through a long paste blanked the row the caret moved up to. Bash reads
//! `bash.sh` as it starts, in place of ~/.bashrc (`--init-file`): it reads
//! a login shell's files, then keeps such a line on one row while it's too
//! tall, with the terminal's help (src/lib/bashLine.ts), and lets it wrap
//! once it fits.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

/// What bash reads as it starts.
const SCRIPT: &str = include_str!("bash.sh");

/// A bash's version: (5, 2) for 5.2.21.
type Version = (u32, u32);

/// Whether bash `version` takes the script: from 4.4, where a key's command
/// reads the line and the prompt as it shows, to 5.2. From 5.3 on, readline
/// draws a tall line as it should.
fn takes_script(version: Version) -> bool {
    ((4, 4)..(5, 3)).contains(&version)
}

/// The version `bash --version` says: "GNU bash, version 5.2.21(1)-release
/// (x86_64-pc-linux-gnu)" is (5, 2).
fn parse_version(text: &str) -> Option<Version> {
    let (_, rest) = text.split_once("version ")?;
    let mut numbers = rest.split(|c: char| !c.is_ascii_digit());
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next()?.parse().ok()?;
    Some((major, minor))
}

/// The version of the bash at `program`, as it says; None when it doesn't
/// within a second.
fn ask_version(program: &str) -> Option<Version> {
    let mut child = rovik_core::process::command(program)
        .arg("--version")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(1) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_version(&out)
}

/// [`ask_version`], asked once a program: a terminal opens on the app's
/// main thread.
fn version(program: &str) -> Option<Version> {
    static KNOWN: Mutex<Vec<(String, Option<Version>)>> = Mutex::new(Vec::new());
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, version)) = known.iter().find(|(asked, _)| asked == program) {
        return *version;
    }
    let version = ask_version(program);
    known.push((program.to_string(), version));
    version
}

/// Writes the script to `path`, left alone when it says that already: a
/// write waits for the disk. False when it can't be written.
fn write(path: &Path) -> bool {
    if std::fs::read(path).is_ok_and(|now| now == SCRIPT.as_bytes()) {
        return true;
    }
    // Whole or not at all: a shell starting meanwhile reads one or the other.
    match rovik_core::atomic_file::write_atomic(path, SCRIPT.as_bytes()) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("terminal: couldn't write {}: {e}", path.display());
            false
        }
    }
}

/// The file `shell` reads as it starts (`--init-file`) in a terminal here,
/// when it's a bash that takes it; None for any other shell, which starts
/// as a login shell.
pub(super) fn init_file(app: &AppHandle, shell: &str) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?.join("terminal");
    // Readline's settings, which 0.1.17 wrote for every terminal (INPUTRC):
    // read by nothing now.
    let _ = std::fs::remove_file(dir.join("inputrc"));
    let name = Path::new(shell).file_name()?;
    if name != "bash" || !version(shell).is_some_and(takes_script) {
        return None;
    }
    let path = dir.join("bash.sh");
    write(&path).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("terminal-bash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bash_says_its_version() {
        let said = "GNU bash, version 5.2.21(1)-release (x86_64-pc-linux-gnu)\n\
                    Copyright (C) 2022 Free Software Foundation, Inc.";
        assert_eq!(parse_version(said), Some((5, 2)));
        assert_eq!(
            parse_version("GNU bash, version 3.2.57(1)-release (arm64-apple-darwin24)"),
            Some((3, 2))
        );
        assert_eq!(
            parse_version("GNU bash, version 5.3.0(1)-release"),
            Some((5, 3))
        );
        assert_eq!(parse_version("zsh 5.9 (x86_64-pc-linux-gnu)"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn bash_4_4_to_5_2_takes_it() {
        for old in [(3, 2), (4, 3)] {
            assert!(!takes_script(old), "{old:?}");
        }
        for takes in [(4, 4), (5, 0), (5, 1), (5, 2)] {
            assert!(takes_script(takes), "{takes:?}");
        }
        for new in [(5, 3), (5, 4), (6, 0)] {
            assert!(!takes_script(new), "{new:?}");
        }
    }

    #[test]
    fn it_is_written_once_and_again_when_it_changed() {
        let dir = temp_dir("write");
        let path = dir.join("terminal").join("bash.sh");
        assert!(write(&path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SCRIPT);
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(write(&path));
        let left = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(written, left, "written again, saying the same");
        std::fs::write(&path, "older\n").unwrap();
        assert!(write(&path));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SCRIPT);
    }

    /// What the bash on PATH makes of the script, where there's one: run
    /// interactive, as a terminal's is, in a home with a login shell's
    /// file and bracketed paste on. Bash 4.4 to 5.2 then has the keys, and
    /// its key's commands say how the line is; another reads the login file
    /// alone.
    #[test]
    fn bash_reads_the_login_files_then_binds_the_keys() {
        let dir = temp_dir("bash");
        let script = dir.join("bash.sh");
        assert!(write(&script));
        std::fs::write(dir.join(".bash_profile"), "FROM_PROFILE=yes\n").unwrap();
        // On by default from bash 5.1 on; the script needs it.
        std::fs::write(dir.join(".inputrc"), "set enable-bracketed-paste on\n").unwrap();
        // A long line, and a short one, on a 10×6 terminal with a prompt of 2.
        let check = r#"echo "profile=$FROM_PROFILE"
            echo "bash=${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}"
            bind -X 2>/dev/null | grep -c rovik | sed 's/^/keys=/'
            if declare -F __rovik_check >/dev/null && [ "$__rovik_bash" = 1 ]; then
              COLUMNS=10 LINES=6 PS1='$ ' __rovik_pw=
              READLINE_LINE=$(printf 'a%.0s' {1..100}) __rovik_check 2>&1 | od -An -c | tr -d ' \n'; echo
              READLINE_LINE=aaaaaaa __rovik_check 2>&1 | od -An -c | tr -d ' \n'; echo
            fi"#;
        let out = Command::new("bash")
            .arg("--init-file")
            .arg(&script)
            .args(["-i", "-c", check])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &dir)
            .env("TERM", super::super::TERM)
            .env("LANG", "C")
            .stdin(Stdio::null())
            .output();
        let Ok(out) = out else {
            eprintln!("no bash here: skipped");
            return;
        };
        let said = String::from_utf8_lossy(&out.stdout);
        let line = |prefix: &str| {
            said.lines()
                .find_map(|line| line.strip_prefix(prefix))
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(line("profile="), "yes", "the login file unread:\n{said}");
        let version = line("bash=");
        let mut numbers = version.split_whitespace().map(|n| n.parse().unwrap_or(0));
        let version = (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0));
        if !takes_script(version) {
            assert_eq!(line("keys="), "0", "bash {version:?} has the keys:\n{said}");
            return;
        }
        assert_eq!(
            line("keys="),
            "2",
            "bash {version:?} hasn't the keys:\n{said}"
        );
        // 100 characters and a prompt of 2 take 11 rows, 6 too many for 5:
        // 53 of them must go before it fits (the rest, 49, takes 5). 7 and
        // 2 take one.
        assert!(
            said.contains(r"033]7777;rovik;one-row;53\a"),
            "bash {version:?}: the long line not on one row:\n{said}"
        );
        assert!(
            said.contains(r"033]7777;rovik;wrap\a"),
            "bash {version:?}: the short line not wrapping:\n{said}"
        );
    }
}
