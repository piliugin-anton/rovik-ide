//! Runs on a server (D8): each chat works in a git worktree of its own, as
//! a direct-mode run, beside the daemon's own folder, `~/.rovik`.
//!
//! - Git commands that change the repository are refused, as in an
//!   isolated chat: the worktree's changes go back to the project as the
//!   chat's result, not through git.
//! - A command that names the daemon's folder, in any spelling that puts
//!   `.rovik` after a slash (`~/.rovik`, `$HOME/.rovik`, its absolute path,
//!   `../.rovik`), waits for the user, never the AI reviewer, whatever the
//!   settings say. The project's own `.rovik` folder, named from the
//!   worktree's root, doesn't.
//!
//! Both are text matches: they stop accidents, not a determined command
//! (design §6). Parallel workers need an isolated chat, so a server's runs
//! never have them.

use super::permissions::parse_command;
use regex::Regex;
use std::sync::LazyLock;

/// Why a run on a server waits for the user before such a command.
pub const DAEMON_FOLDER_NOTE: &str =
    "Commands that name the server's own folder (~/.rovik) always need your approval.";

/// `.rovik` as a folder after a slash.
static DAEMON_FOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/\.rovik\b").unwrap());

/// Why a command can't run in a server's worktree as written, if it can't.
pub fn command_problem(command: &str) -> Option<String> {
    let parsed = parse_command(command);
    let what = parsed
        .segments
        .iter()
        .find_map(|segment| crate::overlay::chats::git_changes_repo(segment))?;
    Some(format!(
        "Error: `git {what}` changes the repository, and this chat works in a worktree of its own on the server: its changes go back to the project as its result, not through git. Read-only git (status, diff, log, show, blame, branch listing) works. Leave committing to the user."
    ))
}

/// The files a chat on a server changed that haven't landed, earlier runs'
/// too: its worktree's own `git status`, whose index the daemon keeps on
/// the chat's landing base (rovik-agentd's sync.rs and results.rs). An
/// untracked folder is named once, `dir/`. None where `root` isn't a
/// worktree.
pub fn unlanded(root: &std::path::Path) -> Vec<String> {
    let Ok(repo) = git2::Repository::open(root) else {
        return Vec::new();
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(false);
    let statuses = match repo.statuses(Some(&mut opts)) {
        Ok(statuses) => statuses,
        Err(e) => {
            eprintln!(
                "agent: {}: the changes that haven't landed weren't read: {}",
                root.display(),
                e.message()
            );
            return Vec::new();
        }
    };
    let paths = statuses
        .iter()
        .filter_map(|entry| entry.path().ok().map(str::to_string));
    paths.collect()
}

/// Whether a command names the daemon's folder, quotes and escapes aside.
pub fn names_daemon_folder(command: &str) -> bool {
    let plain: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    DAEMON_FOLDER.is_match(&plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_the_daemons_folder_is_named() {
        for command in [
            "cat ~/.rovik/keys.json",
            "cat $HOME/.rovik/keys.json",
            "cat ${HOME}/.rovik/keys.json",
            "cat \"$HOME\"/'.rovik'/keys.json",
            "cat ../../../.rovik/keys.json",
            "cat /home/me/.rovik/state.json",
            "ls ~/.rovik",
            "cp x ~/.ro\"\"vik/",
            "echo hi > ~/.rovik/config.toml",
        ] {
            assert!(names_daemon_folder(command), "{command}");
        }
    }

    #[test]
    fn ordinary_commands_and_the_projects_own_folder_are_not() {
        for command in [
            "cat $PWD/src/a.rs",
            "cat /home/me/.local/share/rovik/work/chat-1/src/main.rs:12:5",
            "cargo build --manifest-path /home/me/.local/share/rovik/work/c/Cargo.toml",
            "cat .rovik/plans/p.md",
            "ls ~/.rovikrc",
            "grep -r rovik src",
        ] {
            assert!(!names_daemon_folder(command), "{command}");
        }
    }

    #[test]
    fn a_worktrees_unlanded_changes_are_its_git_status() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.as_path();
        std::fs::create_dir_all(root.join("api")).unwrap();
        std::fs::write(root.join("api/a.txt"), "1\n").unwrap();
        std::fs::write(root.join("web.txt"), "w\n").unwrap();
        let repo = git2::Repository::init(root).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let me = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &me, &me, "base", &tree, &[])
            .unwrap();
        assert_eq!(unlanded(root), Vec::<String>::new(), "all landed");

        std::fs::write(root.join("api/a.txt"), "2\n").unwrap();
        std::fs::create_dir_all(root.join("web/src")).unwrap();
        std::fs::write(root.join("web/src/b.ts"), "b\n").unwrap();
        let mut found = unlanded(root);
        found.sort();
        assert_eq!(found, ["api/a.txt", "web/"]);
        let elsewhere = crate::testing::temp_dir();
        assert!(unlanded(&elsewhere.0).is_empty(), "no worktree");
    }

    #[test]
    fn git_that_changes_the_repository_is_refused() {
        for command in [
            "git checkout x",
            "git -C . commit -m x",
            "cd src && git reset --hard",
        ] {
            let problem = command_problem(command).unwrap_or_default();
            assert!(
                problem.contains("worktree of its own"),
                "{command}: {problem}"
            );
        }
        for command in ["git status", "git diff HEAD", "git log -3", "cargo test"] {
            assert_eq!(command_problem(command), None, "{command}");
        }
    }
}
