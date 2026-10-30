//! What a chat's worktree can lose and rebuild: its installed dependencies
//! and build output (`usage::disposable_kind`). "Clean up now" removes
//! them from every chat that isn't busy; the chat, its messages, its edits
//! and its other files stay, and its next run reinstalls and rebuilds.
//!
//! A folder goes only when git ignores it and tracks nothing in it (a
//! project may commit its dependencies), it's reached without following a
//! link, and it's on the worktree's file system. The chat's files are held
//! meanwhile (`AgentState::hold_files`): none of its runs goes, and one
//! that starts waits until the folders are gone.

use crate::daemon::Daemon;
use crate::usage::{disposable_kind, freeable_in, Git};
use rovik_core::command_tool::background;
use rovik_core::rt;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Whether the chat is busy: one of its runs goes, or a command it left
/// running in the background (a server, say) still does.
pub fn busy(daemon: &Daemon, chat: &str) -> bool {
    daemon.core.agents.is_running(chat) || background_running(chat)
}

fn background_running(chat: &str) -> bool {
    background::list(chat).iter().any(|c| c.end.is_none())
}

/// Removes the chat's dependencies and build output, with its files held.
/// The bytes freed; None when the chat is busy, its files are held for
/// another change, or it has no worktree here.
pub async fn clean_chat(daemon: &Daemon, chat: &str) -> Option<u64> {
    let worktree = daemon.store.place(chat)?.worktree;
    let _files = daemon.core.agents.hold_files(chat).ok()?;
    // Only a run starts a background command, and none starts while held.
    if background_running(chat) {
        return None;
    }
    let removed = rt::spawn_blocking(move || remove_disposable(&worktree));
    Some(removed.await.unwrap_or(0))
}

/// Removes the disposable folders in `worktree`. The bytes freed.
pub fn remove_disposable(worktree: &Path) -> u64 {
    let Ok(top) = fs::symlink_metadata(worktree) else {
        return 0;
    };
    let git = Git::open(worktree);
    let mut freed = 0;
    let mut pending = vec![worktree.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            // Not a link's target, nor another file system's folder.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = entry.path();
            if !meta.is_dir() || meta.dev() != top.dev() || path == worktree.join(".git") {
                continue;
            }
            if disposable_kind(&path).is_none() {
                pending.push(path);
                continue;
            }
            let rel = path.strip_prefix(worktree).unwrap_or(&path);
            if git.ignores(rel, true) {
                freed += remove(&path);
            }
            // One that stays (tracked, or not ignored) is left whole.
        }
    }
    freed
}

/// Removes the folder `dir`. The bytes freed: not a file's still linked
/// from outside it, and of what went, when not all of it could.
fn remove(dir: &Path) -> u64 {
    let size = freeable_in(dir);
    match fs::remove_dir_all(dir) {
        Ok(()) => size,
        Err(e) => {
            log::warn!("{} stays, in part: {e}", dir.display());
            size.saturating_sub(freeable_in(dir))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::tests::{repository, tag, write};
    use git2::Repository;
    use rovik_core::testing::temp_dir;

    #[test]
    fn dependencies_and_build_output_go_and_edits_stay() {
        let dir = temp_dir();
        let root = &dir.0;
        let ignore = "node_modules/\ntarget/\n.env\n";
        let tracked = [("package.json", "{}"), ("src/main.rs", "fn main() {}")];
        repository(root, ignore, &tracked);
        write(&root.join("src/main.rs"), "fn main() { edited(); }");
        write(&root.join("notes.txt"), "the user's");
        write(&root.join(".env"), "KEY=1");
        write(
            &root.join("node_modules/left-pad/index.js"),
            &"x".repeat(8192),
        );
        tag(&root.join("target"));
        write(&root.join("target/debug/app"), &"x".repeat(8192));

        assert!(remove_disposable(root) >= 16384);
        assert!(!root.join("node_modules").exists());
        assert!(!root.join("target").exists());
        let main = fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert_eq!(main, "fn main() { edited(); }");
        assert!(root.join("notes.txt").exists());
        assert!(root.join(".env").exists());
        assert!(root.join(".git").exists());
    }

    #[test]
    fn a_tracked_node_modules_stays() {
        let dir = temp_dir();
        let root = &dir.0;
        let vendored = [("package.json", "{}"), ("node_modules/a/index.js", "1")];
        repository(root, "", &vendored);
        // Ignored now, but a file in it is tracked.
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        write(&root.join("node_modules/b/index.js"), "2");

        assert_eq!(remove_disposable(root), 0);
        assert!(root.join("node_modules/a/index.js").exists());
        assert!(root.join("node_modules/b/index.js").exists());
    }

    #[test]
    fn node_modules_git_doesnt_ignore_stays() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "", &[("package.json", "{}")]);
        write(&root.join("node_modules/a/index.js"), "1");

        assert_eq!(remove_disposable(root), 0);
        assert!(root.join("node_modules/a/index.js").exists());
    }

    #[test]
    fn node_modules_without_package_json_stays() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "node_modules/\n", &[]);
        write(&root.join("node_modules/a/index.js"), "1");

        assert_eq!(remove_disposable(root), 0);
        assert!(root.join("node_modules/a/index.js").exists());
    }

    #[test]
    fn a_linked_dependency_folder_stays_and_so_does_its_target() {
        let dir = temp_dir();
        let root = dir.0.join("worktree");
        let store = dir.0.join("store/node_modules");
        write(&store.join("a/index.js"), "shared");
        write(&dir.0.join("store/package.json"), "{}");
        repository(&root, "node_modules\n", &[("package.json", "{}")]);
        std::os::unix::fs::symlink(&store, root.join("node_modules")).unwrap();

        assert_eq!(remove_disposable(&root), 0);
        assert!(root.join("node_modules").is_symlink());
        assert!(store.join("a/index.js").exists());
    }

    #[test]
    fn nested_packages_are_found() {
        let dir = temp_dir();
        let root = &dir.0;
        repository(root, "node_modules/\n", &[("app/package.json", "{}")]);
        write(&root.join("app/node_modules/a/index.js"), "1");

        assert!(remove_disposable(root) > 0);
        assert!(!root.join("app/node_modules").exists());
        assert!(root.join("app/package.json").exists());
        assert!(Repository::open(root).is_ok());
    }
}
