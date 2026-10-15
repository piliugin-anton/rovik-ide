//! The daemon's log, logs/agentd.log, rotated at 5 MB with three older
//! files kept (5.1). Requests are logged by kind, never by content, so no
//! token or key gets in (plan rule 9).

use log::{LevelFilter, Log, Metadata, Record};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_BYTES: u64 = 5 << 20;
const KEPT: u32 = 3;

/// Sends the `log` macros' lines to `dir`/agentd.log from now on.
pub fn init(dir: &Path, level: LevelFilter) -> Result<(), String> {
    let log = FileLog::open(dir.join("agentd.log"), MAX_BYTES)?;
    // Kept for as long as the process lives.
    log::set_logger(Box::leak(Box::new(log))).map_err(|e| e.to_string())?;
    log::set_max_level(level);
    Ok(())
}

struct FileLog {
    path: PathBuf,
    max: u64,
    file: Mutex<File>,
}

impl FileLog {
    fn open(path: PathBuf, max: u64) -> Result<FileLog, String> {
        let file = append(&path)?;
        Ok(FileLog {
            path,
            max,
            file: Mutex::new(file),
        })
    }

    fn write(&self, line: &str) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        if size > 0 && size + line.len() as u64 > self.max {
            rotate(&self.path);
            if let Ok(fresh) = append(&self.path) {
                *file = fresh;
            }
        }
        let _ = file.write_all(line.as_bytes());
    }
}

impl Log for FileLog {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.write(&format!(
                "{} {:<5} {}\n",
                rovik_core::agent_run::log::iso_now(),
                record.level(),
                record.args()
            ));
        }
    }

    fn flush(&self) {}
}

fn append(path: &Path) -> Result<File, String> {
    File::options()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// agentd.log becomes agentd.log.1, .1 becomes .2, and so on; the oldest goes.
fn rotate(path: &Path) {
    let numbered = |n: u32| PathBuf::from(format!("{}.{n}", path.display()));
    let _ = fs::remove_file(numbered(KEPT));
    for n in (1..KEPT).rev() {
        let _ = fs::rename(numbered(n), numbered(n + 1));
    }
    let _ = fs::rename(path, numbered(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_rotates_and_keeps_three() {
        let dir = rovik_core::testing::temp_dir();
        let log = FileLog::open(dir.0.join("agentd.log"), 100).unwrap();
        for n in 0..12 {
            log.write(&format!("{n:0>59}\n"));
        }
        // A line a file: the newest in the current one, older ones behind it.
        let read = |name: &str| fs::read_to_string(dir.0.join(name)).unwrap();
        for (name, n) in [
            ("agentd.log", 11),
            ("agentd.log.1", 10),
            ("agentd.log.2", 9),
            ("agentd.log.3", 8),
        ] {
            assert_eq!(read(name), format!("{n:0>59}\n"), "{name}");
        }
        assert!(!dir.0.join("agentd.log.4").exists(), "three kept");
    }
}
