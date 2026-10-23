//! Git hooks: the programs in a repository's hooks folder that git runs
//! around what it does (pre-commit, commit-msg, pre-push, …). libgit2 runs
//! none of them, so the Git panel runs them itself, where git would and
//! with git's arguments and input. A hook that runs before an action and
//! fails stops it, as it stops git, and what the hook printed is the error
//! the panel shows. A hook that runs after (post-commit, post-checkout, …)
//! can't undo anything: its failure is only logged, as git ignores it.
//!
//! Where they are: `core.hooksPath` when it's set (husky, lefthook and the
//! like set it; a relative path is from the working folder), else `hooks/`
//! in the repository's git folder (the main one's, for a worktree). Each
//! runs in the working folder, as git runs it.
//!
//! How they run:
//! - macOS, Linux: the file itself, which must be executable (`chmod +x`),
//!   as git requires: one that isn't is skipped, as git skips it. A script
//!   without a `#!` line runs with /bin/sh, as git runs it.
//! - Windows: through the `sh` of Git for Windows, which reads the `#!` line
//!   as git's own does (`#!/bin/sh`, `#!/usr/bin/env node`, …), with its
//!   tools (grep, sed, …) on the PATH. A `<name>.exe` runs as it is. With no
//!   Git for Windows a script hook can't run: the action stops and says so,
//!   rather than going on as if the hook had passed.

use git2::Repository;
use rovik_core::agent_run::command_output::{cap_output, clean_output};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The most of a hook's output kept while it runs: the end, where errors are.
const MAX_OUTPUT: usize = 1024 * 1024;

/// A repository's hooks.
pub(crate) struct Hooks {
    /// The folder they're in (it may not exist).
    dir: PathBuf,
    /// Where they run: the working folder (the git folder, when bare).
    cwd: PathBuf,
}

impl Hooks {
    pub fn of(repo: &Repository) -> Hooks {
        let cwd = repo.workdir().unwrap_or(repo.path()).to_path_buf();
        // get_path expands `~/`.
        let configured = repo
            .config()
            .ok()
            .and_then(|c| c.get_path("core.hooksPath").ok())
            .filter(|p| !p.as_os_str().is_empty());
        let dir = match configured {
            Some(p) if p.is_relative() => cwd.join(p),
            Some(p) => p,
            None => repo.commondir().join("hooks"),
        };
        Hooks { dir, cwd }
    }

    /// The program of hook `name`, when there's one that runs.
    fn find(&self, name: &str) -> Option<PathBuf> {
        let path = self.dir.join(name);
        if path.is_file() {
            if is_executable(&path) {
                return Some(path);
            }
            log::info!(
                "git hook {} isn't executable: skipped, as git skips it",
                path.display()
            );
            return None;
        }
        // Git for Windows runs `<name>.exe` too.
        let exe = self.dir.join(format!("{name}.exe"));
        (cfg!(windows) && exe.is_file()).then_some(exe)
    }

    pub fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// Runs hook `name` when the repository has it, with `args` and, on its
    /// standard input, `input`. An error when it fails, with what it printed.
    pub fn run(&self, name: &str, args: &[&str], input: Option<&[u8]>) -> Result<(), String> {
        let Some(program) = self.find(name) else {
            return Ok(());
        };
        let child = self
            .spawn(&program, args, input.is_some())
            .map_err(|e| format!("the {name} hook couldn't start: {e}"))?;
        let (status, output) =
            collect(child, input).map_err(|e| format!("the {name} hook: {e}"))?;
        if status.success() {
            return Ok(());
        }
        Err(failure_text(name, status, &output))
    }

    /// Runs a hook that comes after the action: it can't stop it, so a
    /// failure is only logged.
    pub fn run_after(&self, name: &str, args: &[&str], input: Option<&[u8]>) {
        if let Err(e) = self.run(name, args, input) {
            log::warn!("git: {e}");
        }
    }

    /// `file` as a hook is given it: from the working folder when it's in
    /// it (`.git/COMMIT_EDITMSG`, as git gives it), else the whole path; with
    /// `/` on Windows, which both a shell script and node read.
    fn path_arg(&self, file: &Path) -> String {
        let path = file.strip_prefix(&self.cwd).unwrap_or(file);
        let text = path.to_string_lossy();
        if cfg!(windows) {
            text.replace('\\', "/")
        } else {
            text.into_owned()
        }
    }

    /// The message a commit gets once `prepare-commit-msg` and `commit-msg`
    /// have had it. git hands it to them in COMMIT_EDITMSG, which they may
    /// change, and reads it back. `source` is what prepare-commit-msg is told
    /// it came from: "message" (typed), "merge". With neither hook, the
    /// message as it is.
    pub fn commit_message(
        &self,
        repo: &Repository,
        message: &str,
        source: &str,
    ) -> Result<String, String> {
        if !self.has("prepare-commit-msg") && !self.has("commit-msg") {
            return Ok(message.to_string());
        }
        let file = repo.path().join("COMMIT_EDITMSG");
        let mut text = message.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
        let arg = self.path_arg(&file);
        self.run("prepare-commit-msg", &[&arg, source], None)?;
        self.run("commit-msg", &[&arg], None)?;
        let edited =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        // Typed, not edited: git's cleanup is "whitespace", which keeps `#` lines.
        let cleaned = git2::message_prettify(edited, None).map_err(|e| e.message().to_string())?;
        if cleaned.trim().is_empty() {
            return Err("the commit message the hooks left is empty.".to_string());
        }
        Ok(cleaned)
    }

    /// The command for `program` with `args`, as it runs in the working folder.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>, piped_input: bool) -> Command {
        let mut cmd = rovik_core::process::command(program);
        cmd.current_dir(&self.cwd)
            // What git tells a hook when no editor is coming.
            .env("GIT_EDITOR", ":")
            .stdin(if piped_input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        rovik_core::process::no_window(&mut cmd);
        // No terminal to wait on: a hook asking on /dev/tty fails at once
        // rather than waiting for an answer no one can give.
        rovik_core::process::own_session(&mut cmd);
        cmd
    }

    #[cfg(unix)]
    fn spawn(&self, program: &Path, args: &[&str], piped_input: bool) -> Result<Child, String> {
        let mut tries = 0;
        loop {
            match self.command(program, piped_input).args(args).spawn() {
                // A script with no `#!` line: git runs it with sh.
                Err(e) if e.raw_os_error() == Some(libc::ENOEXEC) => {
                    return self
                        .command("/bin/sh", piped_input)
                        .arg(program)
                        .args(args)
                        .spawn()
                        .map_err(|e| e.to_string())
                }
                // Just written, and still open in a process another thread
                // was starting: it's free a moment later.
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 10 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                other => return other.map_err(|e| e.to_string()),
            }
        }
    }

    #[cfg(windows)]
    fn spawn(&self, program: &Path, args: &[&str], piped_input: bool) -> Result<Child, String> {
        let is_exe = program
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
        if is_exe {
            return self
                .command(program, piped_input)
                .args(args)
                .spawn()
                .map_err(|e| e.to_string());
        }
        let git = git_for_windows().ok_or(
            "it's a script, and running one takes the sh of Git for Windows \
             (https://git-scm.com/download/win), which wasn't found",
        )?;
        let mut cmd = self.command(&git.sh, piped_input);
        // sh's exec reads the `#!` line, as Git for Windows does for its hooks.
        cmd.arg("-c")
            .arg(r#"exec "$0" "$@""#)
            .arg(program.to_string_lossy().replace('\\', "/"))
            .args(args);
        let path = rovik_core::process::child_var("PATH").unwrap_or_default();
        let dirs = git
            .tools
            .iter()
            .cloned()
            .chain(std::env::split_paths(&path));
        if let Ok(path) = std::env::join_paths(dirs) {
            cmd.env("PATH", path);
        }
        cmd.spawn().map_err(|e| e.to_string())
    }
}

/// The start of the error a failed hook gives: what it printed, the end of
/// it when it printed a lot. Callers put what didn't happen before it.
fn failure_text(name: &str, status: ExitStatus, output: &str) -> String {
    let how = match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "stopped by a signal".to_string(),
    };
    let output = cap_output(clean_output(output).trim(), 10, 60, 8000);
    if output.is_empty() {
        format!("the {name} hook failed ({how}).")
    } else {
        format!("the {name} hook failed ({how}):\n{output}")
    }
}

/// Waits for the hook, its output and errors read together as it prints
/// them, and `input` written to it.
fn collect(mut child: Child, input: Option<&[u8]>) -> Result<(ExitStatus, String), String> {
    let output = Arc::new(Mutex::new(Vec::<u8>::new()));
    let (done, finished) = mpsc::channel::<()>();
    let pipes: Vec<Box<dyn Read + Send>> = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let readers = pipes.len();
    for mut pipe in pipes {
        let output = output.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n @ 1..) = pipe.read(&mut buf) {
                let mut out = output.lock().unwrap_or_else(|e| e.into_inner());
                out.extend_from_slice(&buf[..n]);
                if out.len() > MAX_OUTPUT {
                    let extra = out.len() - MAX_OUTPUT;
                    out.drain(..extra);
                }
            }
            let _ = done.send(());
        });
    }
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        // A hook that doesn't read it closes the pipe: that's fine.
        let _ = stdin.write_all(input);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    // Something the hook left running in the background can hold its
    // output open for ever: what it printed so far will do.
    let deadline = Instant::now() + Duration::from_secs(2);
    for _ in 0..readers {
        let left = deadline.saturating_duration_since(Instant::now());
        if finished.recv_timeout(left).is_err() {
            break;
        }
    }
    let bytes = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// Windows has no executable bit: every hook file runs, as with Git for Windows.
#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// "Not committed: the pre-commit hook failed …"
pub(crate) fn not_committed(e: String) -> String {
    format!("Not committed: {e}")
}

// ---------------------------------------------------------------------------
// Git for Windows
// ---------------------------------------------------------------------------

/// Git for Windows: its sh, and the folders of the tools its hooks use.
#[cfg_attr(not(windows), allow(dead_code))]
struct GitForWindows {
    sh: PathBuf,
    tools: Vec<PathBuf>,
}

/// The folder Git for Windows is installed in, from its git.exe: that's in
/// `cmd\`, `bin\` or `mingw64\bin\` (`clangarm64\bin\` on ARM). MSYS2's own
/// git is in `usr\bin\`.
#[cfg_attr(not(windows), allow(dead_code))]
fn git_install_root(git_exe: &Path) -> Option<PathBuf> {
    let dir = git_exe.parent()?;
    let up = dir.parent()?;
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
    };
    match name(dir)?.as_str() {
        "cmd" => Some(up.to_path_buf()),
        "bin" => match name(up).as_deref() {
            Some("usr" | "mingw64" | "mingw32" | "clangarm64" | "clang64" | "ucrt64") => {
                up.parent().map(Path::to_path_buf)
            }
            _ => Some(up.to_path_buf()),
        },
        _ => None,
    }
}

/// Git for Windows installed in `root`: its sh, and its tools' folders.
#[cfg_attr(not(windows), allow(dead_code))]
fn installed_in(root: &Path) -> Option<GitForWindows> {
    let sh = ["usr/bin/sh.exe", "bin/sh.exe"]
        .iter()
        .map(|p| root.join(p))
        .find(|p| p.is_file())?;
    let tools = ["mingw64/bin", "clangarm64/bin", "mingw32/bin", "usr/bin"]
        .iter()
        .map(|p| root.join(p))
        .filter(|p| p.is_dir())
        .collect();
    Some(GitForWindows { sh, tools })
}

/// Where Git for Windows is: next to the git on the PATH, else where its
/// installer puts it. Else an sh on the PATH (MSYS2, Cygwin).
#[cfg(windows)]
fn git_for_windows() -> Option<GitForWindows> {
    let path = rovik_core::process::child_var("PATH").unwrap_or_default();
    let on_path = |file: &str| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(file))
            .find(|p| p.is_file())
    };
    let from_git = on_path("git.exe")
        .and_then(|git| git_install_root(&git))
        .and_then(|root| installed_in(&root));
    from_git
        .or_else(|| {
            ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
                .iter()
                .filter_map(|var| std::env::var_os(var))
                .map(|dir| PathBuf::from(dir).join("Git"))
                .chain(
                    std::env::var_os("LOCALAPPDATA")
                        .map(|dir| PathBuf::from(dir).join("Programs").join("Git")),
                )
                .find_map(|root| installed_in(&root))
        })
        .or_else(|| {
            let sh = on_path("sh.exe")?;
            let tools = sh.parent().map(Path::to_path_buf).into_iter().collect();
            Some(GitForWindows { sh, tools })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_for_windows_is_found_from_where_its_git_is() {
        let root = Path::new("/c/Program Files/Git");
        for git in [
            "/c/Program Files/Git/cmd/git.exe",
            "/c/Program Files/Git/bin/git.exe",
            "/c/Program Files/Git/mingw64/bin/git.exe",
            "/c/Program Files/Git/clangarm64/bin/git.exe",
            "/c/Program Files/Git/usr/bin/git.exe",
        ] {
            assert_eq!(
                git_install_root(Path::new(git)).as_deref(),
                Some(root),
                "{git}"
            );
        }
        assert_eq!(git_install_root(Path::new("/c/tools/git.exe")), None);
    }

    #[test]
    fn a_failure_shows_the_end_of_what_the_hook_printed() {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(1 << 8)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(1)
        };
        assert_eq!(
            failure_text("pre-commit", status, "\x1b[31mlint: 2 errors\x1b[0m\n"),
            "the pre-commit hook failed (exit code 1):\nlint: 2 errors"
        );
        assert_eq!(
            failure_text("pre-push", status, " \n"),
            "the pre-push hook failed (exit code 1)."
        );
    }
}
