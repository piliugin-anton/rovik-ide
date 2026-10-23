//! The Git panel's "Git hooks" view: which of the hooks the panel runs
//! (hooks.rs) the repository has, and making one from a template, making
//! one executable, and moving one to the trash. The file tree doesn't show
//! `.git`, so this is where its hooks are seen and opened.
//!
//! The folder is the one hooks run from: `.git/hooks`, or `core.hooksPath`.
//! With husky 9 that's its generated `.husky/_`, whose files only call the
//! hooks people write in `.husky/`: those are the ones shown, and husky
//! runs them with sh, so they needn't be executable.

use super::hooks::{is_executable, HooksDir};
use super::open_repo;
use crate::util::blocking;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The hooks the panel runs: their name, when they run, and whether they
/// can stop what's being done.
const HOOKS: [(&str, &str, bool); 10] = [
    ("pre-commit", "Before a commit. Failing stops it.", true),
    (
        "prepare-commit-msg",
        "Before the commit message is used, which it can change ($1 is its file, $2 where it came from).",
        true,
    ),
    (
        "commit-msg",
        "Checks the commit message ($1 is its file). Failing stops the commit.",
        true,
    ),
    ("post-commit", "After a commit.", false),
    (
        "pre-merge-commit",
        "Before the commit of a pull's merge. Failing stops it, leaving the merge to continue or abort.",
        true,
    ),
    ("post-merge", "After a pull ($1 is 0).", false),
    (
        "pre-push",
        "Before a push ($1 is the remote, $2 its URL; a line per branch on its input). Failing stops it.",
        true,
    ),
    (
        "post-checkout",
        "After switching branch ($1 and $2 are the commits before and after, $3 is 1).",
        false,
    ),
    (
        "pre-rebase",
        "Before a rebase ($1 is the branch rebased onto). Failing stops it.",
        true,
    ),
    (
        "post-rewrite",
        "After a rebase ($1 is \"rebase\"; a line `<old> <new>` per commit on its input).",
        false,
    ),
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHooks {
    /// The folder the hooks are in (it may not exist yet).
    pub dir: String,
    /// The folder as shown: from the working folder when it's in it.
    pub shown_dir: String,
    /// "default" (.git/hooks) | "hooksPath" (core.hooksPath) | "husky".
    pub source: &'static str,
    /// A hook must be executable to run (macOS, Linux; not husky's).
    pub needs_executable: bool,
    pub hooks: Vec<GitHook>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHook {
    pub name: &'static str,
    /// When it runs, and what it's given.
    pub when: &'static str,
    /// It runs before, and failing stops the action.
    pub blocks: bool,
    /// "active" | "not-executable" (git skips it) | "sample" (only git's
    /// example, `<name>.sample`, which doesn't run) | "missing".
    pub state: &'static str,
    /// The hook's file, when there is one.
    pub path: Option<String>,
}

/// Where the hooks people write are, and how they run.
struct Place {
    dir: PathBuf,
    cwd: PathBuf,
    source: &'static str,
    needs_executable: bool,
}

fn place(path: &str) -> Result<Place, String> {
    let repo = open_repo(path)?;
    let HooksDir {
        dir,
        configured,
        cwd,
    } = HooksDir::of(&repo);
    // husky 9: core.hooksPath is `.husky/_`, with its runner `h` in it.
    let husky = configured && dir.file_name().is_some_and(|n| n == "_") && dir.join("h").is_file();
    Ok(match (husky, dir.parent()) {
        (true, Some(parent)) => Place {
            dir: parent.to_path_buf(),
            cwd,
            source: "husky",
            needs_executable: false,
        },
        _ => Place {
            dir,
            cwd,
            source: if configured { "hooksPath" } else { "default" },
            needs_executable: cfg!(unix),
        },
    })
}

/// `name`, when it's one of the hooks the panel runs.
fn known(name: &str) -> Result<&'static (&'static str, &'static str, bool), String> {
    HOOKS
        .iter()
        .find(|(n, ..)| *n == name)
        .ok_or_else(|| format!("{name}: not a hook the Git panel runs"))
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl Place {
    /// The hook's file: `<name>`, or on Windows `<name>.exe`.
    fn file(&self, name: &str) -> Option<PathBuf> {
        let path = self.dir.join(name);
        if path.is_file() {
            return Some(path);
        }
        let exe = self.dir.join(format!("{name}.exe"));
        (cfg!(windows) && exe.is_file()).then_some(exe)
    }

    fn hook(&self, &(name, when, blocks): &(&'static str, &'static str, bool)) -> GitHook {
        let (state, path) = match self.file(name) {
            Some(file) if self.needs_executable && !is_executable(&file) => {
                ("not-executable", Some(file))
            }
            Some(file) => ("active", Some(file)),
            None if self.dir.join(format!("{name}.sample")).is_file() => ("sample", None),
            None => ("missing", None),
        };
        GitHook {
            name,
            when,
            blocks,
            state,
            path: path.as_deref().map(text),
        }
    }
}

#[tauri::command]
pub async fn git_hooks(path: String) -> Result<GitHooks, String> {
    blocking(move || git_hooks_blocking(&path)).await
}

pub fn git_hooks_blocking(path: &str) -> Result<GitHooks, String> {
    let place = place(path)?;
    let shown = place.dir.strip_prefix(&place.cwd).unwrap_or(&place.dir);
    let shown = text(shown).replace('\\', "/");
    Ok(GitHooks {
        dir: text(&place.dir),
        shown_dir: if shown.is_empty() { ".".into() } else { shown },
        source: place.source,
        needs_executable: place.needs_executable,
        hooks: HOOKS.iter().map(|h| place.hook(h)).collect(),
    })
}

/// A new hook: a shell script that does nothing yet, with a note of when it
/// runs, executable. Its path; an existing hook is left as it is.
#[tauri::command]
pub async fn git_hook_create(path: String, name: String) -> Result<String, String> {
    blocking(move || git_hook_create_blocking(&path, &name)).await
}

pub fn git_hook_create_blocking(path: &str, name: &str) -> Result<String, String> {
    let &(name, when, blocks) = known(name)?;
    let place = place(path)?;
    if let Some(existing) = place.file(name) {
        return Ok(text(&existing));
    }
    std::fs::create_dir_all(&place.dir).map_err(|e| format!("{}: {e}", place.dir.display()))?;
    let file = place.dir.join(name);
    let stop = if blocks {
        "# Exit with a code other than 0 to stop it: what it prints shows in the\n\
         # Git panel. "
    } else {
        "# "
    };
    // LF line ends whatever the platform: sh reads a CR as part of a word.
    let script = format!(
        "#!/bin/sh\n\
         # {name}: {when}\n\
         {stop}It runs in the repository's working folder.\n\
         \n\
         exit 0\n"
    );
    std::fs::write(&file, script).map_err(|e| format!("{}: {e}", file.display()))?;
    make_executable(&file)?;
    Ok(text(&file))
}

/// Makes hook `name` executable (`chmod +x`): git skips one that isn't.
#[tauri::command]
pub async fn git_hook_make_executable(path: String, name: String) -> Result<(), String> {
    blocking(move || git_hook_make_executable_blocking(&path, &name)).await
}

pub fn git_hook_make_executable_blocking(path: &str, name: &str) -> Result<(), String> {
    let &(name, ..) = known(name)?;
    let file = place(path)?
        .file(name)
        .ok_or_else(|| format!("There's no {name} hook."))?;
    make_executable(&file)
}

/// Moves hook `name` to the trash, from where it can be put back.
#[tauri::command]
pub async fn git_hook_delete(path: String, name: String) -> Result<(), String> {
    blocking(move || git_hook_delete_blocking(&path, &name)).await
}

pub fn git_hook_delete_blocking(path: &str, name: &str) -> Result<(), String> {
    let &(name, ..) = known(name)?;
    let Some(file) = place(path)?.file(name) else {
        return Ok(());
    };
    crate::filesystem::to_trash(&file)
        .map_err(|e| format!("{name}: couldn't move it to the trash: {e}"))
}

/// Executable by whoever may read it, as `chmod +x` makes it. No-op off Unix.
fn make_executable(file: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file
            .metadata()
            .map_err(|e| format!("{}: {e}", file.display()))?
            .permissions();
        let mode = permissions.mode();
        permissions.set_mode(mode | ((mode & 0o444) >> 2));
        std::fs::set_permissions(file, permissions)
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;
    use std::fs;

    fn repo(tag: &str) -> (PathBuf, Repository) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "git-hook-files-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        // Whatever the machine's global config says.
        repo.config()
            .unwrap()
            .set_str("core.hooksPath", ".git/hooks")
            .unwrap();
        (dir, repo)
    }

    fn p(dir: &Path) -> String {
        text(dir)
    }

    fn state<'a>(hooks: &'a GitHooks, name: &str) -> (&'a str, Option<&'a str>) {
        let hook = hooks.hooks.iter().find(|h| h.name == name).unwrap();
        (hook.state, hook.path.as_deref())
    }

    #[test]
    fn a_new_hook_runs_at_once_and_is_listed() {
        let (dir, _repo) = repo("create");
        let file = git_hook_create_blocking(&p(&dir), "pre-commit").unwrap();
        assert_eq!(Path::new(&file), dir.join(".git/hooks/pre-commit"));
        let script = fs::read_to_string(&file).unwrap();
        assert!(script.starts_with("#!/bin/sh\n# pre-commit: Before a commit."));
        assert!(script.ends_with("\nexit 0\n") && !script.contains('\r'));

        let hooks = git_hooks_blocking(&p(&dir)).unwrap();
        assert_eq!(hooks.source, "hooksPath");
        assert_eq!(hooks.shown_dir, ".git/hooks");
        assert_eq!(state(&hooks, "pre-commit"), ("active", Some(file.as_str())));
        // git's own examples, which a new repository gets.
        assert_eq!(state(&hooks, "pre-push"), ("sample", None));
        assert_eq!(state(&hooks, "post-checkout"), ("missing", None));
        assert_eq!(hooks.hooks.len(), HOOKS.len());

        // It runs: a commit goes through it.
        fs::write(&file, "#!/bin/sh\necho ran; exit 1\n").unwrap();
        fs::write(dir.join("a.txt"), "a\n").unwrap();
        let repo = Repository::open(&dir).unwrap();
        let mut c = repo.config().unwrap();
        c.set_str("user.name", "t").unwrap();
        c.set_str("user.email", "t@t").unwrap();
        let err = crate::git::git_commit_blocking(p(&dir), "x".into()).unwrap_err();
        assert!(err.ends_with("ran"), "{err}");
    }

    #[test]
    fn creating_a_hook_there_is_leaves_it_as_it_is() {
        let (dir, _repo) = repo("exists");
        let folder = dir.join(".git/hooks");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("commit-msg"), "mine\n").unwrap();
        let file = git_hook_create_blocking(&p(&dir), "commit-msg").unwrap();
        assert_eq!(fs::read_to_string(file).unwrap(), "mine\n");
    }

    #[test]
    fn only_the_hooks_the_panel_runs_are_made() {
        let (dir, _repo) = repo("unknown");
        for name in ["../escape", "pre-commit/../x", "pre-receive", ""] {
            assert!(git_hook_create_blocking(&p(&dir), name).is_err(), "{name}");
        }
        assert!(!dir.join("escape").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_that_isnt_executable_says_so_until_made_executable() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _repo) = repo("not-executable");
        let folder = dir.join(".git/hooks");
        fs::create_dir_all(&folder).unwrap();
        let file = folder.join("pre-push");
        fs::write(&file, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(folder.join("pre-rebase.sample"), "#!/bin/sh\n").unwrap();

        let hooks = git_hooks_blocking(&p(&dir)).unwrap();
        assert!(hooks.needs_executable);
        assert_eq!(state(&hooks, "pre-push").0, "not-executable");
        assert_eq!(state(&hooks, "pre-rebase"), ("sample", None));

        git_hook_make_executable_blocking(&p(&dir), "pre-push").unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o755);
        let hooks = git_hooks_blocking(&p(&dir)).unwrap();
        assert_eq!(state(&hooks, "pre-push").0, "active");
    }

    #[test]
    fn a_deleted_hook_goes_to_the_trash() {
        let (dir, _repo) = repo("delete");
        let file = git_hook_create_blocking(&p(&dir), "post-merge").unwrap();
        git_hook_delete_blocking(&p(&dir), "post-merge").unwrap();
        assert!(!Path::new(&file).exists());
        let hooks = git_hooks_blocking(&p(&dir)).unwrap();
        assert_eq!(state(&hooks, "post-merge").0, "missing");
        // Nothing to delete: nothing done.
        git_hook_delete_blocking(&p(&dir), "post-merge").unwrap();
    }

    #[test]
    fn husky_hooks_are_the_ones_in_its_folder() {
        let (dir, repo) = repo("husky");
        // husky 9: core.hooksPath is its generated `.husky/_`.
        let generated = dir.join(".husky/_");
        fs::create_dir_all(&generated).unwrap();
        fs::write(generated.join("h"), "#!/usr/bin/env sh\n").unwrap();
        fs::write(generated.join("pre-commit"), ". \"$(dirname \"$0\")/h\"\n").unwrap();
        fs::write(dir.join(".husky/pre-commit"), "npm test\n").unwrap();
        repo.config()
            .unwrap()
            .set_str("core.hooksPath", ".husky/_")
            .unwrap();

        let hooks = git_hooks_blocking(&p(&dir)).unwrap();
        assert_eq!(hooks.source, "husky");
        assert_eq!(hooks.shown_dir, ".husky");
        assert!(!hooks.needs_executable, "husky runs them with sh");
        let pre_commit = text(&dir.join(".husky/pre-commit"));
        assert_eq!(
            state(&hooks, "pre-commit"),
            ("active", Some(pre_commit.as_str()))
        );
        assert_eq!(state(&hooks, "commit-msg").0, "missing");
        let made = git_hook_create_blocking(&p(&dir), "commit-msg").unwrap();
        assert_eq!(Path::new(&made), dir.join(".husky/commit-msg"));
    }
}
