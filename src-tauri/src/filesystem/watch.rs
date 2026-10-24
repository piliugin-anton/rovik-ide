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
use std::collections::HashMap;
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
    path: String,
    operation: WatchOp,
    is_dir: bool, // Helps frontend know if it's a file or folder immediately
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
fn watch_finished(disconnected: bool, batch: &WatchBatch, git_touched: Option<Instant>) -> bool {
    disconnected && batch.is_empty() && git_touched.is_none()
}

/// Watches the project folder `path_str`, in place of the one watched
/// before: its changes go to the frontend as `fs-changes` and `git-changed`
/// events until another folder is watched.
#[tauri::command]
pub async fn start_file_watch(
    app_handle: AppHandle,
    state: State<'_, WatcherState>,
    path_str: String,
) -> Result<(), String> {
    let root = PathBuf::from(path_str);
    // The old watcher goes first (dropping it stops its events), and its
    // task ends on its own once it sees the generation change: dropping
    // the watcher alone only stops new events, and its loop would keep
    // waking every 50 ms forever, one more per project opened.
    let generation = begin_watch(&state.active_watcher, &WATCH_GENERATION);

    let (tx, rx) = std::sync::mpsc::channel::<NotifyResult<Event>>();
    // Off the main thread: watching a big tree folder by folder takes a
    // while (inotify), and nothing else must wait for it.
    let watch_root = root.clone();
    let watcher = crate::util::blocking(move || {
        let mut watcher = notify::recommended_watcher(tx).err_text()?;
        // One unreadable subdir (a database owned by another user, a FUSE
        // mount with restricted permissions) must not fail the whole
        // project's watch.
        let skipped = watch_recursive_lenient(&mut watcher, &watch_root)?;
        if !skipped.is_empty() {
            eprintln!(
                "file watch: watching {:?} without {} unreadable dirs (e.g. {:?})",
                watch_root,
                skipped.len(),
                skipped.first()
            );
        }
        Ok(watcher)
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
    println!("Watching: {:?}", root);
    tauri::async_runtime::spawn(Watch::new(app_handle, root, generation, rx).run());
    Ok(())
}

/// One project's watch: the events notify sends, batched and reported.
struct Watch {
    app: AppHandle,
    root: PathBuf,
    generation: u64,
    events: Receiver<NotifyResult<Event>>,
    /// The project's .gitignore files: the root's at once, all of them when
    /// found (and again whenever one changes).
    ignores: Arc<WatchIgnores>,
    found_ignores: (Sender<WatchIgnores>, Receiver<WatchIgnores>),
    batch: WatchBatch,
    /// Last activity inside .git/ (a commit writes hundreds of files there).
    git_touched: Option<Instant>,
}

impl Watch {
    fn new(
        app: AppHandle,
        root: PathBuf,
        generation: u64,
        events: Receiver<NotifyResult<Event>>,
    ) -> Self {
        let watch = Watch {
            ignores: Arc::new(WatchIgnores::root_only(&root)),
            found_ignores: std::sync::mpsc::channel(),
            app,
            root,
            generation,
            events,
            batch: WatchBatch::default(),
            git_touched: None,
        };
        watch.find_ignores();
        watch
    }

    /// Every .gitignore of the project, looked for off the runtime.
    fn find_ignores(&self) {
        let (root, tx) = (self.root.clone(), self.found_ignores.0.clone());
        tauri::async_runtime::spawn_blocking(move || {
            let _ = tx.send(WatchIgnores::all(&root));
        });
    }

    async fn run(mut self) {
        loop {
            // Another project's watch replaced this one: stop.
            if watch_replaced(self.generation) {
                break;
            }
            if let Ok(found) = self.found_ignores.1.try_recv() {
                self.ignores = Arc::new(found);
            }
            let disconnected = self.drain();
            self.report();
            if watch_finished(disconnected, &self.batch, self.git_touched) {
                break;
            }
            // A short pause to rest the CPU before polling again.
            tokio::time::sleep(Duration::from_millis(50)).await;
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

    /// One changed path. Git's own files and ignored paths (target/,
    /// node_modules/) aren't the user's files: reporting each one flooded
    /// the app. `brings_folder`: the change can put a folder there (made,
    /// or moved in), whose contents are then reported too.
    fn saw(&mut self, operation: &WatchOp, path: PathBuf, brings_folder: bool) {
        match watch_path_kind(&self.root, &self.ignores, &path) {
            WatchPath::Git => {
                self.git_touched = Some(Instant::now());
                // A checkout, merge or rebase is rewriting the working
                // tree: isolated chats wait it out.
                if path.parent() == Some(self.root.join(".git").as_path())
                    && path
                        .file_name()
                        .is_some_and(|n| n == "HEAD" || n == "ORIG_HEAD")
                {
                    rovik_core::overlay::chats::project_busy(&self.root);
                }
                return;
            }
            WatchPath::Ignored => return,
            WatchPath::Visible => {}
        }
        let state = self.app.state::<WatcherState>();
        if path.file_name().is_some_and(|n| n == ".gitignore") {
            self.find_ignores();
            // The tree's listing re-reads it too.
            if let Ok(mut cached) = state.gitignore.lock() {
                *cached = None;
            }
        }
        let is_dir = path.is_dir();
        let appeared = brings_folder && is_dir && path.starts_with(&self.root);
        // A new dir isn't covered when the initial watch fell back to
        // per-dir watches: cover it now, best effort. (Under a recursive
        // watcher this re-watch is harmless.) Nor is one moved in.
        if appeared {
            watch_new_dir(&state, &path);
        }
        let payload = FileWatchPayload {
            path: path.to_string_lossy().into_owned(),
            operation: operation.clone(),
            is_dir,
        };
        self.batch.add(payload, Instant::now());
        // What it holds already, which no event will report (watched first,
        // so whatever comes after this look is reported by itself). Past a
        // flush's worth, the batch turns into "re-check everything".
        if appeared {
            for (inner, inner_is_dir) in
                folder_contents(&self.root, &self.ignores, &path, MAX_EVENTS_PER_FLUSH + 1)
            {
                let payload = FileWatchPayload {
                    path: inner.to_string_lossy().into_owned(),
                    operation: WatchOp::Create,
                    is_dir: inner_is_dir,
                };
                self.batch.add(payload, Instant::now());
            }
        }
    }

    /// What's due, reported. A flood is one "re-check everything"
    /// (operation "unknown", the project root). Git activity once it has
    /// settled, or a change to the working tree: the Git tab refreshes.
    fn report(&mut self) {
        let now = Instant::now();
        let flush = self.batch.take_due(now);
        // A visible file changed: git's status of the working tree did too.
        let tree_changed = flush != WatchFlush::Nothing;
        match flush {
            WatchFlush::Nothing => {}
            WatchFlush::Everything => {
                // Isolated chats on this project read it afresh.
                rovik_core::overlay::chats::project_changed(&self.root, &[], true);
                let _ = self.app.emit(
                    "fs-changes",
                    FileWatchPayload {
                        path: self.root.to_string_lossy().into_owned(),
                        operation: WatchOp::Unknown,
                        is_dir: true,
                    },
                );
            }
            WatchFlush::Events(events) => {
                let paths: Vec<PathBuf> = events.iter().map(|p| PathBuf::from(&p.path)).collect();
                // Isolated chats on this project: see it as it is now, and
                // hear right away when their own change conflicts.
                for (chat, path) in
                    rovik_core::overlay::chats::project_changed(&self.root, &paths, false)
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
                for payload in events {
                    let _ = self.app.emit("fs-changes", payload);
                }
            }
        }
        let git_settled = self
            .git_touched
            .is_some_and(|t| now.duration_since(t) >= WATCH_DEBOUNCE);
        if git_settled {
            self.git_touched = None;
        }
        if git_settled || tree_changed {
            let _ = self.app.emit("git-changed", ());
        }
    }
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
        FileWatchPayload {
            path: path.into(),
            operation: WatchOp::Modify,
            is_dir: false,
        }
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
        assert!(!watch_finished(false, &b, None), "still connected");
        assert!(watch_finished(true, &b, None));
        b.add(change("a"), t0);
        assert!(!b.is_empty());
        assert!(!watch_finished(true, &b, None), "a change still to report");
        assert!(matches!(
            b.take_due(t0 + Duration::from_secs(5)),
            WatchFlush::Events(_)
        ));
        assert!(b.is_empty(), "reported");
        assert!(
            !watch_finished(true, &b, Some(t0)),
            "git activity still to report"
        );
        assert!(watch_finished(true, &b, None));
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
        let op = |path: &str, operation: WatchOp| FileWatchPayload {
            path: path.into(),
            operation,
            is_dir: false,
        };
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
}
