//! Chat storage in SQLite (`<app data>/chats.db`).
//!
//! - One writer thread owns the process's only read-write connection and
//!   runs jobs from a channel, one transaction each: its writes are ordered.
//!   Another of Rovik's processes may write too (a window each): every write
//!   transaction takes the database's write lock as it begins, waiting for
//!   the other's to commit, so none fails halfway.
//! - Two read-only connections serve reads from the blocking pool; WAL lets
//!   them read while the writer commits.
//! - Screenshots and other binary data live in `attachments/`, addressed by
//!   their SHA-256, and are served to the webview by a URI scheme.

pub mod api;
pub mod attachments;
pub mod history;
pub mod images;
pub mod location;
pub mod mirror;
pub mod moves;
pub mod projects;
#[cfg(test)]
mod rev_tests;
mod schema;
pub mod search;
pub mod store;
pub mod sync;
#[cfg(test)]
mod tests;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

const DB_FILE: &str = "chats.db";
/// Held while the database is open (`Presence`); "running" in it at a start
/// with no other process = the last run didn't exit cleanly.
const RUNNING_MARKER: &str = "chats.db.running";
const BACKUP_DIR: &str = "backups";
const BACKUPS_KEPT: usize = 3;
const BACKUP_EVERY_MS: i64 = 24 * 60 * 60 * 1000;
const READERS: usize = 2;

struct Inner {
    writer: Mutex<mpsc::Sender<Job>>,
    /// Joined by `close`, which only the headless runner calls.
    writer_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
    data_dir: PathBuf,
    presence: Mutex<Presence>,
}

#[derive(Clone)]
pub struct DbState(Arc<Inner>);

impl DbState {
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir).map_err(store::err)?;
        std::fs::create_dir_all(data_dir.join(attachments::DIR)).map_err(store::err)?;
        let path = data_dir.join(DB_FILE);
        // Migrated by one process at a time: the first to start, while the
        // others wait for it.
        let mut presence = Presence::start(&data_dir.join(RUNNING_MARKER));

        let mut writer = Connection::open(&path).map_err(store::err)?;
        schema::configure(&writer, true).map_err(store::err)?;
        writer.set_transaction_behavior(TransactionBehavior::Immediate);
        schema::migrate(&mut writer).map_err(store::err)?;

        let readers = (0..READERS)
            .map(|_| {
                let conn = Connection::open_with_flags(
                    &path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                schema::configure(&conn, false)?;
                Ok(Mutex::new(conn))
            })
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(store::err)?;

        presence.settle();
        let (unclean, alone) = (presence.unclean, presence.alone);

        let (tx, rx) = mpsc::channel::<Job>();
        let dir = data_dir.to_path_buf();
        let writer_thread = std::thread::Builder::new()
            .name("chat-db-writer".into())
            .spawn(move || {
                maintenance(&mut writer, &dir, unclean, alone);
                for job in rx {
                    job(&mut writer);
                }
            })
            .map_err(store::err)?;

        Ok(DbState(Arc::new(Inner {
            writer: Mutex::new(tx),
            writer_thread: Mutex::new(Some(writer_thread)),
            readers,
            next_reader: AtomicUsize::new(0),
            data_dir: data_dir.to_path_buf(),
            presence: Mutex::new(presence),
        })))
    }

    /// Runs `f` on the writer thread and returns its result.
    pub async fn write<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let done = match newer_schema(conn) {
                Some(why) => Err(why),
                None => f(conn),
            };
            let _ = tx.send(done);
        });
        self.0
            .writer
            .lock()
            .map_err(store::err)?
            .send(job)
            .map_err(|_| "Chat database writer has stopped".to_string())?;
        rx.await
            .map_err(|_| "Chat database writer dropped the request".to_string())?
    }

    /// Queues `f` on the writer thread without waiting for it: ordered with
    /// every write queued after it. Errors are logged.
    pub fn write_detached<F>(&self, f: F)
    where
        F: FnOnce(&mut Connection) -> Result<(), String> + Send + 'static,
    {
        let job: Job = Box::new(move |conn| {
            let done = match newer_schema(conn) {
                Some(why) => Err(why),
                None => f(conn),
            };
            if let Err(e) = done {
                eprintln!("chat_db: {e}");
            }
        });
        if let Ok(writer) = self.0.writer.lock() {
            let _ = writer.send(job);
        }
    }

    /// Runs `f` on a read-only connection in the blocking pool.
    pub async fn read<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, String> + Send + 'static,
    {
        let inner = self.0.clone();
        crate::rt::spawn_blocking(move || {
            let i = inner.next_reader.fetch_add(1, Ordering::Relaxed) % inner.readers.len();
            let conn = inner.readers[i].lock().map_err(store::err)?;
            f(&conn)
        })
        .await
        .map_err(store::err)?
    }

    pub fn data_dir(&self) -> &Path {
        &self.0.data_dir
    }

    /// Clean exit: the next start skips the integrity check (once no other
    /// process has the database open either).
    pub fn mark_clean_exit(&self) {
        self.0
            .presence
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .leave();
    }

    /// How many handles share this database (one: this is the last). The
    /// headless runner's, before it closes the database.
    pub fn handles(&self) -> usize {
        Arc::strong_count(&self.0)
    }

    /// Closes every connection, the writer's once its queued jobs are done.
    /// Only the last handle can: another one would keep its connections
    /// open (and a folder removed next would still hold open files). The
    /// headless runner's; the app keeps its database open until it exits.
    pub fn close(self) -> Result<(), String> {
        let inner =
            Arc::try_unwrap(self.0).map_err(|_| "the chat database is still in use".to_string())?;
        let Inner {
            writer,
            writer_thread,
            readers,
            ..
        } = inner;
        drop(writer);
        drop(readers);
        if let Some(thread) = writer_thread.into_inner().ok().flatten() {
            thread
                .join()
                .map_err(|_| "the chat database writer panicked".to_string())?;
        }
        Ok(())
    }
}

/// Why this process no longer writes: a newer Rovik's window updated the
/// database since this one opened it (an update installed while it ran).
/// Its schema has what this build doesn't know (a chat's location, say),
/// which a write could go around, as `schema::migrate` says at the start.
/// Reads go on.
fn newer_schema(conn: &Connection) -> Option<String> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .ok()?;
    (version > schema::VERSION).then(|| {
        format!(
            "A newer version of Rovik updated the chat database (schema {version}; this window \
             reads {}): close this window and open Rovik again.",
            schema::VERSION
        )
    })
}

/// That the database is open, kept in `RUNNING_MARKER` by each of Rovik's
/// processes that has it open, as a shared lock. A process that starts
/// with the lock to itself is the only one: it migrates the database while
/// the others wait, and reads whether the last one left it "clean". The
/// last to close writes "clean". The file is never removed, so a lock is
/// never left on a file another process can't find.
struct Presence {
    file: Option<File>,
    /// No other process had the database open at the start.
    alone: bool,
    /// The last run didn't exit cleanly.
    unclean: bool,
}

const CLEAN: &[u8] = b"clean";
const RUNNING: &[u8] = b"running";

impl Presence {
    /// The marker, to this process alone or shared once others have
    /// migrated the database.
    fn start(marker: &Path) -> Presence {
        let existed = marker.exists();
        let opened = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(marker);
        let Ok(mut file) = opened else {
            return Presence {
                file: None,
                alone: true,
                unclean: existed,
            };
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let _ = file.lock_shared();
                return Presence {
                    file: Some(file),
                    alone: false,
                    unclean: false,
                };
            }
            Err(TryLockError::Error(_)) => {
                return Presence {
                    file: None,
                    alone: true,
                    unclean: existed,
                }
            }
        }
        let mut was = Vec::new();
        let _ = file.read_to_end(&mut was);
        // An empty one is an older Rovik's, which removed it at a clean exit.
        let unclean = existed && was.as_slice() != CLEAN;
        Presence {
            file: Some(file),
            alone: true,
            unclean,
        }
    }

    /// Migrated: "running", and the lock shared with the processes after.
    fn settle(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if self.alone {
            write_marker(file, RUNNING);
            // On Unix the lock turns shared in place (flock changes it in
            // the kernel's one step): let go of first, another process
            // starting that moment could take it whole and think itself
            // alone. Windows takes no second lock on a handle that holds
            // one.
            if cfg!(windows) {
                let _ = file.unlock();
            }
            let _ = file.lock_shared();
        }
    }

    /// The last to leave says it left cleanly. Let go of at once: a
    /// process starting meanwhile doesn't wait for this one to end, and
    /// Windows lets no one else read a file while it's locked.
    fn leave(&mut self) {
        let Some(mut file) = self.file.take() else {
            return;
        };
        // On Unix the shared lock turns whole in place (flock's one step),
        // when no one else shares it: let go of first, a process starting
        // that moment could take it and find it marked as left cleanly
        // while it runs.
        if cfg!(windows) {
            let _ = file.unlock();
        }
        if file.try_lock().is_ok() {
            write_marker(&mut file, CLEAN);
        }
        let _ = file.unlock();
    }
}

fn write_marker(file: &mut File, content: &[u8]) {
    let _ = file.set_len(0);
    let _ = file.rewind();
    let _ = file.write_all(content);
    let _ = file.sync_all();
}

/// Startup housekeeping on the writer thread, before any job runs. With
/// another of Rovik's processes running, attachments only it refers to (an
/// image in its unsent input) aren't anyone's to remove.
fn maintenance(conn: &mut Connection, data_dir: &Path, unclean: bool, alone: bool) {
    if unclean {
        match conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)) {
            Ok(result) if result == "ok" => {}
            Ok(problem) => eprintln!("chat_db: quick_check after unclean exit reported: {problem}"),
            Err(e) => eprintln!("chat_db: quick_check failed: {e}"),
        }
    }
    if let Err(e) = backup_if_due(conn, data_dir) {
        eprintln!("chat_db: backup failed: {e}");
    }
    if !alone {
        return;
    }
    if let Err(e) = attachments::collect_garbage(conn, data_dir) {
        eprintln!("chat_db: attachment cleanup failed: {e}");
    }
}

/// `VACUUM INTO backups/chats-<ms>.db` at most once a day; keeps the newest 3.
fn backup_if_due(conn: &Connection, data_dir: &Path) -> Result<(), String> {
    let dir = data_dir.join(BACKUP_DIR);
    std::fs::create_dir_all(&dir).map_err(store::err)?;

    let mut backups: Vec<(i64, PathBuf)> = std::fs::read_dir(&dir)
        .map_err(store::err)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stamp = name
                .strip_prefix("chats-")?
                .strip_suffix(".db")?
                .parse()
                .ok()?;
            Some((stamp, e.path()))
        })
        .collect();
    backups.sort_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));

    let now = store::now_ms();
    if backups
        .first()
        .is_none_or(|(stamp, _)| now - stamp >= BACKUP_EVERY_MS)
    {
        let target = dir.join(format!("chats-{now}.db"));
        conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
            .map_err(store::err)?;
        backups.insert(0, (now, target));
    }
    for (_, old) in backups.into_iter().skip(BACKUPS_KEPT) {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}
