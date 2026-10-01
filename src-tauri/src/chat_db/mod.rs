//! Chat storage in SQLite (`<app data>/chats.db`).
//!
//! - One writer thread owns the only read-write connection and runs jobs
//!   from a channel, one transaction each: writes are ordered and never
//!   contend with each other.
//! - Two read-only connections serve reads from the blocking pool; WAL lets
//!   them read while the writer commits.
//! - Screenshots and other binary data live in `attachments/`, addressed by
//!   their SHA-256, and are served to the webview by a URI scheme.

pub mod attachments;
pub mod commands;
pub mod history;
mod schema;
pub mod search;
pub mod store;
#[cfg(test)]
mod tests;

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

const DB_FILE: &str = "chats.db";
/// Present while the app runs; found at startup = the last run didn't exit cleanly.
const RUNNING_MARKER: &str = "chats.db.running";
const BACKUP_DIR: &str = "backups";
const BACKUPS_KEPT: usize = 3;
const BACKUP_EVERY_MS: i64 = 24 * 60 * 60 * 1000;
const READERS: usize = 2;

struct Inner {
    writer: Mutex<mpsc::Sender<Job>>,
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
    data_dir: PathBuf,
}

#[derive(Clone)]
pub struct DbState(Arc<Inner>);

impl DbState {
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir).map_err(store::err)?;
        std::fs::create_dir_all(data_dir.join(attachments::DIR)).map_err(store::err)?;
        let path = data_dir.join(DB_FILE);
        let marker = data_dir.join(RUNNING_MARKER);
        let unclean = marker.exists();

        let mut writer = Connection::open(&path).map_err(store::err)?;
        schema::configure(&writer, true).map_err(store::err)?;
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

        let _ = std::fs::write(&marker, b"");

        let (tx, rx) = mpsc::channel::<Job>();
        let dir = data_dir.to_path_buf();
        std::thread::Builder::new()
            .name("chat-db-writer".into())
            .spawn(move || {
                maintenance(&mut writer, &dir, unclean);
                for job in rx {
                    job(&mut writer);
                }
            })
            .map_err(store::err)?;

        Ok(DbState(Arc::new(Inner {
            writer: Mutex::new(tx),
            readers,
            next_reader: AtomicUsize::new(0),
            data_dir: data_dir.to_path_buf(),
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
            let _ = tx.send(f(conn));
        });
        self.0
            .writer
            .lock()
            .map_err(store::err)?
            .send(job)
            .map_err(|_| "Chat database writer has stopped".to_string())?;
        rx.await.map_err(|_| "Chat database writer dropped the request".to_string())?
    }

    /// Runs `f` on a read-only connection in the blocking pool.
    pub async fn read<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, String> + Send + 'static,
    {
        let inner = self.0.clone();
        tauri::async_runtime::spawn_blocking(move || {
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

    /// Clean exit: the next start skips the integrity check.
    pub fn mark_clean_exit(&self) {
        let _ = std::fs::remove_file(self.0.data_dir.join(RUNNING_MARKER));
    }
}

/// Startup housekeeping on the writer thread, before any job runs.
fn maintenance(conn: &mut Connection, data_dir: &Path, unclean: bool) {
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
            let stamp = name.strip_prefix("chats-")?.strip_suffix(".db")?.parse().ok()?;
            Some((stamp, e.path()))
        })
        .collect();
    backups.sort_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));

    let now = store::now_ms();
    if backups.first().map_or(true, |(stamp, _)| now - stamp >= BACKUP_EVERY_MS) {
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
