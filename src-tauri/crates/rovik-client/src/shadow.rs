//! Shadow repositories and snapshots (6.4, D7). Every project gets a
//! repository of its own in app data, whose work tree is the project's
//! repository folder: read, never checked out into. A snapshot is the tree
//! `git add -A` would commit there, made without writing a byte to the
//! project, and its commit goes to `refs/rovik/snapshots/<chat>`, for the
//! packs (T02) to take to a daemon.
//!
//! - **The index** is the shadow's own and keeps every file's stat data, so
//!   a snapshot reads only the files that changed since the last one. It is
//!   staged against the project's `HEAD` (`rovik_core::staging`): a tracked
//!   file that matches an ignore rule stays in and keeps its mode (K30), and
//!   entries `HEAD` doesn't have that an ignore rule matches now come out.
//! - **A git project lends its objects** through `objects/info/alternates`,
//!   so nothing is written to its `.git`. Its `info/exclude` and
//!   `info/attributes` are copied in, and the user's global excludes come
//!   from their git config, as for the project (K32).
//! - **A subfolder of a repository** snapshots the whole repository, so the
//!   ignore rules above it apply and the history matches (K31).
//! - **Line endings** are what `git add` would store (K40): `core.autocrlf`
//!   comes from the project, with its attributes, and `core.filemode` and
//!   `core.symlinks` follow the OS.
//! - **The parent** is the project's `HEAD` when it and its history can be
//!   read, so the history goes with the first push. Otherwise, and once the
//!   user has chosen to send the project without its history, it is the
//!   chat's previous snapshot, or none.
//!
//! Each operation holds the shadow for itself: two at once would race on
//! its index.

mod guards;

pub use guards::{Guards, LARGE_FILE, LARGE_HISTORY};

use git2::{Commit, Config, Oid, Reference, Repository, RepositoryInitOptions, Signature};
use rovik_core::staging::{self, Base, Staged};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, String>;

/// The project's settings that decide what `git add` stores and how a
/// checkout writes files (T06): the shadow takes them each time it opens.
const FROM_PROJECT: [&str; 4] = [
    "core.autocrlf",
    "core.eol",
    "core.excludesfile",
    "core.attributesfile",
];

/// Rules that live only in the project's git folder (K32, K40).
const INFO_FILES: [&str; 2] = ["info/exclude", "info/attributes"];

const SNAPSHOTS: &str = "refs/rovik/snapshots/";
/// False once the user has chosen to send the project without its history.
const HISTORY: &str = "rovik.history";
/// When the loose objects were last packed, in seconds since the epoch.
const PACKED_AT: &str = "rovik.packedat";
const DAY: i64 = 24 * 60 * 60;

/// A project's shadow repository.
pub struct Shadow {
    repo: Repository,
    /// The project's repository, when it is in one.
    project: Option<Repository>,
    /// Where the project sits in the snapshots' tree: "" at its root.
    subpath: String,
    busy: Arc<Mutex<()>>,
}

/// A snapshot of the project, at the chat's ref.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub commit: Oid,
    pub tree: Oid,
    /// The tree isn't the chat's previous snapshot's, or there was none.
    pub changed: bool,
    /// The files read and stored this time: those that changed since the
    /// last snapshot.
    pub hashed: usize,
    /// Nested repositories, left out: v1 doesn't carry them.
    pub nested: Vec<String>,
}

impl Shadow {
    /// Opens the shadow at `dir` for the project at `root`, creating it the
    /// first time, and brings its settings, borrowed objects and rules in
    /// line with the project.
    pub fn open(dir: &Path, root: &Path) -> Result<Shadow> {
        let busy = busy(dir);
        let held = hold(&busy);
        let project = Repository::discover(root)
            .ok()
            .filter(|repo| repo.workdir().is_some());
        let workdir = match project.as_ref().and_then(Repository::workdir) {
            Some(workdir) => workdir.to_path_buf(),
            None => std::path::absolute(root).map_err(|e| e.to_string())?,
        };
        let real = |path: &Path| {
            fs::canonicalize(path).map_err(|e| format!("Can't read {}: {e}", path.display()))
        };
        let (root, top) = (real(root)?, real(&workdir)?);
        let subpath = root.strip_prefix(&top).map(slashed).unwrap_or_default();
        if !dir.join("HEAD").is_file() {
            create(dir)?;
        }
        // A shadow in the folder it snapshots stays out of its own snapshots.
        let inside = real(dir)?.strip_prefix(&top).map(slashed).ok();
        configure(dir, &workdir, project.as_ref(), inside.as_deref())?;
        let repo = Repository::open(dir).map_err(git)?;
        drop(held);
        Ok(Shadow {
            repo,
            project,
            subpath,
            busy,
        })
    }

    /// The shadow repository, for the packs.
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// Where the project sits in the snapshots' tree: "" at its root, or its
    /// folder in its repository (K31).
    pub fn subpath(&self) -> &str {
        &self.subpath
    }

    /// The ignore rules only this computer has (K32): the project's
    /// `info/exclude` and the user's global excludes, which a daemon puts in
    /// the project's repository so that the chat's worktree ignores the
    /// same files.
    pub fn excludes(&self) -> Vec<String> {
        let config = self.repo.config();
        let global = config.and_then(|c| c.get_path("core.excludesfile")).ok();
        let files = [self.repo.path().join("info/exclude")]
            .into_iter()
            .chain(global.or_else(xdg_excludes));
        let mut rules = Vec::new();
        for text in files.filter_map(|file| fs::read_to_string(file).ok()) {
            let lines = text
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
            rules.extend(lines.map(String::from));
        }
        rules
    }

    /// Takes a snapshot of the project for `chat`. A tree that hasn't
    /// changed keeps the chat's previous commit.
    pub fn snapshot(&self, chat: &str) -> Result<Snapshot> {
        let name = format!("{SNAPSHOTS}{chat}");
        if !Reference::is_valid_name(&name) {
            return Err(format!("Not a chat's id: {chat}"));
        }
        let _held = hold(&self.busy);
        let head = self.head();
        let head_tree = head.as_ref().and_then(|commit| commit.tree().ok());
        let has_head = self.project.as_ref().is_some_and(|p| p.head().is_ok());
        let base = match (&head_tree, has_head) {
            (Some(tree), _) => Base::Tree(tree),
            (None, true) => Base::Unknown,
            (None, false) => Base::Nothing,
        };
        let Staged {
            tree,
            hashed,
            nested,
        } = staging::stage(&self.repo, base)?;
        let previous = self
            .repo
            .find_reference(&name)
            .and_then(|reference| reference.peel_to_commit())
            .ok();
        let changed = previous.as_ref().is_none_or(|p| p.tree_id() != tree);
        let parent = self
            .history_parent(head.as_ref())
            .or_else(|| previous.as_ref().map(Commit::id));
        // The previous snapshot stands when its tree is the same and it goes
        // on what this one would: `HEAD`, or the chain it ends.
        let same = previous.filter(|p| {
            p.tree_id() == tree && (parent == Some(p.id()) || p.parent_id(0).ok() == parent)
        });
        let commit = match same {
            Some(previous) => previous.id(),
            None => self.commit(tree, parent, chat)?,
        };
        self.repo
            .reference(&name, commit, true, "snapshot")
            .map_err(git)?;
        self.upkeep()?;
        Ok(Snapshot {
            commit,
            tree,
            changed,
            hashed,
            nested,
        })
    }

    /// Whether snapshots carry the project's history.
    pub fn with_history(&self) -> bool {
        self.repo
            .config()
            .and_then(|config| config.get_bool(HISTORY))
            .unwrap_or(true)
    }

    /// Keeps the user's choice for the project (6.4's guard). Without its
    /// history, snapshots go on each other instead of on `HEAD`; the ones
    /// taken so far are on `HEAD`, so the next ones start afresh.
    pub fn set_history(&self, with: bool) -> Result<()> {
        let _held = hold(&self.busy);
        self.repo
            .config()
            .and_then(|mut config| config.set_bool(HISTORY, with))
            .map_err(git)?;
        if !with {
            let mut snapshots = self
                .repo
                .references_glob(&format!("{SNAPSHOTS}*"))
                .map_err(git)?;
            let names: Vec<String> = snapshots.names().flatten().map(String::from).collect();
            for name in names {
                self.repo
                    .find_reference(&name)
                    .and_then(|mut reference| reference.delete())
                    .map_err(git)?;
            }
        }
        Ok(())
    }

    /// Packs the shadow's loose objects into one pack and deletes them,
    /// returning how many there were. Snapshots do it once a day.
    pub fn pack_loose(&self) -> Result<usize> {
        let _held = hold(&self.busy);
        self.pack()
    }

    /// The project's `HEAD`, when the shadow can read it and its tree.
    fn head(&self) -> Option<Commit<'_>> {
        let id = self.project.as_ref()?.head().ok()?.target()?;
        let commit = self.repo.find_commit(id).ok()?;
        commit.tree().ok()?;
        Some(commit)
    }

    /// `HEAD`, when the snapshot goes on it: the project is sent with its
    /// history, and that history can be read.
    fn history_parent(&self, head: Option<&Commit>) -> Option<Oid> {
        let (head, project) = (head?, self.project.as_ref()?);
        let readable = !project.is_shallow()
            && head
                .parent_ids()
                .all(|parent| self.repo.find_commit(parent).is_ok());
        (readable && self.with_history()).then(|| head.id())
    }

    fn commit(&self, tree: Oid, parent: Option<Oid>, chat: &str) -> Result<Oid> {
        let tree = self.repo.find_tree(tree).map_err(git)?;
        let parent = parent
            .map(|id| self.repo.find_commit(id))
            .transpose()
            .map_err(git)?;
        let parents: Vec<&Commit> = parent.iter().collect();
        let rovik = Signature::now("Rovik", "snapshots@rovik.invalid").map_err(git)?;
        let message = format!("Snapshot for chat {chat}");
        self.repo
            .commit(None, &rovik, &rovik, &message, &tree, &parents)
            .map_err(git)
    }

    /// Packs the loose objects, once a day.
    fn upkeep(&self) -> Result<()> {
        let mut config = self.repo.config().map_err(git)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() as i64);
        if !due(config.get_i64(PACKED_AT).ok(), now) {
            return Ok(());
        }
        self.pack()?;
        config.set_i64(PACKED_AT, now).map_err(git)
    }

    fn pack(&self) -> Result<usize> {
        let objects = self.repo.path().join("objects");
        let loose = loose_objects(&objects);
        if loose.is_empty() {
            return Ok(0);
        }
        let mut builder = self.repo.packbuilder().map_err(git)?;
        for (id, _) in &loose {
            builder.insert_object(*id, None).map_err(git)?;
        }
        builder.write(&objects.join("pack"), 0).map_err(git)?;
        for (_, file) in &loose {
            let _ = fs::remove_file(file);
        }
        for dir in loose.iter().filter_map(|(_, file)| file.parent()) {
            let _ = fs::remove_dir(dir);
        }
        Ok(loose.len())
    }
}

/// A new shadow: bare until `configure` gives it the project's folder as
/// its work tree. Initialising it with a work tree would write a `.git`
/// file there.
fn create(dir: &Path) -> Result<()> {
    let mut opts = RepositoryInitOptions::new();
    opts.bare(true).mkpath(true).external_template(false);
    Repository::init_opts(dir, &opts).map(drop).map_err(git)
}

/// Brings the shadow in line with the project, writing only what changed:
/// its work tree, the OS's file modes and links, the project's settings
/// and rules, and the object folder it borrows. `inside` is the shadow's
/// own folder when it is in its work tree.
fn configure(
    dir: &Path,
    workdir: &Path,
    project: Option<&Repository>,
    inside: Option<&str>,
) -> Result<()> {
    let worktree = workdir
        .to_str()
        .ok_or_else(|| format!("The project's path isn't UTF-8: {}", workdir.display()))?;
    let worktree = worktree.replace('\\', "/");
    let unix = if cfg!(unix) { "true" } else { "false" };
    let mut config = Config::open(&dir.join("config")).map_err(git)?;
    ensure(&mut config, "core.bare", Some("false"))?;
    ensure(
        &mut config,
        "core.worktree",
        Some(worktree.trim_end_matches('/')),
    )?;
    ensure(&mut config, "core.filemode", Some(unix))?;
    ensure(&mut config, "core.symlinks", Some(unix))?;
    // `git add` warns about CRLF it would change; libgit2 would refuse it.
    ensure(&mut config, "core.safecrlf", Some("false"))?;
    let settings = project
        .and_then(|repo| repo.config().ok())
        .and_then(|mut config| config.snapshot().ok());
    for key in FROM_PROJECT {
        let value = settings.as_ref().and_then(|s| s.get_string(key).ok());
        ensure(&mut config, key, value.as_deref())?;
    }
    let git_dir = project.map(Repository::commondir);
    for item in INFO_FILES {
        let mut rules = git_dir.and_then(|git_dir| fs::read(git_dir.join(item)).ok());
        if let (Some(inside), "info/exclude") = (inside, item) {
            let rules = rules.get_or_insert_with(Vec::new);
            rules.extend(format!("\n/{inside}/\n").bytes());
        }
        mirror(&dir.join(item), rules.as_deref())?;
    }
    let alternates = git_dir.map(|git_dir| format!("{}\n", slashed(&git_dir.join("objects"))));
    let alternates = alternates.as_deref().map(str::as_bytes);
    if mirror(&dir.join("objects/info/alternates"), alternates)? {
        // The index may name objects only the old store had: every file is
        // read again.
        let _ = fs::remove_file(dir.join("index"));
    }
    Ok(())
}

/// Sets `key` to `value`, or removes it for none, writing only a change.
fn ensure(config: &mut Config, key: &str, value: Option<&str>) -> Result<()> {
    let now = config.get_string(key).ok();
    match value {
        Some(value) if now.as_deref() != Some(value) => config.set_str(key, value).map_err(git),
        None if now.is_some() => config.remove(key).map_err(git),
        _ => Ok(()),
    }
}

/// Makes `path` hold `content`, or not be there for none. True when it
/// changed.
fn mirror(path: &Path, content: Option<&[u8]>) -> Result<bool> {
    if fs::read(path).ok().as_deref() == content {
        return Ok(false);
    }
    let written = match content {
        Some(bytes) => {
            let next = path.with_extension("rovik-next");
            path.parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| {
                    fs::write(&next, bytes)?;
                    fs::rename(&next, path)
                })
        }
        None => fs::remove_file(path),
    };
    written.map_err(|e| format!("Can't write {}: {e}", path.display()))?;
    Ok(true)
}

/// The loose objects under `objects`, with their files.
fn loose_objects(objects: &Path) -> Vec<(Oid, PathBuf)> {
    let mut loose = Vec::new();
    for fanout in fs::read_dir(objects).into_iter().flatten().flatten() {
        let prefix = fanout.file_name().to_string_lossy().into_owned();
        if prefix.len() != 2 {
            continue;
        }
        for file in fs::read_dir(fanout.path()).into_iter().flatten().flatten() {
            let rest = file.file_name().to_string_lossy().into_owned();
            if let (38, Ok(id)) = (rest.len(), Oid::from_str(&format!("{prefix}{rest}"))) {
                loose.push((id, file.path()));
            }
        }
    }
    loose
}

/// The user's global excludes when their git config names none: git's own
/// default, in the XDG config folder.
fn xdg_excludes() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("git/ignore"))
}

/// Whether the loose objects are due to be packed: never packed, or a day
/// since they were.
fn due(last: Option<i64>, now: i64) -> bool {
    last.is_none_or(|last| now - last >= DAY)
}

/// Each shadow's lock, by its folder.
fn busy(dir: &Path) -> Arc<Mutex<()>> {
    static BUSY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut all = hold(BUSY.get_or_init(Default::default));
    all.entry(dir.to_path_buf()).or_default().clone()
}

fn hold<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|e| e.into_inner())
}

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn git(e: git2::Error) -> String {
    e.message().to_string()
}
