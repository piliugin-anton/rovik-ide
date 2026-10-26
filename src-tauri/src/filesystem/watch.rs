//! The project's file watcher: what changes on disk reaches the file tree
//! and the Git tab as `fs-changes` and `git-changed` events, each path
//! once per burst, in the order the changes happened, git's own files and
//! ignored ones left out.

use super::{build_gitignore, WatcherState};
use crate::util::ErrText;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::event::ModifyKind;
use notify::{self, Event, EventKind, RecursiveMode, Result as NotifyResult, Watcher};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

// Clean, unified cross-platform operation types
#[derive(Clone, Debug, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")] // Turns variants into "create", "modify", "remove", "unknown"
enum WatchOp {
    Create,
    Modify,
    Remove,
    /// The project as a whole: re-check everything.
    Unknown,
}

impl WatchOp {
    /// A change the tree is told about; None for what it isn't (access,
    /// an unknown kind).
    fn of(kind: &EventKind) -> Option<Self> {
        match kind {
            EventKind::Create(_) => Some(WatchOp::Create),
            EventKind::Modify(_) => Some(WatchOp::Modify),
            EventKind::Remove(_) => Some(WatchOp::Remove),
            _ => None,
        }
    }
}

/// Whether an event can put a folder at its path: made there, or moved
/// there (a rename's new name; where a platform doesn't say which name is
/// which, both come as a rename of `Any` kind).
fn may_bring_folder(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))
    )
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, Hash)]
pub struct FileWatchPayload {
    /// The window's folder the change is in, as the window spells it.
    folder: String,
    path: String,
    operation: WatchOp,
    is_dir: bool, // Helps frontend know if it's a file or folder immediately
}

/// What `git-changed` carries: whose repository changed, so each of a
/// workspace's repositories refreshes on its own.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, Hash)]
pub struct GitChangedPayload {
    folder: String,
    /// The folder may have become a repository of its own, or stopped being
    /// one (a `git init` here, a `.git` taken away): the window reads which
    /// repositories its folders are in again. A commit, a checkout or a save
    /// isn't one — the folder stays in the same repository.
    repositories: bool,
}

/// What a changed path is, for the watcher.
#[derive(Debug, PartialEq)]
enum WatchPath {
    /// Inside `.git/`: git's own bookkeeping, reported as one "git-changed"
    /// (visible changes are followed by one too: they change the status).
    Git,
    /// Matched by a .gitignore (build output, dependencies): not reported.
    Ignored,
    Visible,
}

/// The project's .gitignore files for the watcher, each matching under its
/// own folder: the root's and every package's. In a workspace, a package's
/// build output is often ignored only by `packages/x/.gitignore`; with the
/// root's alone, every rebuild of a dev server reached the app as the
/// user's changes. (A pattern in one file doesn't undo another's here.)
struct WatchIgnores {
    matchers: Vec<(PathBuf, Gitignore)>,
}

impl WatchIgnores {
    /// The root's .gitignore (and `.git/`): at once, while `all` looks for the rest.
    fn root_only(root: &Path) -> Self {
        WatchIgnores {
            matchers: build_gitignore(root)
                .ok()
                .map(|m| (root.to_path_buf(), m))
                .into_iter()
                .collect(),
        }
    }

    /// Every .gitignore in the project, found without entering what's ignored.
    fn all(root: &Path) -> Self {
        let mut ignores = Self::root_only(root);
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .require_git(false)
            .parents(false)
            .filter_entry(|e| e.file_name() != ".git" && e.file_name() != "node_modules")
            .build();
        for entry in walker.flatten() {
            let (path, depth) = (entry.path(), entry.depth());
            if depth == 1
                || entry.file_name() != ".gitignore"
                || !entry.file_type().is_some_and(|t| t.is_file())
            {
                continue; // the root's is in already
            }
            let Some(dir) = path.parent() else { continue };
            let mut builder = GitignoreBuilder::new(dir);
            if builder.add(path).is_none() {
                if let Ok(matcher) = builder.build() {
                    ignores.matchers.push((dir.to_path_buf(), matcher));
                }
            }
        }
        ignores
    }

    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.matchers.iter().any(|(dir, m)| {
            path.starts_with(dir) && m.matched_path_or_any_parents(path, is_dir).is_ignore()
        })
    }
}

fn watch_path_kind(root: &Path, ignores: &WatchIgnores, path: &Path) -> WatchPath {
    let Ok(rel) = path.strip_prefix(root) else {
        return WatchPath::Visible;
    };
    if rel.components().any(|c| c.as_os_str() == ".git") {
        return WatchPath::Git;
    }
    // Dependencies: always ignored, and checked without asking the disk
    // whether it's a folder (installs and caches churn there).
    if rel.components().any(|c| c.as_os_str() == "node_modules") {
        return WatchPath::Ignored;
    }
    if ignores.is_ignored(path, path.is_dir()) {
        return WatchPath::Ignored;
    }
    WatchPath::Visible
}

/// One folder the window has open: watched, with its own .gitignore files.
struct WatchFolder {
    /// The folder as the window spells it: one opened through a link is
    /// reported under that link, as the tree, the tabs and the chats have it.
    root: PathBuf,
    /// The same folder as the disk names paths in it (links resolved): an
    /// event's path is turned back into the window's spelling.
    canonical: PathBuf,
    /// Its repository's working folder: a checkout there holds the isolated
    /// chats of every folder in the repository (3.7).
    workdir: Option<PathBuf>,
    /// Its .gitignore files: the folder's at once, all of them when found.
    ignores: Arc<WatchIgnores>,
}

/// One folder being watched, as `start_file_watch` handed it over.
struct StartedFolder {
    root: PathBuf,
    canonical: PathBuf,
    /// Its repository's git folder, and the `refs/` a push writes in, when
    /// they're outside the folder (the folder is in a subfolder of the
    /// repository): commits and pushes from a terminal anywhere in the
    /// repository are otherwise never seen. Empty when the git folder is
    /// inside the folder, whose own watch sees it.
    git_dirs: Vec<PathBuf>,
    /// Its repository's working folder (the folder itself in a repository of
    /// its own).
    workdir: Option<PathBuf>,
}

impl StartedFolder {
    fn watch_folder(&self) -> WatchFolder {
        WatchFolder {
            ignores: Arc::new(WatchIgnores::root_only(&self.root)),
            root: self.root.clone(),
            canonical: self.canonical.clone(),
            workdir: self.workdir.clone(),
        }
    }
}
/// The folder a changed path is in, with the path under that folder's own
/// spelling: a change the disk reports through a link's target is the
/// window's path, in the folder it opened. The innermost folder decides: a
/// workspace's rules refuse a folder inside another (D5), but the state it's
/// restored from can hold one, and a change there is that folder's.
fn folder_of(folders: &[WatchFolder], path: &Path) -> Option<(usize, PathBuf)> {
    let mut found: Option<(usize, PathBuf, usize)> = None;
    for (at, folder) in folders.iter().enumerate() {
        let hit = match path.strip_prefix(&folder.canonical) {
            // The folder itself (its own watch sees it go, or its name
            // change): its path as the window spells it, no separator at the
            // end to join nothing onto.
            Ok(rest) if rest.as_os_str().is_empty() => folder.root.clone(),
            Ok(rest) => folder.root.join(rest),
            Err(_) if path.strip_prefix(&folder.root).is_ok() => path.to_path_buf(),
            Err(_) => continue,
        };
        let depth = folder.root.as_os_str().len();
        if found.as_ref().is_none_or(|(_, _, best)| depth > *best) {
            found = Some((at, hit, depth));
        }
    }
    found.map(|(at, hit, _)| (at, hit))
}

/// A folder's repository, as the watch needs it: its git folder (where
/// `HEAD` and the index are), the shared folder whose `refs/` the branches
/// live in (the same as the git folder, but not in a linked worktree), and
/// the working folder (what a checkout rewrites). None outside a repository.
struct RepoDirs {
    git_dir: PathBuf,
    common_dir: PathBuf,
    workdir: PathBuf,
}

fn repo_dirs_of(folder: &Path) -> Option<RepoDirs> {
    let repo = git2::Repository::discover(folder).ok()?;
    let resolved = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Some(RepoDirs {
        git_dir: resolved(repo.path()),
        common_dir: resolved(repo.commondir()),
        workdir: resolved(repo.workdir()?),
    })
}

/// The dirs to watch for a repository above `folder`, and how: the git
/// folder (`HEAD`, `index`, `packed-refs`), and the `refs/` a branch push
/// writes in — the shared one, which a linked worktree's own git folder
/// doesn't hold. None when the git folder is inside the folder, whose own
/// watch sees it.
fn git_dirs_beside(folder: &Path, dirs: &RepoDirs) -> Vec<(PathBuf, RecursiveMode)> {
    if dirs.git_dir.starts_with(folder) {
        return Vec::new();
    }
    let mut watched = vec![(dirs.git_dir.clone(), RecursiveMode::NonRecursive)];
    // A linked worktree's refs are the common folder's: its own has none.
    for refs in [dirs.common_dir.join("refs"), dirs.git_dir.join("refs")] {
        if refs.is_dir() && !watched.iter().any(|(dir, _)| dir == &refs) {
            watched.push((refs, RecursiveMode::Recursive));
        }
    }
    watched
}

/// Watches each folder in turn. One that can't be watched (gone, or a
/// permissions error) is named and left out; the others are watched still:
/// one bad folder of a workspace mustn't leave the window unwatched.
fn watch_folders(
    watcher: &mut notify::RecommendedWatcher,
    roots: Vec<PathBuf>,
) -> Vec<(PathBuf, PathBuf)> {
    let mut watched = Vec::new();
    for root in roots {
        match watch_recursive_lenient(watcher, &root) {
            Ok(skipped) => {
                if !skipped.is_empty() {
                    eprintln!(
                        "file watch: watching {:?} without {} unreadable dirs (e.g. {:?})",
                        root,
                        skipped.len(),
                        skipped.first()
                    );
                }
                let canonical = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
                watched.push((root, canonical));
            }
            Err(err) => eprintln!("file watch: {:?} isn't watched: {err}", root),
        }
    }
    watched
}

/// More changes than this in one burst (a branch switch, an unpacked
/// archive, a dev server's rebuild) are reported as a single "re-check
/// everything" event: the frontend handles each event, and thousands at
/// once froze it.
const MAX_EVENTS_PER_FLUSH: usize = 300;
/// Raw events handled per pass, so a flood can't keep the loop from ever
/// reporting anything.
const MAX_RAW_EVENTS_PER_PASS: usize = 5_000;
/// Changes are reported once things are quiet for this long...
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// ...or at the latest this long after the first, while they keep coming.
const WATCH_MAX_WAIT: Duration = Duration::from_secs(2);

/// The watcher's changes waiting to be reported: each path once, when the
/// burst is quiet for WATCH_DEBOUNCE or WATCH_MAX_WAIT after it began; past
/// MAX_EVENTS_PER_FLUSH, one "re-check everything" instead.
#[derive(Default)]
struct WatchBatch {
    events: HashMap<FileWatchPayload, Instant>,
    started: Option<Instant>,
    last: Option<Instant>,
    overflowed: bool,
}

#[derive(Debug, PartialEq)]
enum WatchFlush {
    Nothing,
    Events(Vec<FileWatchPayload>),
    Everything,
}

impl WatchBatch {
    /// Nothing waiting to be reported (a flush resets the batch to this).
    fn is_empty(&self) -> bool {
        self.started.is_none()
    }

    fn add(&mut self, payload: FileWatchPayload, now: Instant) {
        self.started.get_or_insert(now);
        self.last = Some(now);
        if self.overflowed {
            return;
        }
        self.events.insert(payload, now);
        if self.events.len() > MAX_EVENTS_PER_FLUSH {
            // Too many to report one by one: stop collecting them.
            self.events.clear();
            self.overflowed = true;
        }
    }

    fn take_due(&mut self, now: Instant) -> WatchFlush {
        let (Some(started), Some(last)) = (self.started, self.last) else {
            return WatchFlush::Nothing;
        };
        let overdue = now.duration_since(started) >= WATCH_MAX_WAIT;
        if self.overflowed {
            if !overdue && now.duration_since(last) < WATCH_DEBOUNCE {
                return WatchFlush::Nothing;
            }
            *self = WatchBatch::default();
            return WatchFlush::Everything;
        }
        let mut ready = Vec::new();
        self.events.retain(|payload, seen| {
            let due = overdue || now.duration_since(*seen) >= WATCH_DEBOUNCE;
            if due {
                ready.push((*seen, payload.clone()));
            }
            !due
        });
        if self.events.is_empty() {
            *self = WatchBatch::default();
        }
        if ready.is_empty() {
            return WatchFlush::Nothing;
        }
        // In the order they happened (each as last seen), not the map's:
        // a file removed and written again (git checkout, some editors'
        // saves) came out in either order, and whoever took the last word
        // on the path (IntelliSense) dropped a file that was there.
        ready.sort_by_key(|(seen, _)| *seen);
        WatchFlush::Events(ready.into_iter().map(|(_, payload)| payload).collect())
    }
}

/// What's in `dir`, a folder that just appeared, as (path, is a folder):
/// at most `limit` entries, never through a link, ignored ones (and what's
/// in them) left out. inotify reports what happens in a new folder only
/// once it watches the folder, a moment after the folder is made: what a
/// landing, an unpacked archive or `mkdir -p x && echo > x/y` writes
/// straight into it went unreported, and so did all of a folder moved in.
fn folder_contents(
    root: &Path,
    ignores: &WatchIgnores,
    dir: &Path,
    limit: usize,
) -> Vec<(PathBuf, bool)> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if found.len() >= limit {
                return found;
            }
            // file_type() doesn't follow links: a linked folder is an entry.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if watch_path_kind(root, ignores, &path) != WatchPath::Visible {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path.clone());
            }
            found.push((path, file_type.is_dir()));
        }
    }
    found
}

/// Bounds the fallback walk below: past this many watched dirs we stop
/// descending (still Ok — the watched part keeps working).
const MAX_WATCH_DIRS: usize = 30_000;

/// Watch `root` and everything below it, skipping what can't be watched
/// (e.g. a database dir owned by another user: `Permission denied`).
/// A single `watch(Recursive)` fails the whole project when one subdir is
/// unreadable, so on error fall back to watching dir-by-dir and skipping
/// failures. Returns the skipped dirs (logged, not fatal).
fn watch_recursive_lenient(
    watcher: &mut notify::RecommendedWatcher,
    root: &Path,
) -> Result<Vec<PathBuf>, String> {
    if watcher.watch(root, RecursiveMode::Recursive).is_ok() {
        return Ok(Vec::new());
    }
    // Fallback: one non-recursive watch per dir; failures are skipped.
    watcher
        .watch(root, RecursiveMode::NonRecursive)
        .err_text()?;
    let mut skipped = Vec::new();
    let watched = watch_below(watcher, root, MAX_WATCH_DIRS - 1, &mut skipped);
    if watched + 1 >= MAX_WATCH_DIRS {
        eprintln!(
            "file watch: {} folders watched; the rest aren't",
            MAX_WATCH_DIRS
        );
    }
    Ok(skipped)
}

/// Watches the folders below `dir` one by one (never through a link, so
/// nothing outside the project is watched; `node_modules` left out, as its
/// events are), at most `limit` of them; those that can't be are noted in
/// `skipped`. Returns how many were watched.
fn watch_below(
    watcher: &mut notify::RecommendedWatcher,
    dir: &Path,
    limit: usize,
    skipped: &mut Vec<PathBuf>,
) -> usize {
    let mut watched = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            skipped.push(dir);
            continue;
        };
        for entry in entries.flatten() {
            if watched >= limit {
                return watched;
            }
            // file_type() doesn't follow symlinks: linked dirs are skipped.
            if !entry.file_type().is_ok_and(|t| t.is_dir()) || entry.file_name() == "node_modules" {
                continue;
            }
            let path = entry.path();
            match watcher.watch(&path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched += 1;
                    stack.push(path);
                }
                Err(_) => skipped.push(path),
            }
        }
    }
    watched
}

/// Best effort: cover a newly created dir (and its subdirs) with watches.
/// Needed when the initial watch fell back to per-dir watches above, which
/// don't pick up new dirs on their own. Re-watching under a recursive
/// watcher is harmless. Only call for paths under the watched root.
fn watch_new_dir(state: &WatcherState, dir: &Path) {
    let Ok(mut guard) = state.active_watcher.lock() else {
        return;
    };
    let Some(watcher) = guard.as_mut() else {
        return;
    };
    if watcher.watch(dir, RecursiveMode::NonRecursive).is_err() {
        return;
    }
    watch_below(watcher, dir, 500, &mut Vec::new());
}

/// The current file watch (see `start_file_watch`): bumped by each new
/// watch, so the task of the one it replaced stops.
static WATCH_GENERATION: AtomicU64 = AtomicU64::new(0);

/// A newer watch has started since `generation`'s.
fn watch_replaced(generation: u64) -> bool {
    WATCH_GENERATION.load(Ordering::SeqCst) != generation
}

/// Begins a watch: the watcher before is dropped (its events stop) and the
/// generation moves on (its task ends once it sees that), both under the
/// slot's lock. The new watch's generation.
fn begin_watch<W>(slot: &std::sync::Mutex<Option<W>>, generations: &AtomicU64) -> u64 {
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
    generations.fetch_add(1, Ordering::SeqCst) + 1
}

/// Keeps `watcher` as the active one, unless a newer watch began while it
/// was being built: then it's dropped, and false. Two folders opened in
/// quick succession build their watchers at once, and the older one,
/// finishing last, must not replace the newer one's (which would leave the
/// project unwatched).
fn keep_watcher<W>(
    slot: &std::sync::Mutex<Option<W>>,
    generations: &AtomicU64,
    generation: u64,
    watcher: W,
) -> bool {
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    if generations.load(Ordering::SeqCst) != generation {
        return false;
    }
    *slot = Some(watcher);
    true
}

/// The watcher is gone (`disconnected`) and nothing is left to report: no
/// event can arrive any more, so the watch's task can end.
fn watch_finished(disconnected: bool, batch: &WatchBatch, git_pending: bool) -> bool {
    disconnected && batch.is_empty() && !git_pending
}

/// Watches one folder, and the dirs its repository needs beside it. The git
/// folder is a real path, resolved: `canonical` is the folder as the disk
/// names it, so a folder opened through a link is compared as the disk's own
/// path, and its `.git` isn't registered twice (its own watch sees it).
/// Returns the folder as started, and the git dirs registered on their own.
fn start_folder(
    watcher: &mut notify::RecommendedWatcher,
    root: PathBuf,
    canonical: PathBuf,
) -> StartedFolder {
    let dirs = repo_dirs_of(&canonical);
    let mut git_dirs = Vec::new();
    if let Some(dirs) = &dirs {
        for (path, mode) in git_dirs_beside(&canonical, dirs) {
            // Best effort: the folder's own files are watched anyway.
            match watcher.watch(&path, mode) {
                Ok(()) => git_dirs.push(path),
                Err(err) => eprintln!("file watch: {:?} isn't watched: {err}", path),
            }
        }
    }
    StartedFolder {
        root,
        canonical,
        git_dirs,
        workdir: dirs.map(|dirs| dirs.workdir),
    }
}

/// Watches the window's folders, in place of those watched before: their
/// changes go to the frontend as `fs-changes` and `git-changed` events until
/// another set of folders is watched. A workspace has several; a project is
/// the list of one.
#[tauri::command]
pub async fn start_file_watch(
    app_handle: AppHandle,
    state: State<'_, WatcherState>,
    paths: Vec<String>,
) -> Result<(), String> {
    // The folders watched before go first (dropping the watcher stops their
    // events), and their task ends on its own once it sees the generation
    // change: dropping the watcher alone only stops new events, and its loop
    // would keep waking every 50 ms forever, one more per window switched.
    let generation = begin_watch(&state.active_watcher, &WATCH_GENERATION);
    // The folders watched before had their own .gitignore files: another
    // folder's, or the same folder as it was, isn't what the tree reads now.
    if let Ok(mut cached) = state.gitignore.lock() {
        cached.clear();
    }

    let (tx, rx) = std::sync::mpsc::channel::<NotifyResult<Event>>();
    let roots: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    // Off the main thread: watching a big tree folder by folder takes a
    // while (inotify), and nothing else must wait for it.
    let (watcher, started) = crate::util::blocking(move || {
        let mut watcher = notify::recommended_watcher(tx).err_text()?;
        // One unreadable subdir (a database owned by another user, a FUSE
        // mount with restricted permissions) must not fail the watch, and
        // one folder that can't be watched must not fail the others.
        let mut started: Vec<StartedFolder> = Vec::new();
        for (root, canonical) in watch_folders(&mut watcher, roots) {
            let folder = start_folder(&mut watcher, root, canonical);
            for dir in &folder.git_dirs {
                println!("Watching (git): {:?}", dir);
            }
            started.push(folder);
        }
        Ok((watcher, started))
    })
    .await?;
    // Kept alive until the next switch, unless that came while this one
    // was being built.
    if !keep_watcher(
        &state.active_watcher,
        &WATCH_GENERATION,
        generation,
        watcher,
    ) {
        return Ok(());
    }
    for folder in &started {
        println!("Watching: {:?}", folder.root);
    }
    // A thread of its own: what it does with each change (a new folder's
    // listing and watches, the isolated chats' conflict checks) is disk
    // work, which would hold up the async threads every other command
    // and run shares.
    std::thread::Builder::new()
        .name("file-watch".into())
        .spawn(move || Watch::new(app_handle, started, generation, rx).run())
        .err_text()?;
    Ok(())
}

/// The folders' .gitignore files, found off the runtime: the folder each is
/// for, and its matchers.
type IgnoreFinder = (
    Sender<(usize, WatchIgnores)>,
    Receiver<(usize, WatchIgnores)>,
);

/// The started folders' watch, with each folder's own state.
fn watch_folders_of(started: &[StartedFolder]) -> Vec<WatchFolder> {
    started.iter().map(StartedFolder::watch_folder).collect()
}

/// The dirs watched on their own, with the folders they serve: a repository
/// whose git folder is above its folder (the folder is in a subfolder of the
/// repository), so a commit or a push from a terminal anywhere in the
/// repository is seen — every dir of it, the shared `refs/` a linked
/// worktree's push writes included, so that change is credited too. A dir
/// inside any watched folder is left out: that folder's own watch reports it,
/// as git's bookkeeping (a hold on the chats included). Every folder a dir
/// serves gets an entry: two folders of one repository both hear of its
/// commits.
fn watched_git_dirs(started: &[StartedFolder], folders: &[WatchFolder]) -> Vec<(PathBuf, usize)> {
    let mut dirs: Vec<(PathBuf, usize)> = Vec::new();
    for (at, folder) in started.iter().enumerate() {
        for dir in &folder.git_dirs {
            if folders
                .iter()
                .any(|watched| dir.starts_with(&watched.canonical))
            {
                continue;
            }
            dirs.push((dir.clone(), at));
        }
    }
    dirs
}

/// The folders a change in a watched git folder is credited to: every folder
/// in that repository, not the first found.
fn git_folders_touched(git_dirs: &[(PathBuf, usize)], path: &Path) -> Vec<usize> {
    let mut at: Vec<usize> = git_dirs
        .iter()
        .filter(|(dir, _)| path.starts_with(dir))
        .map(|(_, at)| *at)
        .collect();
    at.sort_unstable();
    at.dedup();
    at
}

/// Whether this change can change which repository the folder at `root` is in:
/// the folder's own `.git` made or taken away (a `git init`, a repository
/// removed), or, for a folder inside a repository, the git folder watched for
/// it above. `HEAD` stands in for the git folder itself: a platform need not
/// report a folder that went, but it reports the `HEAD` git made or a
/// `rm -rf .git` took with it. A commit, a checkout or a push only changes
/// them, which leaves the folder in the repository it's in.
fn repository_boundary(
    operation: &WatchOp,
    path: &Path,
    root: &Path,
    git_dirs: &[(PathBuf, usize)],
    at: usize,
) -> bool {
    if !matches!(operation, WatchOp::Create | WatchOp::Remove) {
        return false;
    }
    let own = root.join(".git");
    path == own
        || path == own.join("HEAD")
        || git_dirs
            .iter()
            .any(|(dir, owner)| *owner == at && (path == *dir || path == dir.join("HEAD")))
}

/// Whether `path` is a moved `HEAD` or `ORIG_HEAD` of one of the watched git
/// folders: a commit, a checkout, a merge or a rebase, which rewrites the
/// working tree the isolated chats read.
fn is_head_or_orig_head(path: &Path, git_dirs: &[(PathBuf, usize)]) -> bool {
    let moved = path
        .file_name()
        .is_some_and(|name| name == "HEAD" || name == "ORIG_HEAD");
    moved
        && git_dirs
            .iter()
            .any(|(dir, _)| path.parent() == Some(dir.as_path()))
}

/// One window's folders, watched: the events notify sends, batched and
/// reported. A project is the list of one; a workspace has several, each
/// with its own .gitignore files and its own repository.
struct Watch {
    app: AppHandle,
    /// The window's folders, in its order.
    folders: Vec<WatchFolder>,
    /// The dirs the folders' repositories are watched at, each with the folder
    /// it serves (its git folder is above the folder): commits and pushes from
    /// a terminal anywhere in the repository, and a repository removed.
    git_dirs: Vec<(PathBuf, usize)>,
    generation: u64,
    events: Receiver<NotifyResult<Event>>,
    /// The folders' .gitignore files, found off the runtime, by folder.
    found_ignores: IgnoreFinder,
    batch: WatchBatch,
    /// Last activity in each folder's repository, by folder (a commit writes
    /// hundreds of files there).
    git_touched: HashMap<usize, Instant>,
    /// The folders whose own `.git` was made or taken away since the last
    /// report: which repository they are in may have changed, and the window
    /// reads them again.
    repo_changed: HashSet<usize>,
}

impl Watch {
    fn new(
        app: AppHandle,
        started: Vec<StartedFolder>,
        generation: u64,
        events: Receiver<NotifyResult<Event>>,
    ) -> Self {
        let watch = Watch {
            found_ignores: std::sync::mpsc::channel(),
            folders: watch_folders_of(&started),
            git_dirs: Vec::new(),
            app,
            generation,
            events,
            batch: WatchBatch::default(),
            git_touched: HashMap::new(),
            repo_changed: HashSet::new(),
        };
        // A repository's git folder inside a watched folder is that folder's
        // own watch's: it comes through `watch_path_kind` as git's bookkeeping.
        let mut watch = watch;
        watch.git_dirs = watched_git_dirs(&started, &watch.folders);
        for at in 0..watch.folders.len() {
            watch.find_ignores(at);
        }
        watch
    }

    /// One folder's .gitignore files, looked for off the runtime.
    fn find_ignores(&self, at: usize) {
        let Some(folder) = self.folders.get(at) else {
            return;
        };
        let (root, tx) = (folder.root.clone(), self.found_ignores.0.clone());
        tauri::async_runtime::spawn_blocking(move || {
            let _ = tx.send((at, WatchIgnores::all(&root)));
        });
    }

    fn run(mut self) {
        loop {
            // Another set of folders replaced this watch: stop.
            if watch_replaced(self.generation) {
                break;
            }
            if let Ok((at, found)) = self.found_ignores.1.try_recv() {
                if let Some(folder) = self.folders.get_mut(at) {
                    folder.ignores = Arc::new(found);
                }
            }
            let disconnected = self.drain();
            self.report();
            if watch_finished(disconnected, &self.batch, !self.git_touched.is_empty()) {
                break;
            }
            // A short pause to rest the CPU before polling again.
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Takes what notify sent, a bounded number of events per pass. True
    /// once the watcher is gone (dropped on a switch): no event can come
    /// any more.
    fn drain(&mut self) -> bool {
        for _ in 0..MAX_RAW_EVENTS_PER_PASS {
            let event = match self.events.try_recv() {
                Ok(Ok(event)) => event,
                Ok(Err(_)) => continue,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
            };
            let Some(operation) = WatchOp::of(&event.kind) else {
                continue;
            };
            let brings_folder = may_bring_folder(&event.kind);
            for path in event.paths {
                self.saw(&operation, path, brings_folder);
            }
        }
        false
    }

    /// One changed path, in the folder it's in. Git's own files and ignored
    /// paths (target/, node_modules/) aren't the user's files: reporting each
    /// one flooded the app. `brings_folder`: the change can put a folder
    /// there (made, or moved in), whose contents are then reported too.
    fn saw(&mut self, operation: &WatchOp, path: PathBuf, brings_folder: bool) {
        // A repository above its folders (the git folder is outside them, so
        // it's watched on its own): a commit or a push from a terminal
        // anywhere in the repository, every folder's Git panel hears of it.
        let touched = git_folders_touched(&self.git_dirs, &path);
        if !touched.is_empty() {
            let now = Instant::now();
            // The watched dir itself made or taken away (the repository
            // removed): the folders it serves may be in another repository
            // now, or in none.
            for at in &touched {
                self.git_touched.insert(*at, now);
                if repository_boundary(
                    operation,
                    &path,
                    &self.folders[*at].root,
                    &self.git_dirs,
                    *at,
                ) {
                    self.repo_changed.insert(*at);
                }
            }
            // A checkout, merge or rebase moves HEAD and rewrites the working
            // tree: isolated chats on every folder in that repository wait it
            // out (3.7), as they do for a `.git` inside a folder below.
            if is_head_or_orig_head(&path, &self.git_dirs) {
                for at in &touched {
                    if let Some(workdir) = &self.folders[*at].workdir {
                        rovik_core::overlay::chats::project_busy(workdir);
                    }
                }
            }
            return;
        }
        let Some((at, path)) = folder_of(&self.folders, &path) else {
            return; // none of the window's folders: a watch left over
        };
        let root = self.folders[at].root.clone();
        let ignores = self.folders[at].ignores.clone();
        match watch_path_kind(&root, &ignores, &path) {
            WatchPath::Git => {
                self.git_touched.insert(at, Instant::now());
                // The folder's own `.git` made or taken away (a `git init`
                // here, a repository removed): which repository the folder is
                // in may have changed.
                if repository_boundary(operation, &path, &root, &self.git_dirs, at) {
                    self.repo_changed.insert(at);
                }
                // A checkout, merge or rebase is rewriting the working
                // tree: isolated chats wait it out.
                if path.parent() == Some(root.join(".git").as_path())
                    && path
                        .file_name()
                        .is_some_and(|n| n == "HEAD" || n == "ORIG_HEAD")
                {
                    rovik_core::overlay::chats::project_busy(&root);
                }
                return;
            }
            WatchPath::Ignored => return,
            WatchPath::Visible => {}
        }
        let state = self.app.state::<WatcherState>();
        if path.file_name().is_some_and(|n| n == ".gitignore") {
            self.find_ignores(at);
            // The tree's listing re-reads it too.
            if let Ok(mut cached) = state.gitignore.lock() {
                cached.clear();
            }
        }
        let is_dir = path.is_dir();
        let appeared = brings_folder && is_dir && path.starts_with(&root);
        // A new dir isn't covered when the initial watch fell back to
        // per-dir watches: cover it now, best effort. (Under a recursive
        // watcher this re-watch is harmless.) Nor is one moved in.
        if appeared {
            watch_new_dir(&state, &path);
        }
        let folder = spelling(&root);
        self.batch.add(
            payload(&folder, &path, operation.clone(), is_dir),
            Instant::now(),
        );
        // What it holds already, which no event will report (watched first,
        // so whatever comes after this look is reported by itself). Past a
        // flush's worth, the batch turns into "re-check everything".
        if appeared {
            for (inner, inner_is_dir) in
                folder_contents(&root, &ignores, &path, MAX_EVENTS_PER_FLUSH + 1)
            {
                self.batch.add(
                    payload(&folder, &inner, WatchOp::Create, inner_is_dir),
                    Instant::now(),
                );
            }
        }
    }

    /// What's due, reported. A flood is one "re-check everything" per folder
    /// (operation "unknown", the folder itself). Git activity once it has
    /// settled, or a change in a folder's working tree: that folder's Git
    /// panel refreshes.
    fn report(&mut self) {
        let now = Instant::now();
        let flush = self.batch.take_due(now);
        // Folders whose git status is read again: a repository's activity
        // that has settled, the working tree of a folder with a change in
        // it, or every folder after a flood.
        let mut refresh: Vec<usize> = Vec::new();
        self.git_touched.retain(|at, touched| {
            if now.duration_since(*touched) >= WATCH_DEBOUNCE {
                refresh.push(*at);
                return false;
            }
            true
        });
        match flush {
            WatchFlush::Nothing => {}
            WatchFlush::Everything => {
                for folder in &self.folders {
                    // Isolated chats on this folder read it afresh.
                    rovik_core::overlay::chats::project_changed(&folder.root, &[], true);
                }
                for payload in everything_payloads(&self.folders) {
                    let _ = self.app.emit("fs-changes", payload);
                }
                refresh = (0..self.folders.len()).collect();
            }
            WatchFlush::Events(events) => {
                for (at, folder) in self.folders.iter().enumerate() {
                    let name = spelling(&folder.root);
                    let paths: Vec<PathBuf> = events
                        .iter()
                        .filter(|p| p.folder == name)
                        .map(|p| PathBuf::from(&p.path))
                        .collect();
                    if paths.is_empty() {
                        continue;
                    }
                    refresh.push(at);
                    // Isolated chats on this folder: see it as it is now, and
                    // hear right away when their own change conflicts.
                    for (chat, path) in
                        rovik_core::overlay::chats::project_changed(&folder.root, &paths, false)
                    {
                        self.app.state::<rovik_core::Core>().emit(
                            rovik_core::agent_run::events::AgentEvent::Notice {
                                dialog_id: Some(chat),
                                kind: "warning".into(),
                                title: "A chat's change conflicts with the project".into(),
                                message: format!(
                                    "{path} changed in the project on lines an isolated chat changed too. That chat's changes won't land without you deciding."
                                ),
                            },
                        );
                    }
                }
                for payload in events {
                    let _ = self.app.emit("fs-changes", payload);
                }
            }
        }
        refresh.sort_unstable();
        refresh.dedup();
        for at in refresh {
            let Some(root) = self.folders.get(at).map(|folder| folder.root.clone()) else {
                continue;
            };
            let _ = self.app.emit(
                "git-changed",
                GitChangedPayload {
                    folder: spelling(&root),
                    repositories: self.repo_changed.remove(&at),
                },
            );
        }
    }
}

/// A path as the frontend spells it.
fn spelling(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// One change, for the frontend, naming the folder it's in.
fn payload(folder: &str, path: &Path, operation: WatchOp, is_dir: bool) -> FileWatchPayload {
    FileWatchPayload {
        folder: folder.to_string(),
        path: path.to_string_lossy().into_owned(),
        operation,
        is_dir,
    }
}

/// What a flood reports: one "re-check everything" per folder, each naming
/// its folder, so each folder's part of the window is read again.
fn everything_payloads(folders: &[WatchFolder]) -> Vec<FileWatchPayload> {
    folders
        .iter()
        .map(|folder| {
            payload(
                &spelling(&folder.root),
                &folder.root,
                WatchOp::Unknown,
                true,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_files_and_ignored_paths_are_kept_out() {
        let dir = std::env::temp_dir().join(format!("watch-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\n").unwrap();
        let m = WatchIgnores::root_only(&dir);
        let kind = |p: &str| watch_path_kind(&dir, &m, &dir.join(p));
        assert_eq!(kind(".git/objects/ab/cdef0123"), WatchPath::Git);
        assert_eq!(kind(".git/index"), WatchPath::Git);
        assert_eq!(kind("sub/.git/HEAD"), WatchPath::Git);
        assert_eq!(kind("target/debug/app"), WatchPath::Ignored);
        assert_eq!(kind("build.log"), WatchPath::Ignored);
        assert_eq!(kind("src/main.rs"), WatchPath::Visible);
        assert_eq!(kind(".gitignore"), WatchPath::Visible);
        assert_eq!(kind(".github/workflows/ci.yml"), WatchPath::Visible);
        assert_eq!(
            watch_path_kind(&dir, &m, Path::new("/elsewhere/x")),
            WatchPath::Visible
        );
        assert_eq!(
            kind("packages/a/node_modules/.vite/deps/x.js"),
            WatchPath::Ignored
        );
    }

    fn change(path: &str) -> FileWatchPayload {
        payload("", Path::new(path), WatchOp::Modify, false)
    }

    /// Two watches begun at once: the older one's watcher, built last,
    /// doesn't replace the newer one's, which stays.
    #[test]
    fn an_older_watch_built_last_doesnt_replace_the_newer_one() {
        let slot = std::sync::Mutex::new(Some("before"));
        let generations = AtomicU64::new(0);
        let older = begin_watch(&slot, &generations);
        assert_eq!(*slot.lock().unwrap(), None, "the one before is dropped");
        let newer = begin_watch(&slot, &generations);
        assert!(keep_watcher(&slot, &generations, newer, "newer"));
        assert!(!keep_watcher(&slot, &generations, older, "older"));
        assert_eq!(*slot.lock().unwrap(), Some("newer"));
    }

    #[test]
    fn a_replaced_or_finished_watch_stops() {
        // A newer watch replaces an older one's task.
        let mine = WATCH_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(!watch_replaced(mine));
        WATCH_GENERATION.fetch_add(1, Ordering::SeqCst);
        assert!(watch_replaced(mine));

        // The watcher gone: done once nothing is left to report.
        let t0 = Instant::now();
        let mut b = WatchBatch::default();
        assert!(b.is_empty());
        assert!(!watch_finished(false, &b, false), "still connected");
        assert!(watch_finished(true, &b, false));
        b.add(change("a"), t0);
        assert!(!b.is_empty());
        assert!(!watch_finished(true, &b, false), "a change still to report");
        assert!(matches!(
            b.take_due(t0 + Duration::from_secs(5)),
            WatchFlush::Events(_)
        ));
        assert!(b.is_empty(), "reported");
        assert!(
            !watch_finished(true, &b, true),
            "git activity still to report"
        );
        assert!(watch_finished(true, &b, false));
    }

    #[test]
    fn changes_are_reported_once_quiet_or_at_the_latest_after_the_max_wait() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut b = WatchBatch::default();
        assert_eq!(b.take_due(t0), WatchFlush::Nothing);
        b.add(change("a"), t0);
        b.add(change("a"), ms(100));
        assert_eq!(b.take_due(ms(200)), WatchFlush::Nothing, "still changing");
        assert_eq!(
            b.take_due(ms(400)),
            WatchFlush::Events(vec![change("a")]),
            "once, when quiet"
        );
        assert_eq!(b.take_due(ms(500)), WatchFlush::Nothing);

        // A dev server writing every 200ms: reported anyway after WATCH_MAX_WAIT.
        let mut b = WatchBatch::default();
        let mut reported = None;
        for i in 0..20u64 {
            b.add(change("out.js"), ms(i * 200));
            if let WatchFlush::Events(e) = b.take_due(ms(i * 200 + 50)) {
                reported = Some((i, e));
                break;
            }
        }
        let (i, events) = reported.expect("reported while still changing");
        assert_eq!(
            (i * 200 + 50 >= 2_000, events),
            (true, vec![change("out.js")])
        );
    }

    #[test]
    fn a_burst_is_reported_in_the_order_it_happened() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let op = |path: &str, operation: WatchOp| payload("", Path::new(path), operation, false);
        let mut b = WatchBatch::default();
        // git checkout: the file removed, then written again...
        b.add(op("a.ts", WatchOp::Remove), ms(0));
        b.add(op("b.ts", WatchOp::Modify), ms(5));
        b.add(op("a.ts", WatchOp::Create), ms(10));
        b.add(op("a.ts", WatchOp::Modify), ms(12));
        // ...and another written, then removed.
        b.add(op("c.ts", WatchOp::Create), ms(14));
        b.add(op("c.ts", WatchOp::Remove), ms(20));
        // Seen again: it counts from the last time.
        b.add(op("b.ts", WatchOp::Modify), ms(25));
        assert_eq!(
            b.take_due(ms(400)),
            WatchFlush::Events(vec![
                op("a.ts", WatchOp::Remove),
                op("a.ts", WatchOp::Create),
                op("a.ts", WatchOp::Modify),
                op("c.ts", WatchOp::Create),
                op("c.ts", WatchOp::Remove),
                op("b.ts", WatchOp::Modify),
            ])
        );
    }

    #[test]
    fn a_new_folders_contents_are_found_without_links_or_ignored_ones() {
        let dir = std::env::temp_dir().join(format!("watch-contents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["src/utils/deep", "src/dist", "src/node_modules/m"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join(".gitignore"), "dist/\n").unwrap();
        for f in [
            "src/utils/format.ts",
            "src/utils/deep/x.ts",
            "src/dist/out.js",
            "src/node_modules/m/index.js",
        ] {
            std::fs::write(dir.join(f), "export {};\n").unwrap();
        }
        // A link to a folder outside: an entry, never looked into.
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::env::temp_dir(), dir.join("src/link")).unwrap();
        let ignores = WatchIgnores::root_only(&dir);
        let relative = |found: Vec<(PathBuf, bool)>| {
            let mut found: Vec<(String, bool)> = found
                .into_iter()
                .map(|(p, is_dir)| {
                    let rel = p.strip_prefix(&dir).unwrap().to_string_lossy();
                    (rel.replace('\\', "/"), is_dir)
                })
                .collect();
            found.sort();
            found
        };
        let mut expected = vec![
            ("src/utils".to_string(), true),
            ("src/utils/deep".to_string(), true),
            ("src/utils/deep/x.ts".to_string(), false),
            ("src/utils/format.ts".to_string(), false),
        ];
        if cfg!(unix) {
            expected.push(("src/link".to_string(), false));
        }
        expected.sort();
        assert_eq!(
            relative(folder_contents(&dir, &ignores, &dir.join("src"), 100)),
            expected
        );
        // At most `limit`: past a flush's worth the batch reports everything.
        assert_eq!(
            folder_contents(&dir, &ignores, &dir.join("src"), 2).len(),
            2
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_can_come_with_a_creation_or_a_rename() {
        use notify::event::{CreateKind, DataChange, RemoveKind, RenameMode};
        assert!(may_bring_folder(&EventKind::Create(CreateKind::Folder)));
        for mode in [RenameMode::To, RenameMode::Both, RenameMode::Any] {
            assert!(may_bring_folder(&EventKind::Modify(ModifyKind::Name(mode))));
        }
        assert!(!may_bring_folder(&EventKind::Modify(ModifyKind::Data(
            DataChange::Any
        ))));
        assert!(!may_bring_folder(&EventKind::Remove(RemoveKind::Folder)));
    }

    #[test]
    fn a_flood_becomes_one_recheck_of_everything() {
        let t0 = Instant::now();
        let mut b = WatchBatch::default();
        for i in 0..=MAX_EVENTS_PER_FLUSH {
            b.add(change(&format!("f{i}")), t0);
        }
        assert!(b.events.is_empty(), "no longer collected one by one");
        b.add(change("more"), t0 + Duration::from_millis(250));
        assert_eq!(
            b.take_due(t0 + Duration::from_millis(400)),
            WatchFlush::Nothing,
            "still flooding"
        );
        assert_eq!(
            b.take_due(t0 + Duration::from_millis(600)),
            WatchFlush::Everything
        );
        assert_eq!(
            b.take_due(t0 + Duration::from_millis(700)),
            WatchFlush::Nothing
        );
    }

    #[test]
    fn an_unreadable_subdir_does_not_fail_the_whole_watch() {
        let dir = std::env::temp_dir().join(format!("watch-lenient-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ok/sub")).unwrap();
        std::fs::create_dir_all(dir.join("locked/sub")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o000))
                .unwrap();
        }
        let (tx, _rx) = std::sync::mpsc::channel::<NotifyResult<Event>>();
        let mut watcher = notify::recommended_watcher(tx).expect("watcher");
        let skipped = watch_recursive_lenient(&mut watcher, &dir).expect("watch succeeds");
        // Rooted at `locked` when we can't read it (non-root users); root
        // itself always stays watched, so the project keeps live updates.
        // Only inotify (Linux) needs a watch per folder: FSEvents and
        // Windows watch the whole tree in one, with nothing to skip.
        if cfg!(target_os = "linux") && fs::read_dir(dir.join("locked")).is_err() {
            assert!(skipped.iter().any(|p| p.starts_with(dir.join("locked"))));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_packages_own_gitignore_counts_under_that_package_only() {
        let dir = std::env::temp_dir().join(format!("watch-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in [
            "packages/web/src",
            "packages/web/.turbo",
            "packages/api/src",
            "node_modules/x",
        ] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
        // Anchored: packages/web/dist, not a dist anywhere else.
        std::fs::write(
            dir.join("packages/web/.gitignore"),
            "/dist\n.turbo/\n*.timestamp-*.mjs\n",
        )
        .unwrap();
        // In node_modules: never looked at.
        std::fs::write(dir.join("node_modules/x/.gitignore"), "*\n").unwrap();
        let m = WatchIgnores::all(&dir);
        assert_eq!(m.matchers.len(), 2, "the root's and packages/web's");
        let kind = |p: &str| watch_path_kind(&dir, &m, &dir.join(p));
        assert_eq!(kind("packages/web/dist/index.js"), WatchPath::Ignored);
        assert_eq!(kind("packages/web/.turbo/cache.json"), WatchPath::Ignored);
        assert_eq!(
            kind("packages/web/vite.config.ts.timestamp-1700000000-ab12.mjs"),
            WatchPath::Ignored
        );
        assert_eq!(kind("packages/web/src/app.ts"), WatchPath::Visible);
        assert_eq!(
            kind("packages/api/dist/index.js"),
            WatchPath::Visible,
            "another package's dist"
        );
        assert_eq!(kind("dist/index.js"), WatchPath::Visible);
        assert_eq!(kind("server.log"), WatchPath::Ignored);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder as the watch keeps it: its own .gitignore files, its root as
    /// spelled and as the disk has it, and its repository's working folder
    /// once it has a git folder.
    fn open_folder(root: &Path) -> WatchFolder {
        WatchFolder {
            root: root.to_path_buf(),
            canonical: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            workdir: None,
            ignores: Arc::new(WatchIgnores::root_only(root)),
        }
    }

    #[test]
    fn a_change_is_reported_in_the_folder_that_holds_it() {
        let base = std::env::temp_dir().join(format!("watch-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["api/src", "web/src"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        let folders = vec![
            open_folder(&base.join("api")),
            open_folder(&base.join("web")),
        ];
        let (at, path) = folder_of(&folders, &base.join("web/src/a.ts")).unwrap();
        assert_eq!((at, path), (1, base.join("web/src/a.ts")));
        let (at, _) = folder_of(&folders, &base.join("api/src/a.ts")).unwrap();
        assert_eq!(at, 0);
        // In no folder of this window: a watch left over.
        assert_eq!(folder_of(&folders, &base.join("other/a.ts")), None);
        // A folder's own root, as the remove of a folder that went: its path
        // as the window spells it, with no separator joined onto nothing.
        let own = folder_of(&folders, &base.join("api")).unwrap();
        assert_eq!(own, (0, base.join("api")));
        assert!(
            !own.1.to_string_lossy().ends_with('/'),
            "no trailing separator for the folder itself"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_watched_folder_that_went_is_reported_as_removed() {
        let base = std::env::temp_dir().join(format!("watch-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("app/src")).unwrap();
        let folders = vec![open_folder(&base.join("app"))];
        // The folder's own watch sees the folder go: notify names its root.
        std::fs::remove_dir_all(base.join("app")).unwrap();
        let (at, path) = folder_of(&folders, &base.join("app")).unwrap();
        assert_eq!((at, path.clone()), (0, base.join("app")));
        assert_eq!(
            watch_path_kind(&folders[0].root, &folders[0].ignores, &path),
            WatchPath::Visible,
            "a folder that went is the window's to drop, not git's or ignored"
        );
        // It's reported, naming that folder and no other: its tree goes, and
        // what it held is read again where it went. `is_dir` as the disk has
        // it, which for a folder that went is no longer a folder.
        let reported = payload(
            &spelling(&folders[0].root),
            &path,
            WatchOp::Remove,
            path.is_dir(),
        );
        assert_eq!(reported.folder, spelling(&folders[0].root));
        assert_eq!(reported.path, spelling(&folders[0].root));
        assert_eq!(reported.operation, WatchOp::Remove);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_moved_head_in_a_repository_above_a_folder_holds_the_chats() {
        let base = std::env::temp_dir().join(format!("watch-head-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("app")).unwrap();
        let git_dir =
            std::fs::canonicalize(base.join(".git")).unwrap_or_else(|_| base.join(".git"));
        let dirs = vec![(git_dir.clone(), 0usize)];
        // A commit or a checkout moves HEAD or ORIG_HEAD in the git folder.
        assert!(is_head_or_orig_head(&git_dir.join("HEAD"), &dirs));
        assert!(is_head_or_orig_head(&git_dir.join("ORIG_HEAD"), &dirs));
        // The index (a `git add`) doesn't rewrite the working tree, and a
        // ref update under refs/ isn't the checkout itself.
        assert!(!is_head_or_orig_head(&git_dir.join("index"), &dirs));
        assert!(!is_head_or_orig_head(
            &git_dir.join("refs/heads/main"),
            &dirs
        ));
        // A HEAD deeper in (a worktree's own) is that worktree's, not this.
        assert!(!is_head_or_orig_head(
            &git_dir.join("worktrees/other/HEAD"),
            &dirs
        ));
        // A HEAD of no watched repository: nothing to hold.
        assert!(!is_head_or_orig_head(&base.join("HEAD"), &dirs));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_opened_through_a_link_is_reported_under_its_own_path() {
        let base = std::env::temp_dir().join(format!("watch-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real/src")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let folders = vec![open_folder(&base.join("link"))];
        // The disk names the change through the link's target; the window
        // knows the folder as the link.
        let (at, path) = folder_of(&folders, &base.join("real/src/a.ts")).unwrap();
        assert_eq!((at, path), (0, base.join("link/src/a.ts")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn one_unwatchable_folder_leaves_the_other_watched() {
        let base = std::env::temp_dir().join(format!("watch-one-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ok/src")).unwrap();
        std::fs::create_dir_all(base.join("locked/src")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(base.join("locked"), std::fs::Permissions::from_mode(0o000))
                .unwrap();
        }
        let (tx, _rx) = std::sync::mpsc::channel::<NotifyResult<Event>>();
        let mut watcher = notify::recommended_watcher(tx).expect("watcher");
        let watched = watch_folders(
            &mut watcher,
            vec![base.join("locked"), base.join("ok"), base.join("gone")],
        );
        let roots: Vec<PathBuf> = watched.into_iter().map(|(root, _)| root).collect();
        assert!(
            roots.contains(&base.join("ok")),
            "the readable one is watched"
        );
        // A folder that isn't there can't be watched, and the others are.
        assert!(!roots.contains(&base.join("gone")));
        #[cfg(unix)]
        if fs::read_dir(base.join("locked")).is_err() {
            assert!(!roots.contains(&base.join("locked")));
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(base.join("locked"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_flood_names_each_folder() {
        let base = std::env::temp_dir().join(format!("watch-flood-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["api", "web"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        let folders = vec![
            open_folder(&base.join("api")),
            open_folder(&base.join("web")),
        ];
        let payloads = everything_payloads(&folders);
        assert_eq!(
            payloads
                .iter()
                .map(|p| p.folder.as_str())
                .collect::<Vec<_>>(),
            vec![
                base.join("api").to_string_lossy().as_ref(),
                base.join("web").to_string_lossy().as_ref(),
            ]
        );
        assert!(payloads
            .iter()
            .all(|p| p.operation == WatchOp::Unknown && p.is_dir));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_innermost_folder_a_change_is_in_gets_it() {
        let base = std::env::temp_dir().join(format!("watch-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("packages/web/src")).unwrap();
        // A workspace's state can hold a folder inside another (D5 refuses
        // making one, not restoring it): a change in the inner folder is the
        // inner folder's, so its own chats, its git refresh and its .git hold
        // are right, whichever order the folders were opened in.
        for folders in [
            vec![open_folder(&base), open_folder(&base.join("packages/web"))],
            vec![open_folder(&base.join("packages/web")), open_folder(&base)],
        ] {
            let (at, path) = folder_of(&folders, &base.join("packages/web/src/a.ts")).unwrap();
            assert_eq!(
                folders[at].root,
                base.join("packages/web"),
                "the innermost folder has it"
            );
            assert_eq!(path, base.join("packages/web/src/a.ts"));
            // A change above it is the outer folder's still.
            let (at, _) = folder_of(&folders, &base.join("a.ts")).unwrap();
            assert_eq!(folders[at].root, base);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_opened_through_a_link_does_not_watch_its_git_folder_twice() {
        let base = std::env::temp_dir().join(format!("watch-link-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real/app")).unwrap();
        git2::Repository::init(base.join("real")).expect("a repository");
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel::<NotifyResult<Event>>();
        let mut watcher = notify::recommended_watcher(tx).expect("watcher");
        // The window opened the folder through the link: its own watch covers
        // the real folder's `.git`, so nothing is registered twice.
        let link = base.join("link");
        let canonical = std::fs::canonicalize(&link).unwrap();
        let folder = start_folder(&mut watcher, link.clone(), canonical.clone());
        assert_eq!(folder.root, link, "reported as the window spells it");
        assert!(
            folder.git_dirs.is_empty(),
            "the folder's own watch sees its .git: {:?}",
            folder.git_dirs
        );
        // The git folder is still found, for the folders it serves.
        assert!(repo_dirs_of(&canonical).is_some());
        let folders = vec![StartedFolder::watch_folder(&folder)];
        assert!(watched_git_dirs(&[folder], &folders).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_repository_above_a_folder_is_watched_for_its_commits_and_pushes() {
        let base = std::env::temp_dir().join(format!("watch-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("app/src")).unwrap();
        std::fs::create_dir_all(base.join("loose")).unwrap();
        git2::Repository::init(&repo).expect("a repository");
        let app = repo.join("app");
        let dirs = repo_dirs_of(&app).expect("the repository above it");
        let git_dir = std::fs::canonicalize(repo.join(".git")).unwrap();
        assert_eq!(dirs.git_dir, git_dir);
        assert_eq!(dirs.workdir, std::fs::canonicalize(&repo).unwrap());
        // Commits and pushes from a terminal anywhere in the repository
        // write in the git folder and its refs: both watched.
        let beside = git_dirs_beside(&app, &dirs);
        assert!(beside
            .iter()
            .any(|(dir, mode)| dir == &git_dir && *mode == RecursiveMode::NonRecursive));
        assert!(beside
            .iter()
            .any(|(dir, mode)| dir == &git_dir.join("refs") && *mode == RecursiveMode::Recursive));
        // A folder that's the repository's own is watched by itself.
        assert!(git_dirs_beside(&repo, &dirs).is_empty());
        // Outside any repository: nothing to watch beside it.
        assert!(repo_dirs_of(&base.join("loose")).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A linked worktree's branches are the shared folder's: its own git
    /// folder has no `refs/` to watch, so a push there was never seen.
    #[test]
    fn a_linked_worktree_watches_the_refs_a_push_writes() {
        let base = std::env::temp_dir().join(format!("watch-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let main = git2::Repository::init(&repo).expect("a repository");
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        {
            let mut index = main.index().unwrap();
            index
                .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree = main.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = git2::Signature::now("t", "t@t").unwrap();
            main.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        let head = main.head().unwrap().peel_to_commit().unwrap();
        let branch = main.branch("other", &head, false).unwrap();
        let worktree = base.join("worktree");
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(branch.get()));
        main.worktree("other", &worktree, Some(&opts)).unwrap();

        let dirs = repo_dirs_of(&worktree).expect("the worktree's repository");
        // Its own git folder, and the refs a push writes: the shared ones.
        assert!(
            dirs.git_dir.ends_with("worktrees/other"),
            "{:?}",
            dirs.git_dir
        );
        assert_eq!(
            dirs.common_dir,
            std::fs::canonicalize(repo.join(".git")).unwrap()
        );
        let beside = git_dirs_beside(&worktree, &dirs);
        assert!(
            beside
                .iter()
                .any(|(dir, mode)| dir == &dirs.common_dir.join("refs")
                    && *mode == RecursiveMode::Recursive),
            "the shared refs are watched: {beside:?}"
        );
        // And a push written there is the worktree's folder's: the dirs
        // watched on its own are credited to it, its own git folder's
        // `refs/` or the shared one.
        let (tx, _rx) = std::sync::mpsc::channel::<NotifyResult<Event>>();
        let mut watcher = notify::recommended_watcher(tx).expect("watcher");
        let canonical = std::fs::canonicalize(&worktree).unwrap();
        let folder = start_folder(&mut watcher, worktree.clone(), canonical);
        let folders = vec![StartedFolder::watch_folder(&folder)];
        let watched = watched_git_dirs(&[folder], &folders);
        let pushed = dirs.common_dir.join("refs/heads/other");
        assert_eq!(
            git_folders_touched(&watched, &pushed),
            [0],
            "a push in the shared refs is the worktree folder's: {watched:?}"
        );
        drop(branch);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A folder as `start_file_watch` hands it over: its root, its canonical
    /// path, its repository's git folder, and that repository's working
    /// folder.
    /// A folder as `start_file_watch` hands it over: its root, its canonical
    /// path, the dirs of its repository watched beside it, and that
    /// repository's working folder.
    fn started(root: &Path, git_dir: Option<&Path>) -> StartedFolder {
        StartedFolder {
            root: root.to_path_buf(),
            canonical: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            git_dirs: git_dir.map(Path::to_path_buf).into_iter().collect(),
            workdir: git_dir
                .map(|_| std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())),
        }
    }

    /// Whether a change can change which repository a folder is in: its own
    /// `.git` made or gone, or the git folder it's watched at above it gone.
    /// A commit, a checkout or a save only writes inside them.
    #[test]
    fn a_gits_boundary_is_what_may_change_a_folders_repository() {
        let base = std::env::temp_dir().join(format!("watch-boundary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo_git = base.join("repo/.git");
        std::fs::create_dir_all(&repo_git).unwrap();
        let app = base.join("repo/app");
        std::fs::create_dir_all(&app).unwrap();
        let dirs = vec![(repo_git.clone(), 0usize)];
        let made = WatchOp::Create;
        let gone = WatchOp::Remove;
        // A `git init` in the folder makes its own `.git`, and `HEAD` with
        // it; taking it away makes the folder a plain one again.
        assert!(repository_boundary(
            &made,
            &app.join(".git"),
            &app,
            &dirs,
            0
        ));
        assert!(repository_boundary(
            &made,
            &app.join(".git/HEAD"),
            &app,
            &dirs,
            0
        ));
        assert!(repository_boundary(
            &gone,
            &app.join(".git/HEAD"),
            &app,
            &dirs,
            0
        ));
        // A commit, a checkout or a push writes inside the git folder, and a
        // checkout only changes the `HEAD` that's there.
        assert!(!repository_boundary(
            &WatchOp::Modify,
            &app.join(".git/HEAD"),
            &app,
            &dirs,
            0
        ));
        assert!(!repository_boundary(
            &made,
            &app.join(".git/refs/heads/main"),
            &app,
            &dirs,
            0
        ));
        assert!(!repository_boundary(
            &WatchOp::Modify,
            &app.join(".git/index"),
            &app,
            &dirs,
            0
        ));
        // A file saved or made beside it isn't one either.
        assert!(!repository_boundary(
            &made,
            &app.join("src/a.ts"),
            &app,
            &dirs,
            0
        ));
        // The repository above the folder taken away: the watched git folder,
        // or the `HEAD` removed with it, for the folder it serves and no
        // other.
        assert!(repository_boundary(&gone, &repo_git, &app, &dirs, 0));
        assert!(repository_boundary(
            &gone,
            &repo_git.join("HEAD"),
            &app,
            &dirs,
            0
        ));
        assert!(!repository_boundary(&gone, &repo_git, &app, &dirs, 1));
        // A folder that's a repository of its own has nothing watched beside
        // it: its own `.git` is the boundary, deeper files aren't.
        let own: Vec<(PathBuf, usize)> = Vec::new();
        assert!(repository_boundary(&made, &app.join(".git"), &app, &own, 0));
        assert!(!repository_boundary(
            &made,
            &app.join("src/a.ts"),
            &app,
            &own,
            0
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn every_folder_of_one_repository_hears_of_its_commits() {
        let base = std::env::temp_dir().join(format!("watch-shared-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["app", "web"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        git2::Repository::init(&base).expect("a repository");
        let git_dir = std::fs::canonicalize(base.join(".git")).unwrap();
        let started = vec![
            started(&base.join("app"), Some(&git_dir)),
            started(&base.join("web"), Some(&git_dir)),
        ];
        let folders: Vec<WatchFolder> = started.iter().map(StartedFolder::watch_folder).collect();
        // The repository's git folder is above both folders: watched on its
        // own, once for each of them.
        let dirs = watched_git_dirs(&started, &folders);
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        // A commit or a push from a terminal: BOTH folders' panels refresh,
        // not the first one's alone.
        assert_eq!(git_folders_touched(&dirs, &git_dir.join("HEAD")), [0, 1]);
        assert_eq!(
            git_folders_touched(&dirs, &git_dir.join("refs/heads/main")),
            [0, 1]
        );
        // A change in the folders themselves isn't git activity.
        assert!(git_folders_touched(&dirs, &base.join("web/a.ts")).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_repositorys_git_folder_inside_a_folder_is_that_folders_own() {
        let base = std::env::temp_dir().join(format!("watch-inside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("app")).unwrap();
        git2::Repository::init(&base).expect("a repository");
        let git_dir = std::fs::canonicalize(base.join(".git")).unwrap();
        // A workspace of the repository's folder and a folder in it.
        let started = vec![
            started(&base, Some(&git_dir)),
            started(&base.join("app"), Some(&git_dir)),
        ];
        let folders: Vec<WatchFolder> = started.iter().map(StartedFolder::watch_folder).collect();
        // The repository's folder watches its own .git: an entry for the
        // folder inside it would answer first, and that folder would never
        // see the checkout hold its isolated chats.
        assert!(watched_git_dirs(&started, &folders).is_empty());
        let (at, _) = folder_of(&folders, &git_dir.join("HEAD")).unwrap();
        assert_eq!(at, 0, "the folder that holds the repository");
        assert_eq!(
            watch_path_kind(&folders[0].root, &folders[0].ignores, &git_dir.join("HEAD")),
            WatchPath::Git,
            "git's own bookkeeping, where the checkout hold is taken"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
