//! Bash in a terminal, here or on a cloud host. Bash, up to 5.4 in
//! development at least, can't redraw an input line taller than the
//! terminal: holding Backspace through a long paste blanked the row the
//! caret moved up to, and from 5.3 on the rows that had scrolled off the
//! top didn't come back once the line fit again. Bash reads `bash.sh` as it
//! starts, in place of ~/.bashrc (`--init-file`): it reads a login shell's
//! files, then keeps such a line on one row while it's too tall, with the
//! terminal's help (src/lib/bashLine.ts), and lets it wrap once it fits.
//!
//! Here the file goes in the app's data, and each bash's version is asked
//! once (`init_file`). On a host, a WSL distribution too, one command
//! there says what the login shell is and, for a bash that takes the file,
//! saves it in ~/.rovik/terminal (`on_host`).

#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::sync::Mutex;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use rovik_client::host_shell::HostShell;
use rovik_client::ssh::quote;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use tauri::{AppHandle, Manager};

/// What bash reads as it starts.
const SCRIPT: &str = include_str!("bash.sh");

/// A bash's version: (5, 2) for 5.2.21.
type Version = (u32, u32);

/// Whether bash `version` takes the script: from 4.4, where a key's command
/// reads the line and the prompt as it shows, to 5.4, in development as
/// 5.4.0-devel in October 2026, which garbles a tall line as 5.3 does. A
/// newer one starts as a login shell until it's known to need the script,
/// and to take it.
fn takes_script(version: Version) -> bool {
    ((4, 4)..=(5, 4)).contains(&version)
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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

/// How a terminal on a host starts: as the login shell there, or, for a
/// bash that takes the file, as that bash reading it.
#[derive(Debug, PartialEq)]
pub(super) struct HostStart {
    /// The login shell's path there: "/bin/bash".
    shell: String,
    /// Where the file is there, for a bash that takes it.
    init_file: Option<String>,
}

impl HostStart {
    /// What the terminal's tab calls it: "bash".
    pub(super) fn name(&self) -> String {
        let name = self.shell.rsplit('/').next().unwrap_or_default();
        match name.is_empty() {
            true => "shell".into(),
            false => name.into(),
        }
    }

    /// The program and its arguments, for a bash that takes the file:
    /// what a terminal there runs in place of the login shell.
    pub(super) fn argv(&self) -> Option<[&str; 3]> {
        let file = self.init_file.as_deref()?;
        Some([self.shell.as_str(), "--init-file", file])
    }

    /// [`Self::argv`] as a command for sh there.
    pub(super) fn command(&self) -> Option<String> {
        let argv = self.argv()?.map(quote);
        Some(format!("exec {}", argv.join(" ")))
    }
}

/// How long a host may take to say how its terminal starts; past that,
/// it starts as the login shell.
const ON_HOST_WAIT: Duration = Duration::from_secs(10);

/// The file's name on a host: its content's. A host that computers with
/// different Rovik versions share keeps each version's file, and one
/// that's there is the one wanted.
fn host_file_name() -> String {
    let digest = Sha256::digest(SCRIPT.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("bash-{hex}.sh")
}

/// The versions `bash --version` names that take the file, as patterns
/// for sh's `case`: "*'version 5.2.'*" for 5.2.
fn version_patterns() -> Vec<String> {
    (4..=5)
        .flat_map(|major| (0..10).map(move |minor| (major, minor)))
        .filter(|&version| takes_script(version))
        .map(|(major, minor)| format!("*'version {major}.{minor}.'*"))
        .collect()
}

/// The command a host runs, through sh, with the file on its stdin. It
/// says the login shell (`$SHELL`, as sshd and WSL set it, else the
/// account's) and, for bash, its version; a bash that takes the file gets
/// it saved in ~/.rovik/terminal, and then where it is. Whatever the file
/// isn't saved for, its stdin is read to the end: a program sending it
/// mustn't find the pipe closed. One line: `wsl.exe` passes it on too.
fn on_host_command() -> String {
    let file = format!("\"$HOME\"/.rovik/terminal/{}", host_file_name());
    let patterns = version_patterns().join("|");
    format!(
        "shell=${{SHELL:-}}; \
         [ -n \"$shell\" ] || shell=$(getent passwd \"$(id -un)\" 2>/dev/null | cut -d: -f7); \
         printf 'shell=%s\\n' \"$shell\"; \
         version=; \
         case ${{shell##*/}} in bash) version=$(LC_ALL=C \"$shell\" --version 2>/dev/null | head -n 1);; esac; \
         printf 'version=%s\\n' \"$version\"; \
         case $version in {patterns}) \
         file={file}; \
         if [ ! -s \"$file\" ]; then \
         mkdir -p \"${{file%/*}}\" && cat > \"$file.$$\" && mv -f \"$file.$$\" \"$file\" || rm -f \"$file.$$\"; \
         fi; \
         [ -s \"$file\" ] && printf 'file=%s\\n' \"$file\";; \
         esac; \
         cat > /dev/null"
    )
}

/// What a host said of its login shell: None without a shell's path.
fn parse_on_host(said: &str) -> Option<HostStart> {
    let field = |key: &str| said.lines().find_map(|line| line.strip_prefix(key));
    let shell = field("shell=").filter(|shell| shell.starts_with('/'))?;
    let takes = field("version=")
        .and_then(parse_version)
        .is_some_and(takes_script);
    let init_file = field("file=")
        .filter(|file| takes && file.starts_with('/'))
        .map(str::to_string);
    Some(HostStart {
        shell: shell.to_string(),
        init_file,
    })
}

/// How a terminal on `host` starts, as the host says; for a bash that
/// takes the file, the file saved there first. None when the host doesn't
/// say: the terminal starts as the login shell, as it always has.
pub(super) async fn on_host(host: &dyn HostShell) -> Option<HostStart> {
    let command = on_host_command();
    let said = host.run(&command, SCRIPT.as_bytes(), ON_HOST_WAIT).await;
    match said {
        Ok(output) => parse_on_host(&String::from_utf8_lossy(&output.stdout)),
        Err(e) => {
            log::warn!("terminal: the host didn't say how its shell starts: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use rovik_client::host_shell::ProcessShell;
    #[cfg(unix)]
    use std::process::Command;

    #[cfg(unix)]
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("terminal-bash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_bash_on_a_host_starts_reading_the_file_where_it_is() {
        let start = HostStart {
            shell: "/usr/local/bin/bash".into(),
            init_file: Some("/home/o'brien/.rovik/terminal/bash-0123.sh".into()),
        };
        assert_eq!(start.name(), "bash");
        assert_eq!(
            start.argv(),
            Some([
                "/usr/local/bin/bash",
                "--init-file",
                "/home/o'brien/.rovik/terminal/bash-0123.sh"
            ])
        );
        assert_eq!(
            start.command().as_deref(),
            Some(
                r"exec '/usr/local/bin/bash' '--init-file' '/home/o'\''brien/.rovik/terminal/bash-0123.sh'"
            )
        );
        // Another shell, or a bash without the file: the login shell.
        let zsh = HostStart {
            shell: "/usr/bin/zsh".into(),
            init_file: None,
        };
        assert_eq!((zsh.name().as_str(), zsh.command()), ("zsh", None));
        assert_eq!(zsh.argv(), None);
    }

    #[test]
    fn a_host_saying_its_shell_is_read() {
        let file = "/home/me/.rovik/terminal/bash-0123.sh";
        let said = format!(
            "shell=/bin/bash\nversion=GNU bash, version 5.1.16(1)-release (x86_64-pc-linux-gnu)\nfile={file}\n"
        );
        assert_eq!(
            parse_on_host(&said),
            Some(HostStart {
                shell: "/bin/bash".into(),
                init_file: Some(file.into()),
            })
        );
        // A bash that doesn't take the file has none, whatever is said.
        for other in ["3.2.57", "5.5.0"] {
            let other = said.replace("5.1.16", other);
            assert_eq!(parse_on_host(&other).unwrap().init_file, None, "{other}");
        }
        let zsh = parse_on_host("shell=/usr/bin/zsh\nversion=\n").unwrap();
        assert_eq!((zsh.name().as_str(), zsh.init_file), ("zsh", None));
        // No shell said: as before.
        assert_eq!(parse_on_host("shell=\nversion=\n"), None);
        assert_eq!(parse_on_host(""), None);
    }

    #[test]
    fn the_versions_named_on_a_host_are_those_that_take_it() {
        let patterns = version_patterns();
        for takes in ["4.4", "5.0", "5.1", "5.2", "5.3", "5.4"] {
            assert!(
                patterns.contains(&format!("*'version {takes}.'*")),
                "{takes}: {patterns:?}"
            );
        }
        for not in ["3.2", "4.3", "5.5", "6.0"] {
            assert!(
                !patterns.contains(&format!("*'version {not}.'*")),
                "{not}: {patterns:?}"
            );
        }
        let name = host_file_name();
        assert!(name.starts_with("bash-") && name.ends_with(".sh"), "{name}");
        assert_eq!(name.len(), "bash-.sh".len() + 16, "{name}");
    }

    /// A fake bash at `dir`/`name`/bash that says it's `version`.
    #[cfg(unix)]
    fn fake_bash(dir: &Path, name: &str, version: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join(name);
        std::fs::create_dir_all(&bin).unwrap();
        let bash = bin.join("bash");
        let said = format!("GNU bash, version {version}(1)-release (x86_64-pc-linux-gnu)");
        std::fs::write(&bash, format!("#!/bin/sh\necho '{said}'\n")).unwrap();
        std::fs::set_permissions(&bash, std::fs::Permissions::from_mode(0o755)).unwrap();
        bash.to_string_lossy().into_owned()
    }

    /// A host that is this machine, with `home` and the login shell `shell`.
    #[cfg(unix)]
    fn host(home: &Path, shell: &str) -> ProcessShell {
        ProcessShell {
            program: "env".into(),
            args: vec![
                format!("HOME={}", home.display()).into(),
                format!("SHELL={shell}").into(),
            ],
            name: "this machine".into(),
        }
    }

    /// The command a terminal's host runs, run by a real sh: a bash that
    /// takes the file gets it, saved once, and starts with it; another
    /// shell, or a bash that doesn't take it, gets nothing, and starts as
    /// the login shell.
    #[cfg(unix)]
    #[test]
    fn a_host_saves_the_file_for_a_bash_that_takes_it() {
        let dir = temp_dir("host");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let ask = |home: &Path, shell: &str| rovik_core::rt::block_on(on_host(&host(home, shell)));

        let bash51 = fake_bash(&dir, "51", "5.1.16");
        let start = ask(&home, &bash51).expect("the host said nothing");
        let saved = home.join(".rovik/terminal").join(host_file_name());
        assert_eq!(
            start,
            HostStart {
                shell: bash51.clone(),
                init_file: Some(saved.to_string_lossy().into_owned()),
            }
        );
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), SCRIPT);
        // Once is enough: there already, it's left alone.
        let written = std::fs::metadata(&saved).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(ask(&home, &bash51), Some(start));
        let left = std::fs::metadata(&saved).unwrap().modified().unwrap();
        assert_eq!(written, left, "saved again");
        // From the oldest that take it to the newest: the same file.
        for (name, version) in [("44", "4.4.20"), ("53", "5.3.20"), ("54", "5.4.0")] {
            let bash = fake_bash(&dir, name, version);
            let start = ask(&home, &bash).expect("the host said nothing");
            assert_eq!(start.init_file.as_deref(), Some(&*saved.to_string_lossy()));
        }

        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        for shell in [
            fake_bash(&dir, "55", "5.5.0"),
            fake_bash(&dir, "32", "3.2.57"),
            "/bin/sh".to_string(),
        ] {
            let start = ask(&other, &shell).expect("the host said nothing");
            assert_eq!(start.init_file, None, "{shell}");
            assert_eq!(start.shell, shell);
        }
        assert!(!other.join(".rovik").exists(), "a file saved for nothing");

        // A home the file can't go in: the login shell, as before.
        let not_a_folder = dir.join("file");
        std::fs::write(&not_a_folder, "").unwrap();
        let start = ask(&not_a_folder, &bash51).expect("the host said nothing");
        assert_eq!(start.init_file, None);

        // The bash here, as it says it is.
        let Ok(found) = Command::new("sh").args(["-c", "command -v bash"]).output() else {
            return;
        };
        let bash = String::from_utf8_lossy(&found.stdout).trim().to_string();
        if bash.is_empty() {
            return;
        }
        let takes = ask_version(&bash).is_some_and(takes_script);
        let start = ask(&dir.join("real"), &bash).expect("the host said nothing");
        assert_eq!(start.init_file.is_some(), takes, "{bash}: {start:?}");
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
    fn bash_4_4_to_5_4_takes_it() {
        for old in [(3, 2), (4, 3)] {
            assert!(!takes_script(old), "{old:?}");
        }
        for takes in [(4, 4), (5, 0), (5, 1), (5, 2), (5, 3), (5, 4)] {
            assert!(takes_script(takes), "{takes:?}");
        }
        for new in [(5, 5), (6, 0)] {
            assert!(!takes_script(new), "{new:?}");
        }
    }

    #[cfg(unix)]
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
    /// file and bracketed paste on. Bash 4.4 to 5.4 then has the keys, and
    /// its key's commands say how the line is; another reads the login file
    /// alone.
    #[cfg(unix)]
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
