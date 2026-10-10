//! Crash-safe file writes.
//!
//! `fs::write` truncates the target and then writes into it, so a crash, power
//! loss or full disk mid-write leaves a truncated file behind. Here the data
//! goes to a temporary file in the same directory, is flushed to disk, and
//! then renamed over the target. The rename is atomic on the same filesystem,
//! so readers only ever see the complete old file or the complete new one.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    write(path, contents, false)
}

/// `write_atomic` for secrets (API keys): on Unix only the user can read
/// the file, from the moment it exists.
pub fn write_atomic_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    write(path, contents, true)
}

fn write(path: &Path, contents: &[u8], private: bool) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    // Unique per process and call, so concurrent writers never share a temp file.
    let tmp = dir.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        unique_suffix()
    ));

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut file = options.open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?; // data on disk before the rename makes it visible
        drop(file);
        fs::rename(&tmp, path)?;
        sync_dir(dir); // persist the rename itself
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Best effort: on Unix a rename is only durable once the directory entry is
/// flushed. Windows has no equivalent and doesn't need one for this.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_files_are_only_readable_by_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("atomic-file-private-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let target = dir.join("keys.json");
        write_atomic_private(&target, b"secret").unwrap();
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replaces_contents_and_leaves_no_temp_files() {
        let dir = std::env::temp_dir().join(format!("atomic-file-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let target = dir.join("data.json");

        write_atomic(&target, b"first").unwrap();
        write_atomic(&target, b"second").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "second");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }
}
