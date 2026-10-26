// src-tauri/src/instances.rs
//
// Rovik runs as several processes, each with its own project: the chats,
// agent runs and file state a process keeps in memory are its own, and two
// processes on one project would each change its chats unaware of the
// other's changes. So a project is open in one process at a time. Opening a
// folder claims it, and the project ID in it (folders with the same ID,
// copies of one another, share their chats); a window with no folder open
// claims the chats without a project. A workspace claims its own ID and
// every one of its folders', all or none (docs/design/workspaces-plan.md,
// D7). A claim is an OS lock on a file named after it (rovik_core::os_lock),
// held while the project is open here and let go when another one is opened
// or the process ends (a crash too: the OS lets go of its locks). A project
// left for another while its chats are still at work here stays claimed
// until they're done. A project another process holds isn't opened again:
// that process is asked to bring its window to the front instead, over a
// localhost connection that only takes the token it wrote next to its
// locks, in the user's own data folder.

use rovik_core::os_lock::Held;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rovik_core::chat_db::projects::{folder_project_id, path_key};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The claim of a window with no folder open: the chats without a project.
const NO_PROJECT: &str = "no-project";

/// What a process holding a project wrote next to its locks: how to ask
/// it to come to the front.
#[derive(Serialize, Deserialize)]
struct Holder {
    /// The folder ("" for a workspace, and for the chats without a project).
    path: String,
    /// The workspace it's open in, when it is one (D7's notices name it).
    #[serde(default)]
    workspace: Option<String>,
    pid: u32,
    port: u16,
    token: String,
    /// Left for another project, its chats still at work there.
    #[serde(default)]
    at_work: bool,
}

/// What claiming a project came to.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    /// The project is open in this process now (or was already).
    pub claimed: bool,
    /// Another process has it open: it's not opened here.
    pub open_elsewhere: bool,
    /// The folder open there (none for a workspace, and for the chats
    /// without a project).
    pub open_as: Option<String>,
    /// The workspace it's open in there, when it's one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// That's another folder, a copy of this one sharing its chats.
    pub copy: bool,
    /// That process left it for another project, its chats still at work.
    pub at_work: bool,
    /// That process was asked to bring its window to the front, and did.
    pub focused: bool,
}

impl Claim {
    fn claimed() -> Self {
        Claim {
            claimed: true,
            ..Claim::default()
        }
    }
}

/// What a window has open (docs/design/workspaces-plan.md, D7): a folder
/// opened as itself (a project), or a workspace's folders. A claim with no
/// target is the chats without a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Folder(PathBuf),
    Workspace {
        id: String,
        name: String,
        folders: Vec<PathBuf>,
    },
}

/// One project the window lists chats for: its target's, or one it left
/// while its chats were at work here.
#[derive(Debug, Clone, PartialEq)]
struct Owner {
    /// Its ID; None for the chats without a project, and for a folder whose
    /// project the window hasn't resolved yet.
    project: Option<String>,
    /// Its folder (a workspace's owner has none).
    folder: Option<PathBuf>,
    /// The workspace it is, or the workspace its folder is open in.
    workspace: Option<WorkspaceRef>,
}

/// A workspace as a claim names it: its ID, which its folders are matched
/// by (names needn't be unique), and its name, which a notice reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceRef {
    id: String,
    name: String,
}

impl Owner {
    fn no_project() -> Self {
        Owner {
            project: None,
            folder: None,
            workspace: None,
        }
    }

    /// The keys it's claimed by: its folder's, and its project's.
    fn keys(&self) -> Vec<String> {
        match (&self.folder, &self.project) {
            (Some(folder), project) => {
                let mut keys = vec![format!("folder:{}", path_key(folder))];
                if let Some(id) = project {
                    keys.push(format!("project:{id}"));
                }
                keys
            }
            (None, Some(id)) => vec![format!("project:{id}")],
            (None, None) => vec![NO_PROJECT.to_string()],
        }
    }

    /// The ID of the workspace it is or is in, if one.
    fn workspace_id(&self) -> Option<&str> {
        self.workspace
            .as_ref()
            .map(|workspace| workspace.id.as_str())
    }

    /// The folder as a notice names it ("" for a workspace).
    fn shown(&self) -> String {
        self.folder
            .as_ref()
            .map(|folder| display(folder))
            .unwrap_or_default()
    }
}

fn display(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// A folder's path as it's compared (with links and `..` resolved).
fn same_folder(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

#[derive(Default)]
struct Claims {
    /// Every lock held, by claim.
    held: HashMap<String, Held>,
    /// The keys of the target open in the window.
    open: Vec<String>,
    /// What the window has open: only `claim` sets it.
    target: Vec<Owner>,
    /// The projects it left while their chats were at work here: they keep
    /// their keys until those are done.
    at_work: Vec<Owner>,
}

/// This process's claims on projects, and its "come to the front" line.
pub struct Instances {
    /// `<app data>/open-projects`: a `.lock` and a `.json` per claim.
    dir: PathBuf,
    port: u16,
    token: String,
    claims: Mutex<Claims>,
}

impl Instances {
    /// Starts listening for "come to the front" (`on_focus` does it), with
    /// the claims kept in `dir`.
    pub fn start(dir: PathBuf, on_focus: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let expected = token.clone();
        std::thread::Builder::new()
            .name("instances".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    // "ok" once it's done: the asking process goes on then.
                    if sent_token(&stream, &expected) {
                        on_focus();
                        let _ = (&stream).write_all(b"ok\n");
                    }
                }
            })?;
        Ok(Instances {
            dir,
            port,
            token,
            claims: Mutex::new(Claims::default()),
        })
    }

    fn claims(&self) -> std::sync::MutexGuard<'_, Claims> {
        self.claims.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens `target` in this process (None: the chats without a project)
    /// unless another one has it (or its chats) open, in which case that one
    /// is asked to come to the front (`focus_other`). A workspace takes its
    /// key and its folders', all or none.
    ///
    /// What this process had open before is let go, but for the projects in
    /// `at_work` (their chats are still at work here): they keep their keys
    /// until they're done.
    pub fn claim(
        &self,
        target: Option<&Target>,
        focus_other: bool,
        at_work: &[Option<String>],
    ) -> Claim {
        let mut claims = self.claims();
        let owners = owners_of(target, &claims.target);
        let keys = keys_of(&owners);
        let mut taken: Vec<(String, Held)> = Vec::new();
        for key in &keys {
            if claims.held.contains_key(key) {
                continue;
            }
            match self.lock(key) {
                Ok(Some(file)) => taken.push((key.clone(), file)),
                // Better a project open twice than none opened at all.
                Err(e) => log::warn!("instances: {key}: {e}"),
                Ok(None) => {
                    drop(taken);
                    return self.elsewhere(key, focus_other);
                }
            }
        }
        claims.held.extend(taken);
        // The owners left for this target: those whose chats are at work
        // here keep their keys, and the others let them go.
        let before: Vec<Owner> = std::mem::take(&mut claims.at_work)
            .into_iter()
            .chain(std::mem::take(&mut claims.target))
            .filter(|owner| !owners.iter().any(|now| same_owner(now, owner)))
            .collect();
        let busy = |owner: &Owner| {
            owner
                .project
                .as_ref()
                .is_some_and(|id| at_work.iter().any(|at| at.as_deref() == Some(id.as_str())))
        };
        let mut kept: Vec<Owner> = before.iter().filter(|owner| busy(owner)).cloned().collect();
        // A workspace whose own chats are at work keeps every folder of it:
        // a workspace chat's run uses some or all of them (D19). By the
        // workspace's ID: two workspaces may be named alike.
        let busy_workspaces: Vec<String> = kept
            .iter()
            .filter(|owner| owner.folder.is_none())
            .filter_map(Owner::workspace_id)
            .map(str::to_string)
            .collect();
        for folder in before.iter().filter(|owner| owner.folder.is_some()) {
            let of = folder.workspace_id();
            if of.is_some_and(|id| busy_workspaces.iter().any(|busy| busy == id))
                && !kept.iter().any(|held| same_owner(held, folder))
            {
                kept.push(folder.clone());
            }
        }
        claims.at_work = kept;
        let keep = keys_of(&claims.at_work);
        // A key neither the target nor a project at work holds goes.
        for key in claims.held.keys().cloned().collect::<Vec<_>>() {
            if keys.contains(&key) || keep.contains(&key) {
                continue;
            }
            if let Some(file) = claims.held.remove(&key) {
                self.let_go(&key, file);
            }
        }
        claims.open = keys;
        claims.target = owners;
        self.write_holders(&claims);
        Claim::claimed()
    }

    /// Whether another process has `target` (or a copy sharing its chats)
    /// open, as `claim` would find, without claiming it: what this process
    /// has open stays its own (a switch the user may still cancel).
    pub fn check(&self, target: Option<&Target>, focus_other: bool) -> Claim {
        let claims = self.claims();
        for key in keys_of(&owners_of(target, &[])) {
            if !claims.held.contains_key(&key) && self.held_elsewhere(&key) {
                return self.elsewhere(&key, focus_other);
            }
        }
        Claim::default()
    }

    /// The folder `root`'s project is `project` (the window opened it, or
    /// answered a question about a copy or a move): it's claimed, if the
    /// folder is one of the window's. A folder the window has left since
    /// claims nothing back (an answer that came late).
    pub fn reclaim(&self, root: &Path, project: &str) {
        let mut claims = self.claims();
        let found = rename_project(&mut claims.target, root, project)
            | rename_project(&mut claims.at_work, root, project);
        if !found {
            return;
        }
        let mut held: Vec<Owner> = claims.target.clone();
        held.extend(claims.at_work.iter().cloned());
        let wanted = keys_of(&held);
        // Its project's key, if it's free: better a project open twice than
        // one whose chats two windows change unaware.
        let missing: Vec<String> = wanted
            .iter()
            .filter(|key| !claims.held.contains_key(*key))
            .cloned()
            .collect();
        for key in missing {
            match self.lock(&key) {
                Ok(Some(file)) => {
                    claims.held.insert(key, file);
                }
                Ok(None) => log::warn!("instances: {key} is open in another window too"),
                Err(e) => log::warn!("instances: {key}: {e}"),
            }
        }
        // A folder whose ID changed (a separate history) lets the old go.
        for key in claims.held.keys().cloned().collect::<Vec<_>>() {
            if wanted.contains(&key) {
                continue;
            }
            if let Some(file) = claims.held.remove(&key) {
                self.let_go(&key, file);
            }
        }
        claims.open = keys_of(&claims.target);
        self.write_holders(&claims);
    }

    /// The projects the window lists chats for, as the claims record has
    /// them: its target's, and those it left while their chats are at work
    /// here.
    pub fn owners(&self) -> Vec<Option<String>> {
        let claims = self.claims();
        claims
            .target
            .iter()
            .chain(claims.at_work.iter())
            .map(|owner| owner.project.clone())
            .collect()
    }

    /// The projects left for another while their chats were at work here.
    pub fn left_at_work(&self) -> Vec<Option<String>> {
        self.claims()
            .at_work
            .iter()
            .map(|owner| owner.project.clone())
            .collect()
    }

    /// Lets go of the projects left at work whose chats are done (an ID
    /// `still` doesn't name), and those whose project was never known.
    pub fn let_go_done(&self, still: impl Fn(&Option<String>) -> bool) {
        let mut claims = self.claims();
        let (busy, done): (Vec<Owner>, Vec<Owner>) = std::mem::take(&mut claims.at_work)
            .into_iter()
            .partition(|owner| owner.project.is_some() && still(&owner.project));
        claims.at_work = busy;
        for owner in done {
            for key in owner.keys() {
                if let Some(file) = claims.held.remove(&key) {
                    self.let_go(&key, file);
                }
            }
        }
    }

    /// `key` is another process's: what to say, having asked it to come
    /// to the front (`focus_other`).
    fn elsewhere(&self, key: &str, focus_other: bool) -> Claim {
        let holder = read_holder(&self.files(key).1);
        let folder = holder
            .as_ref()
            .map(|h| h.path.clone())
            .filter(|path| !path.is_empty());
        Claim {
            claimed: false,
            open_elsewhere: true,
            open_as: folder.clone(),
            workspace: holder
                .as_ref()
                .and_then(|h| h.workspace.clone())
                .filter(|name| !name.is_empty()),
            // The folder's free; its project ID isn't. A workspace's key
            // isn't a copy of anything: it has no folder (D7).
            copy: key.starts_with("project:") && folder.is_some(),
            at_work: holder.as_ref().is_some_and(|h| h.at_work),
            focused: focus_other && holder.is_some_and(|h| ask_to_focus(&h)),
        }
    }

    /// The folder another process has open with the project `id` (or the
    /// folder `folder`), if one does: its chats aren't this process's to
    /// change (delete, merge).
    pub fn open_elsewhere(&self, id: Option<&str>, folder: Option<&Path>) -> Option<String> {
        let mut keys: Vec<String> = id.map(|id| format!("project:{id}")).into_iter().collect();
        keys.extend(folder.map(|f| format!("folder:{}", path_key(f))));
        let claims = self.claims();
        keys.iter()
            .filter(|k| !claims.held.contains_key(*k))
            .find(|key| self.held_elsewhere(key))
            .map(|key| {
                read_holder(&self.files(key).1)
                    .map(|h| h.path)
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| "another window".to_string())
            })
    }

    /// Lets go of every project held here.
    pub fn release(&self) {
        let mut claims = self.claims();
        claims.open.clear();
        claims.target.clear();
        claims.at_work.clear();
        for (key, file) in std::mem::take(&mut claims.held) {
            self.let_go(&key, file);
        }
    }

    /// Who holds each of this process's claims, for another to read: each
    /// key names its own folder, and the workspace it's open in.
    fn write_holders(&self, claims: &Claims) {
        let holder = |owner: &Owner, at_work: bool| Holder {
            path: owner.shown(),
            workspace: owner
                .workspace
                .as_ref()
                .map(|workspace| workspace.name.clone()),
            pid: std::process::id(),
            port: self.port,
            token: self.token.clone(),
            at_work,
        };
        let open = claims.target.iter().map(|owner| (owner, false));
        let left = claims.at_work.iter().map(|owner| (owner, true));
        for (owner, at_work) in open.chain(left) {
            let holder = holder(owner, at_work);
            for key in owner.keys() {
                let info = self.files(&key).1;
                let json = serde_json::to_vec(&holder).unwrap_or_default();
                if let Err(e) = rovik_core::atomic_file::write_atomic_private(&info, &json) {
                    log::warn!("instances: {}: {e}", info.display());
                }
            }
        }
    }

    /// The lock for `key`, if no other process holds it (None).
    fn lock(&self, key: &str) -> std::io::Result<Option<Held>> {
        rovik_core::os_lock::try_hold(&self.files(key).0)
    }

    fn held_elsewhere(&self, key: &str) -> bool {
        rovik_core::os_lock::held_elsewhere(&self.files(key).0)
    }

    /// Its `.json` first, while still locked: once the lock goes, another
    /// process may write its own.
    fn let_go(&self, key: &str, lock: Held) {
        let _ = std::fs::remove_file(self.files(key).1);
        drop(lock);
    }

    /// A claim's lock, and the file saying who holds it.
    fn files(&self, key: &str) -> (PathBuf, PathBuf) {
        let digest = Sha256::digest(key.as_bytes());
        let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        (
            self.dir.join(format!("{name}.lock")),
            self.dir.join(format!("{name}.json")),
        )
    }
}

/// The owners of a target: a folder's, or a workspace's own and each of its
/// folders'. A folder keeps the project the window resolved for it while it
/// has no ID file (a read-only folder, say).
fn owners_of(target: Option<&Target>, before: &[Owner]) -> Vec<Owner> {
    let remembered = |folder: &Path| -> Option<String> {
        before
            .iter()
            .find(|owner| {
                owner
                    .folder
                    .as_ref()
                    .is_some_and(|known| same_folder(known, folder))
            })
            .and_then(|owner| owner.project.clone())
    };
    let folder_owner = |folder: &Path, workspace: &Option<WorkspaceRef>| Owner {
        project: folder_project_id(folder).or_else(|| remembered(folder)),
        folder: Some(folder.to_path_buf()),
        workspace: workspace.clone(),
    };
    match target {
        None => vec![Owner::no_project()],
        Some(Target::Folder(path)) => vec![folder_owner(path, &None)],
        Some(Target::Workspace { id, name, folders }) => {
            let workspace = Some(WorkspaceRef {
                id: id.clone(),
                name: name.clone(),
            });
            let mut owners = vec![Owner {
                project: Some(id.clone()),
                folder: None,
                workspace: workspace.clone(),
            }];
            owners.extend(
                folders
                    .iter()
                    .map(|folder| folder_owner(folder, &workspace)),
            );
            owners
        }
    }
}

/// The keys of these owners, each once: a folder whose ID file names the
/// workspace's ID takes one key, not two.
fn keys_of(owners: &[Owner]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in owners.iter().flat_map(Owner::keys) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// The owner whose folder is `root` is the project `project` from now on:
/// whether one was.
fn rename_project(owners: &mut [Owner], root: &Path, project: &str) -> bool {
    let mut found = false;
    for owner in owners.iter_mut() {
        if owner
            .folder
            .as_ref()
            .is_some_and(|folder| same_folder(folder, root))
        {
            owner.project = Some(project.to_string());
            found = true;
        }
    }
    found
}

/// Whether two owners are the window's one project: by its ID, else by its
/// folder.
fn same_owner(a: &Owner, b: &Owner) -> bool {
    match (&a.project, &b.project) {
        (Some(a), Some(b)) => a == b,
        _ => match (&a.folder, &b.folder) {
            (Some(a), Some(b)) => same_folder(a, b),
            (None, None) => a.workspace_id().is_some() && a.workspace_id() == b.workspace_id(),
            _ => false,
        },
    }
}

/// Whether `stream` sent `token`, on a line of its own.
fn sent_token(stream: &TcpStream, token: &str) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    let mut reader = BufReader::new(stream.take(256));
    reader.read_line(&mut line).is_ok() && line.trim_end() == token
}

/// Who holds a claim, from its `.json`: just locked, it may not be written
/// yet.
fn read_holder(info: &Path) -> Option<Holder> {
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        let read = std::fs::read(info).ok();
        if let Some(holder) = read.and_then(|b| serde_json::from_slice(&b).ok()) {
            return Some(holder);
        }
    }
    None
}

/// Asks `holder` to come to the front. Whether it did.
fn ask_to_focus(holder: &Holder) -> bool {
    // Windows lets only the process the user is in bring a window forward:
    // it lets the other one do it.
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn AllowSetForegroundWindow(process_id: u32) -> i32;
        }
        // SAFETY: a plain call with a process ID; nothing is borrowed.
        unsafe { AllowSetForegroundWindow(holder.pid) };
    }
    let addr = (Ipv4Addr::LOCALHOST, holder.port).into();
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    if stream
        .write_all(format!("{}\n", holder.token).as_bytes())
        .is_err()
    {
        return false;
    }
    let mut answer = String::new();
    let _ = BufReader::new(&stream).read_line(&mut answer);
    answer.trim_end() == "ok"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "instances-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An instance keeping its claims in `dir`, and how often it was asked
    /// to come to the front. Two in one test process stand for two
    /// processes: each holds its locks through its own open files.
    fn instance(dir: &Path) -> (Instances, Arc<AtomicUsize>) {
        let focused = Arc::new(AtomicUsize::new(0));
        let count = focused.clone();
        let instance = Instances::start(dir.join("open-projects"), move || {
            count.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        (instance, focused)
    }

    /// A project folder `name` in `dir`, with the project ID `id`.
    fn folder(dir: &Path, name: &str, id: Option<&str>) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(&path).unwrap();
        if let Some(id) = id {
            rovik_core::chat_db::projects::write_id_file(&path, id).unwrap();
        }
        std::fs::canonicalize(path).unwrap()
    }

    fn elsewhere(at: &Path, focused: bool) -> Claim {
        Claim {
            claimed: false,
            open_elsewhere: true,
            open_as: Some(at.to_string_lossy().into_owned()),
            workspace: None,
            copy: false,
            at_work: false,
            focused,
        }
    }

    /// Opens a folder, as a window does: its chats aren't at work.
    fn open(instance: &Instances, path: &Path) -> Claim {
        instance.claim(Some(&Target::Folder(path.to_path_buf())), true, &[])
    }

    /// A workspace of these folders, as the window opens it.
    fn workspace(id: &str, name: &str, folders: &[&Path]) -> Target {
        Target::Workspace {
            id: id.to_string(),
            name: name.to_string(),
            folders: folders.iter().map(|f| f.to_path_buf()).collect(),
        }
    }

    #[test]
    fn a_project_open_in_one_instance_brings_that_one_to_the_front() {
        let dir = temp_dir("front");
        let app = folder(&dir, "app", Some("project-app-1"));
        let (a, a_focused) = instance(&dir);
        let (b, b_focused) = instance(&dir);

        assert_eq!(open(&a, &app), Claim::claimed());
        // Again from the same instance: still its own.
        assert_eq!(open(&a, &app), Claim::claimed());
        // The same folder, spelled otherwise: A has it.
        assert_eq!(open(&b, &dir.join("app/../app/")), elsewhere(&app, true));
        assert_eq!(a_focused.load(Ordering::SeqCst), 1);
        // Not asked to come to the front (a project restored at start).
        let target = Target::Folder(app.clone());
        assert_eq!(b.claim(Some(&target), false, &[]), elsewhere(&app, false));
        assert_eq!(a_focused.load(Ordering::SeqCst), 1);
        assert_eq!(b_focused.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_copy_sharing_the_chats_is_open_where_its_original_is() {
        let dir = temp_dir("copy");
        let original = folder(&dir, "app", Some("project-app-1"));
        let copy = folder(&dir, "app-copy", Some("project-app-1"));
        let other = folder(&dir, "other", Some("project-other"));
        let (a, a_focused) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(open(&a, &original), Claim::claimed());
        assert_eq!(
            open(&b, &copy),
            Claim {
                copy: true,
                ..elsewhere(&original, true)
            }
        );
        assert_eq!(a_focused.load(Ordering::SeqCst), 1);
        assert_eq!(open(&b, &other), Claim::claimed());
        // Given an ID of its own ("a separate history"), it's free.
        rovik_core::chat_db::projects::write_id_file(&copy, "project-app-2").unwrap();
        assert_eq!(open(&b, &copy), Claim::claimed());
        // A folder whose ID changes while open is claimed again as itself.
        rovik_core::chat_db::projects::write_id_file(&copy, "project-app-3").unwrap();
        assert_eq!(open(&b, &copy), Claim::claimed());
        let (c, _) = instance(&dir);
        assert_eq!(open(&c, &other), Claim::claimed());
    }

    #[test]
    fn a_project_is_free_again_once_its_instance_moves_on_or_ends() {
        let dir = temp_dir("free");
        let one = folder(&dir, "one", None);
        let two = folder(&dir, "two", Some("project-two"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(open(&a, &one), Claim::claimed());
        // A opens another project: the first is free.
        assert_eq!(open(&a, &two), Claim::claimed());
        assert_eq!(open(&b, &one), Claim::claimed());
        assert_eq!(open(&b, &two), elsewhere(&two, true));
        // A ends (or crashes): its locks go with it.
        drop(a);
        assert_eq!(open(&b, &two), Claim::claimed());
        // B let go of `one` for `two`, and then of everything.
        b.release();
        let (c, _) = instance(&dir);
        assert_eq!(open(&c, &one), Claim::claimed());
        assert_eq!(open(&c, &two), Claim::claimed());
    }

    #[test]
    fn a_project_left_while_its_chats_are_at_work_stays_until_theyre_done() {
        let dir = temp_dir("at-work");
        let one = folder(&dir, "one", Some("project-one"));
        let two = folder(&dir, "two", Some("project-two"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(open(&a, &one), Claim::claimed());
        a.reclaim(&one, "project-one");
        // A opens `two` while one's chats are at work there.
        let target = Target::Folder(two.clone());
        assert_eq!(
            a.claim(Some(&target), true, &[Some("project-one".into())]),
            Claim::claimed()
        );
        assert_eq!(a.left_at_work(), vec![Some("project-one".to_string())]);
        assert_eq!(
            open(&b, &one),
            Claim {
                at_work: true,
                ..elsewhere(&one, true)
            }
        );
        // Still at work: kept.
        a.let_go_done(|p| p.as_deref() == Some("project-one"));
        assert!(open(&b, &one).open_elsewhere);
        // Done: B may open it.
        a.let_go_done(|_| false);
        assert!(a.left_at_work().is_empty());
        assert_eq!(open(&b, &one), Claim::claimed());
        assert_eq!(open(&b, &two), elsewhere(&two, true));
    }

    #[test]
    fn going_back_to_a_project_left_at_work_makes_it_the_open_one_again() {
        let dir = temp_dir("back");
        let one = folder(&dir, "one", Some("project-one"));
        let two = folder(&dir, "two", Some("project-two"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(open(&a, &one), Claim::claimed());
        a.reclaim(&one, "project-one");
        assert_eq!(
            a.claim(
                Some(&Target::Folder(two.clone())),
                true,
                &[Some("project-one".into())]
            ),
            Claim::claimed()
        );
        assert_eq!(open(&a, &one), Claim::claimed());
        assert!(a.left_at_work().is_empty());
        // Open again, not at work: B is told it's open there.
        assert_eq!(open(&b, &one), elsewhere(&one, true));
        // A's chats elsewhere being done doesn't let go of the open one.
        a.let_go_done(|_| false);
        assert!(open(&b, &one).open_elsewhere);
    }

    #[test]
    fn the_chats_without_a_project_are_one_windows_at_a_time() {
        let dir = temp_dir("no-project");
        let app = folder(&dir, "app", Some("project-app"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(a.claim(None, false, &[]), Claim::claimed());
        assert_eq!(a.owners(), vec![None]);
        assert_eq!(
            b.claim(None, false, &[]),
            Claim {
                open_as: None,
                ..elsewhere(&app, false)
            }
        );
        // A opens a folder: they're free.
        assert_eq!(open(&a, &app), Claim::claimed());
        assert_eq!(b.claim(None, false, &[]), Claim::claimed());
        assert!(b.check(None, false) == Claim::default());
        assert!(a.check(None, false).open_elsewhere);
    }

    #[test]
    fn checking_a_project_claims_nothing_and_keeps_what_is_open() {
        let dir = temp_dir("check");
        let one = folder(&dir, "one", Some("project-one"));
        let two = folder(&dir, "two", Some("project-two"));
        let (a, a_focused) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(open(&a, &one), Claim::claimed());
        assert_eq!(open(&b, &two), Claim::claimed());
        // B checks one: A has it, and comes to the front.
        let target = Target::Folder(one.clone());
        assert_eq!(b.check(Some(&target), true), elsewhere(&one, true));
        assert_eq!(a_focused.load(Ordering::SeqCst), 1);
        // A checks a free folder: nothing's claimed, and A keeps `one`.
        let three = folder(&dir, "three", None);
        assert_eq!(
            a.check(Some(&Target::Folder(three.clone())), true),
            Claim::default()
        );
        assert_eq!(
            b.claim(Some(&Target::Folder(one.clone())), false, &[]),
            elsewhere(&one, false)
        );
        assert_eq!(open(&b, &three), Claim::claimed());
    }

    #[test]
    fn a_project_open_elsewhere_is_known_by_its_id_or_folder() {
        let dir = temp_dir("known");
        let app = folder(&dir, "app", Some("project-app-1"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);
        let shown = Some(app.to_string_lossy().into_owned());

        assert_eq!(open(&a, &app), Claim::claimed());
        assert_eq!(b.open_elsewhere(Some("project-app-1"), None), shown);
        assert_eq!(b.open_elsewhere(None, Some(&app)), shown);
        assert_eq!(b.open_elsewhere(Some("project-other"), None), None);
        // Not elsewhere for the one that has it.
        assert_eq!(a.open_elsewhere(Some("project-app-1"), Some(&app)), None);
        // Asking doesn't claim: once A lets go, B can open it.
        a.release();
        assert_eq!(b.open_elsewhere(Some("project-app-1"), Some(&app)), None);
        assert_eq!(open(&b, &app), Claim::claimed());
    }

    /// D7: a workspace takes its own key and its folders', all or none: a
    /// window that can't have one folder has nothing of the workspace, and
    /// one that stops on a key it couldn't take holds none of the others.
    #[test]
    fn a_workspace_takes_its_key_and_its_folders_or_none() {
        let dir = temp_dir("workspace");
        let api = folder(&dir, "api", Some("project-api"));
        let web = folder(&dir, "web", Some("project-web"));
        let shop = workspace("workspace-shop", "Shop", &[&api, &web]);
        let (a, a_focused) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(a.claim(Some(&shop), true, &[]), Claim::claimed());
        // Every key is A's: the workspace's, each folder's and each project's.
        for target in [
            shop.clone(),
            Target::Folder(api.clone()),
            Target::Folder(web.clone()),
        ] {
            assert!(b.claim(Some(&target), false, &[]).open_elsewhere);
        }
        // A window that can't have one of the folders has nothing of it:
        // it holds no key of the workspace it tried, so once A lets go, B
        // takes the whole workspace.
        assert_eq!(
            b.claim(Some(&shop), false, &[]),
            Claim {
                open_as: None,
                workspace: Some("Shop".into()),
                ..elsewhere(&api, false)
            }
        );
        // A folder of another window's workspace is named with it, and
        // isn't a copy of anything: it has a folder, the workspace's ID.
        let claim = b.claim(Some(&Target::Folder(api.clone())), false, &[]);
        assert_eq!(claim.open_as, Some(api.to_string_lossy().into_owned()));
        assert_eq!(claim.workspace.as_deref(), Some("Shop"));
        assert!(!claim.copy);
        assert_eq!(a_focused.load(Ordering::SeqCst), 0);
        a.release();
        assert_eq!(b.claim(Some(&shop), true, &[]), Claim::claimed());
    }

    /// D7: a target's keys are each once, even when a folder's ID file names
    /// the workspace's own ID.
    #[test]
    fn a_targets_keys_have_no_duplicates() {
        let dir = temp_dir("keys");
        let api = folder(&dir, "api", Some("workspace-shop"));
        let shop = workspace("workspace-shop", "Shop", &[&api]);
        let keys = keys_of(&owners_of(Some(&shop), &[]));
        let mut once = keys.clone();
        once.sort();
        once.dedup();
        assert_eq!(keys.len(), once.len(), "{keys:?}");
        assert_eq!(
            keys.len(),
            2,
            "the workspace's (which the folder's ID file names) and the folder's"
        );
    }

    /// D7: adding a folder to a workspace claims its keys, and removing one
    /// lets only its own go.
    #[test]
    fn adding_and_removing_a_folder_keep_the_others_keys() {
        let dir = temp_dir("add-remove");
        let api = folder(&dir, "api", Some("project-api"));
        let web = folder(&dir, "web", Some("project-web"));
        let other = folder(&dir, "other", Some("project-other"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(
            a.claim(Some(&workspace("w", "Shop", &[&api])), true, &[]),
            Claim::claimed()
        );
        assert_eq!(
            a.claim(Some(&workspace("w", "Shop", &[&api, &web])), true, &[]),
            Claim::claimed()
        );
        assert!(open(&b, &web).open_elsewhere, "the folder added is A's");
        assert!(open(&b, &api).open_elsewhere);
        // `web` leaves: only its keys go.
        assert_eq!(
            a.claim(Some(&workspace("w", "Shop", &[&api])), true, &[]),
            Claim::claimed()
        );
        assert!(open(&b, &api).open_elsewhere, "the folder kept is A's");
        assert_eq!(open(&b, &web), Claim::claimed());
        // So does another folder, added and then removed.
        assert_eq!(
            a.claim(Some(&workspace("w", "Shop", &[&api, &other])), true, &[]),
            Claim::claimed()
        );
        assert!(open(&b, &other).open_elsewhere);
        assert_eq!(
            a.claim(Some(&workspace("w", "Shop", &[&api])), true, &[]),
            Claim::claimed()
        );
        assert_eq!(open(&b, &other), Claim::claimed());
    }

    /// D7: two folders resolved at once both keep their keys, and a folder
    /// the window has left takes nothing back when its answer comes late.
    #[test]
    fn two_folders_resolved_at_once_keep_their_keys() {
        let dir = temp_dir("resolve");
        let api = folder(&dir, "api", None);
        let web = folder(&dir, "web", None);
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(
            a.claim(Some(&workspace("w", "Shop", &[&api, &web])), true, &[]),
            Claim::claimed()
        );
        a.reclaim(&api, "project-api");
        a.reclaim(&web, "project-web");
        let copy = folder(&dir, "api-copy", Some("project-api"));
        assert!(open(&b, &copy).copy, "api's project is A's");
        let copy = folder(&dir, "web-copy", Some("project-web"));
        assert!(open(&b, &copy).copy, "web's project is A's");

        // The window moves on; api's answer comes late.
        let other = folder(&dir, "other", Some("project-other"));
        assert_eq!(
            a.claim(Some(&Target::Folder(other.clone())), true, &[]),
            Claim::claimed()
        );
        a.reclaim(&api, "project-late");
        assert_eq!(a.open_elsewhere(Some("project-late"), None), None);
    }

    /// D7 and D19: leaving a workspace whose own chats are at work keeps the
    /// workspace and every folder; leaving one whose folders' chats are at
    /// work keeps those folders.
    #[test]
    fn leaving_a_workspace_keeps_the_keys_of_the_chats_at_work() {
        let dir = temp_dir("workspace-at-work");
        let api = folder(&dir, "api", Some("project-api"));
        let web = folder(&dir, "web", Some("project-web"));
        let other = folder(&dir, "other", Some("project-other"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);
        let shop = workspace("workspace-shop", "Shop", &[&api, &web]);
        assert_eq!(a.claim(Some(&shop), true, &[]), Claim::claimed());

        // A workspace chat of Shop is at work: Shop and both folders stay.
        let target = Target::Folder(other.clone());
        assert_eq!(
            a.claim(Some(&target), true, &[Some("workspace-shop".into())]),
            Claim::claimed()
        );
        assert!(
            b.claim(Some(&shop), false, &[]).open_elsewhere,
            "Shop stays"
        );
        assert!(open(&b, &api).open_elsewhere);
        assert!(open(&b, &web).open_elsewhere);
        a.let_go_done(|_| false);
        assert_eq!(
            b.check(Some(&Target::Folder(api.clone())), false),
            Claim::default(),
            "done: api is free"
        );

        // Now the folders' own chats are at work: the folders stay, and the
        // workspace's own key, whose chats aren't, goes.
        let (c, _) = instance(&dir);
        assert_eq!(c.claim(Some(&shop), true, &[]), Claim::claimed());
        let (d, _) = instance(&dir);
        let at_work = [
            Some("project-api".to_string()),
            Some("project-web".to_string()),
        ];
        let away = Target::Folder(folder(&dir, "away", Some("project-away")));
        assert_eq!(c.claim(Some(&away), true, &at_work), Claim::claimed());
        assert_eq!(
            d.open_elsewhere(Some("workspace-shop"), None),
            None,
            "Shop's own key went"
        );
        assert!(open(&d, &api).open_elsewhere);
        assert!(open(&d, &web).open_elsewhere);
    }

    /// D7: a folder opened alone keeps its keys while a workspace chat that
    /// uses it is at work.
    #[test]
    fn a_folder_opened_alone_keeps_its_keys_while_a_workspace_chat_uses_it() {
        let dir = temp_dir("folder-at-work");
        let api = folder(&dir, "api", Some("project-api"));
        let other = folder(&dir, "other", Some("project-other"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(open(&a, &api), Claim::claimed());
        let target = Target::Folder(other.clone());
        assert_eq!(
            a.claim(Some(&target), true, &[Some("project-api".into())]),
            Claim::claimed()
        );
        let copy = folder(&dir, "api-copy", Some("project-api"));
        assert!(open(&b, &copy).open_elsewhere, "a workspace chat uses api");
        a.let_go_done(|_| false);
        assert_eq!(open(&b, &copy), Claim::claimed(), "then api is free");
        assert!(
            !b.claim(Some(&Target::Folder(api)), false, &[])
                .open_elsewhere
        );
    }

    /// D7: another window's workspace isn't "a copy sharing its chats": it
    /// has no folder to be a copy of.
    #[test]
    fn a_workspace_open_elsewhere_isnt_a_copy() {
        let dir = temp_dir("workspace-copy");
        let api = folder(&dir, "api", Some("project-api"));
        let shop = workspace("workspace-shop", "Shop", &[&api]);
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);

        assert_eq!(a.claim(Some(&shop), true, &[]), Claim::claimed());
        let claim = b.claim(Some(&shop), false, &[]);
        assert!(claim.open_elsewhere);
        assert!(!claim.copy);
        assert_eq!(claim.open_as, None);
        assert_eq!(claim.workspace.as_deref(), Some("Shop"));
        // A folder of it is named with its workspace.
        let claim = b.claim(Some(&Target::Folder(api.clone())), false, &[]);
        assert_eq!(claim.open_as, Some(api.to_string_lossy().into_owned()));
        assert_eq!(claim.workspace.as_deref(), Some("Shop"));
    }

    /// D7: two workspaces may be named alike. A folder is kept at work for
    /// its own workspace, not for another one of the same name.
    #[test]
    fn two_workspaces_named_alike_are_told_apart_by_their_ids() {
        let dir = temp_dir("same-name");
        let api = folder(&dir, "api", Some("project-api"));
        let web = folder(&dir, "web", Some("project-web"));
        let away = folder(&dir, "away", Some("project-away"));
        let (a, _) = instance(&dir);
        let (b, _) = instance(&dir);
        let shop_a = workspace("w-a", "Shop", &[&api]);
        let shop_b = workspace("w-b", "Shop", &[&web]);
        let target = Target::Folder(away);

        // A opens its "Shop" and leaves it, while another window's workspace
        // of that same name has its chats at work.
        assert_eq!(a.claim(Some(&shop_a), true, &[]), Claim::claimed());
        assert_eq!(
            a.claim(Some(&target), true, &[Some("w-b".into())]),
            Claim::claimed()
        );
        assert_eq!(a.left_at_work(), Vec::new(), "A's own workspace wasn't");
        assert_eq!(
            open(&b, &api),
            Claim::claimed(),
            "api is kept for the workspace it's in, not for one named alike"
        );

        // Its own workspace's chats at work do keep it, told apart by ID.
        let away2 = folder(&dir, "away2", Some("project-away-2"));
        let (c, _) = instance(&dir);
        let (d, _) = instance(&dir);
        assert_eq!(c.claim(Some(&shop_b), true, &[]), Claim::claimed());
        assert_eq!(
            c.claim(Some(&Target::Folder(away2)), true, &[Some("w-b".into())]),
            Claim::claimed()
        );
        assert!(c.left_at_work().contains(&Some("project-web".to_string())));
        assert!(
            open(&d, &web).open_elsewhere,
            "web stays with its workspace"
        );
    }

    #[test]
    fn only_the_token_brings_an_instance_to_the_front() {
        let dir = temp_dir("token");
        let (a, focused) = instance(&dir);
        for line in ["", "not the token", &format!("{}x", a.token)] {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, a.port)).unwrap();
            stream.write_all(format!("{line}\n").as_bytes()).unwrap();
            let mut answer = String::new();
            let _ = BufReader::new(&stream).read_line(&mut answer);
            assert_eq!(answer, "");
        }
        assert_eq!(focused.load(Ordering::SeqCst), 0);
    }
}
