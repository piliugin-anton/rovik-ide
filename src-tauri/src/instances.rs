// src-tauri/src/instances.rs
//
// Rovik runs as several processes, each with its own project: the chats,
// agent runs and file state a process keeps in memory are its own, and two
// processes on one project would each change its chats unaware of the
// other's changes. So a project is open in one process at a time. Opening
// a folder claims it, and the project ID in it (folders with the same ID,
// copies of one another, share their chats); a window with no folder open
// claims the chats without a project. A claim is an OS lock on a file
// named after it (rovik_core::os_lock), held while the project is open
// here and let go when another one is opened or the process ends (a crash
// too: the OS lets go of its locks). A project left for another while its
// chats are still at work here stays claimed until they're done. A project
// another process holds isn't opened again: that process is asked to bring
// its window to the front instead, over a localhost connection that only
// takes the token it wrote next to its locks, in the user's own data
// folder.

use std::collections::HashMap;
use std::fs::File;
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

/// What the process holding a project wrote next to its locks: how to ask
/// it to come to the front.
#[derive(Serialize, Deserialize)]
struct Holder {
    /// The folder ("" for the chats without a project).
    path: String,
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
    /// The folder open there (none for the chats without a project).
    pub open_as: Option<String>,
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

/// A project this process left for another while its chats were at work.
struct AtWork {
    keys: Vec<String>,
    path: String,
    project: Option<Option<String>>,
}

#[derive(Default)]
struct Claims {
    /// Every lock held, by claim.
    held: HashMap<String, File>,
    /// The project open in the window: its claims, its folder ("" for none),
    /// and its project's ID once the window knows it (`opened`).
    open: Vec<String>,
    path: String,
    project: Option<Option<String>>,
    at_work: Vec<AtWork>,
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

    /// Opens the folder `project` in this process (None: the chats without
    /// a project) unless another one has it (or its chats) open, in which
    /// case that one is asked to come to the front (`focus_other`). What
    /// this process had open before is let go, or kept while its chats are
    /// at work here (`keep_open`).
    pub fn claim(&self, project: Option<&Path>, focus_other: bool, keep_open: bool) -> Claim {
        let keys = claim_keys(project);
        let path = project
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut claims = self.claims();
        let mut taken: HashMap<String, File> = HashMap::new();
        for key in &keys {
            if claims.held.contains_key(key) {
                continue;
            }
            match self.lock(key) {
                Ok(Some(file)) => {
                    taken.insert(key.clone(), file);
                }
                // Better a project open twice than none opened at all.
                Err(e) => log::warn!("instances: {key}: {e}"),
                Ok(None) => {
                    drop(taken);
                    return self.elsewhere(key, focus_other);
                }
            }
        }
        claims.held.extend(taken);
        // Back to a project left at work: it's the open one again.
        for left in claims.at_work.iter_mut() {
            left.keys.retain(|k| !keys.contains(k));
        }
        claims.at_work.retain(|left| !left.keys.is_empty());
        let before: Vec<String> = std::mem::take(&mut claims.open)
            .into_iter()
            .filter(|k| !keys.contains(k))
            .collect();
        let (was, was_project) = (
            std::mem::replace(&mut claims.path, path),
            claims.project.take(),
        );
        if before.is_empty() {
            // The same project, claimed again (its ID file written meanwhile).
            claims.project = was_project.filter(|_| was == claims.path);
        } else if keep_open {
            claims.at_work.push(AtWork {
                keys: before,
                path: was,
                project: was_project,
            });
        } else {
            for key in before {
                if let Some(file) = claims.held.remove(&key) {
                    self.let_go(&key, file);
                }
            }
        }
        if project.is_none() {
            claims.project = Some(None);
        }
        claims.open = keys;
        self.write_holders(&claims);
        Claim::claimed()
    }

    /// Whether another process has `project` (or a copy sharing its chats)
    /// open, as `claim` would find, without claiming it: what this process
    /// has open stays its own (a switch the user may still cancel).
    pub fn check(&self, project: Option<&Path>, focus_other: bool) -> Claim {
        let claims = self.claims();
        for key in claim_keys(project) {
            if !claims.held.contains_key(&key) && self.held_elsewhere(&key) {
                return self.elsewhere(&key, focus_other);
            }
        }
        Claim::default()
    }

    /// The project open in the window is the one with the ID `project`
    /// (None: the chats without one), as the window found once it opened.
    pub fn opened(&self, project: Option<String>) {
        self.claims().project = Some(project);
    }

    /// The project open in the window, if known: its ID (None: the chats
    /// without a project).
    pub fn open_project(&self) -> Option<Option<String>> {
        self.claims().project.clone()
    }

    /// The projects left for another while their chats were at work here.
    pub fn left_at_work(&self) -> Vec<Option<String>> {
        let claims = self.claims();
        claims
            .at_work
            .iter()
            .filter_map(|left| left.project.clone())
            .collect()
    }

    /// Lets go of the projects left at work whose chats are done (an ID
    /// `still` doesn't name), and those whose project was never known.
    pub fn let_go_done(&self, still: impl Fn(&Option<String>) -> bool) {
        let mut claims = self.claims();
        let (busy, done): (Vec<AtWork>, Vec<AtWork>) = std::mem::take(&mut claims.at_work)
            .into_iter()
            .partition(|left| left.project.as_ref().is_some_and(&still));
        claims.at_work = busy;
        for left in done {
            for key in left.keys {
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
        Claim {
            claimed: false,
            open_elsewhere: true,
            open_as: holder
                .as_ref()
                .map(|h| h.path.clone())
                .filter(|p| !p.is_empty()),
            // The folder's free; its project ID isn't.
            copy: key.starts_with("project:"),
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
        claims.at_work.clear();
        for (key, file) in std::mem::take(&mut claims.held) {
            self.let_go(&key, file);
        }
    }

    /// Who holds each of this process's claims, for another to read.
    fn write_holders(&self, claims: &Claims) {
        let holder = |path: &str, at_work: bool| Holder {
            path: path.to_string(),
            pid: std::process::id(),
            port: self.port,
            token: self.token.clone(),
            at_work,
        };
        let open = claims.open.iter().map(|k| (k, holder(&claims.path, false)));
        let left = claims
            .at_work
            .iter()
            .flat_map(|l| l.keys.iter().map(move |k| (k, holder(&l.path, true))));
        for (key, holder) in open.chain(left) {
            let info = self.files(key).1;
            let json = serde_json::to_vec(&holder).unwrap_or_default();
            if let Err(e) = rovik_core::atomic_file::write_atomic_private(&info, &json) {
                log::warn!("instances: {}: {e}", info.display());
            }
        }
    }

    /// The lock for `key`, if no other process holds it (None).
    fn lock(&self, key: &str) -> std::io::Result<Option<File>> {
        rovik_core::os_lock::try_hold(&self.files(key).0)
    }

    fn held_elsewhere(&self, key: &str) -> bool {
        rovik_core::os_lock::held_elsewhere(&self.files(key).0)
    }

    /// Its `.json` first, while still locked: once the lock goes, another
    /// process may write its own.
    fn let_go(&self, key: &str, lock: File) {
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

/// What opening `project` claims: its folder, and its project ID if it has
/// one (the folders with the same ID share their chats); with none, the
/// chats without a project.
fn claim_keys(project: Option<&Path>) -> Vec<String> {
    let Some(project) = project else {
        return vec![NO_PROJECT.to_string()];
    };
    let mut keys = vec![format!("folder:{}", path_key(project))];
    if let Some(id) = folder_project_id(project) {
        keys.push(format!("project:{id}"));
    }
    keys
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
            copy: false,
            at_work: false,
            focused,
        }
    }

    /// Opens a folder, as a window does: its chats aren't at work.
    fn open(instance: &Instances, path: &Path) -> Claim {
        instance.claim(Some(path), true, false)
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
        assert_eq!(b.claim(Some(&app), false, false), elsewhere(&app, false));
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
        a.opened(Some("project-one".into()));
        // A opens `two` while one's chats are at work there.
        assert_eq!(a.claim(Some(&two), true, true), Claim::claimed());
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
        a.opened(Some("project-one".into()));
        assert_eq!(a.claim(Some(&two), true, true), Claim::claimed());
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

        assert_eq!(a.claim(None, false, false), Claim::claimed());
        assert_eq!(a.open_project(), Some(None));
        assert_eq!(
            b.claim(None, false, false),
            Claim {
                open_as: None,
                ..elsewhere(&app, false)
            }
        );
        // A opens a folder: they're free.
        assert_eq!(open(&a, &app), Claim::claimed());
        assert_eq!(b.claim(None, false, false), Claim::claimed());
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
        assert_eq!(b.check(Some(&one), true), elsewhere(&one, true));
        assert_eq!(a_focused.load(Ordering::SeqCst), 1);
        // A checks a free folder: nothing's claimed, and A keeps `one`.
        let three = folder(&dir, "three", None);
        assert_eq!(a.check(Some(&three), true), Claim::default());
        assert_eq!(b.claim(Some(&one), false, false), elsewhere(&one, false));
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
