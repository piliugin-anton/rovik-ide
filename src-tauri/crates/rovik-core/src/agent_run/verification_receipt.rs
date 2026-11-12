//! Run-local evidence for explicitly reusable project checks. No filesystem
//! input is ignored: uncertainty makes the check run again. Unix change
//! timestamps cover same-size edits even when modification times are restored.

#[cfg(not(unix))]
use crate::restricted_files::Op;
use crate::restricted_files::RestrictedFiles;
use sha2::{Digest, Sha256};
#[cfg(not(unix))]
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A bounded input snapshot, never the file contents or environment values.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Stamp([u8; 32]);

/// Read every entry, including ignored dependencies and repository metadata.
/// External symlinks and incomplete walks disable reuse. On non-Unix hosts,
/// restricted files also disable reuse because the fallback reads contents.
pub(super) fn capture(
    roots: &[PathBuf],
    restricted: &RestrictedFiles,
    configuration: &[u8],
) -> Option<Stamp> {
    #[cfg(unix)]
    let _ = restricted; // Metadata only: never open restricted contents, even to classify them.
    let deadline = Instant::now() + Duration::from_secs(2);
    #[cfg(not(unix))]
    let mut budget = 64 * 1024 * 1024usize;
    let mut entries = 0usize;
    let mut digest = Sha256::new();
    digest.update(configuration);
    let mut environment: Vec<_> = std::env::vars_os().collect();
    environment.sort();
    for (key, value) in environment {
        field(&mut digest, key.as_encoded_bytes());
        field(&mut digest, value.as_encoded_bytes());
    }
    let mut roots: Vec<_> = roots
        .iter()
        .map(|r| r.canonicalize())
        .collect::<Result<_, _>>()
        .ok()?;
    roots.sort();
    roots.dedup();
    for root in roots {
        field(&mut digest, root.as_os_str().as_encoded_bytes());
        for entry in walkdir::WalkDir::new(&root).sort_by_file_name() {
            let entry = entry.ok()?;
            entries += 1;
            if entries > 200_000 || Instant::now() > deadline {
                return None;
            }
            let path = entry.path();
            let rel = path.strip_prefix(&root).ok()?;
            #[cfg(not(unix))]
            if restricted.blocks_file(&rel.to_string_lossy().replace('\\', "/"), path, Op::Read) {
                return None;
            }
            field(&mut digest, rel.as_os_str().as_encoded_bytes());
            let meta = std::fs::symlink_metadata(path).ok()?;
            let identity = metadata_identity(&meta)?;
            field(&mut digest, &identity);
            if meta.file_type().is_symlink() {
                let target = path.canonicalize().ok()?;
                if !target.starts_with(&root) {
                    return None;
                }
                // The physical target is visited elsewhere by this complete walk.
                field(&mut digest, b"link");
                field(&mut digest, target.as_os_str().as_encoded_bytes());
            } else if meta.is_dir() {
                field(&mut digest, b"directory");
            } else if meta.is_file() {
                field(&mut digest, b"file");
                // Unix ctime cannot be restored by a normal file writer, unlike
                // mtime. Metadata covers dependency/build trees without reading
                // gigabytes on every check. Other platforms hash file contents.
                #[cfg(not(unix))]
                {
                    if meta.len() > budget as u64 {
                        return None;
                    }
                    let mut file = std::fs::File::open(path).ok()?;
                    if metadata_identity(&file.metadata().ok()?)? != identity {
                        return None;
                    }
                    let mut bytes = [0u8; 32 * 1024];
                    let mut size = 0u64;
                    loop {
                        if Instant::now() > deadline {
                            return None;
                        }
                        let n = file.read(&mut bytes).ok()?;
                        if n == 0 {
                            break;
                        }
                        budget = budget.checked_sub(n)?;
                        size += n as u64;
                        digest.update(&bytes[..n]);
                    }
                    if size != meta.len()
                        || metadata_identity(&file.metadata().ok()?)? != identity
                        || metadata_identity(&std::fs::symlink_metadata(path).ok()?)? != identity
                    {
                        return None;
                    }
                    digest.update(size.to_le_bytes());
                }
            } else {
                return None;
            }
        }
    }
    Some(Stamp(digest.finalize().into()))
}

// Includes timestamps and file identity as well as content, so a concurrent
// same-length edit or replacement cannot silently become a reusable snapshot.
fn metadata_identity(meta: &std::fs::Metadata) -> Option<Vec<u8>> {
    #[allow(unused_mut)]
    let mut bytes = format!(
        "{:?}:{}:{:?}",
        meta.modified().ok()?,
        meta.len(),
        meta.permissions()
    )
    .into_bytes();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        bytes.extend_from_slice(
            format!(
                ":{}:{}:{}:{}:{}:{}",
                meta.dev(),
                meta.ino(),
                meta.ctime(),
                meta.ctime_nsec(),
                meta.uid(),
                meta.gid()
            )
            .as_bytes(),
        );
    }
    Some(bytes)
}

fn field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stamp(root: &std::path::Path) -> Option<Stamp> {
        capture(
            &[root.to_path_buf()],
            &RestrictedFiles::default(),
            b"checks",
        )
    }
    #[test]
    fn same_size_edits_and_ignored_files_invalidate_evidence() {
        let _environment = crate::toolchains::test_lock();
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".gitignore"), "ignored\n").unwrap();
        std::fs::write(temp.path().join("ignored"), "old").unwrap();
        let first = stamp(temp.path()).unwrap();
        assert!(Some(first.clone()) == stamp(temp.path()));
        std::fs::write(temp.path().join("ignored"), "new").unwrap();
        assert!(Some(first) != stamp(temp.path()));
    }
    #[cfg(unix)]
    #[test]
    fn restoring_modification_time_cannot_hide_a_same_size_edit() {
        let _environment = crate::toolchains::test_lock();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("input");
        std::fs::write(&path, "old").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let first = stamp(temp.path()).unwrap();
        std::fs::write(&path, "new").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(Some(first) != stamp(temp.path()));
    }
    #[cfg(not(unix))]
    #[test]
    fn restricted_inputs_disable_reuse_without_reading_them() {
        let _environment = crate::toolchains::test_lock();
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".env"), "secret").unwrap();
        assert!(stamp(temp.path()).is_none());
    }
    #[cfg(unix)]
    #[test]
    fn restricted_inputs_are_tracked_by_metadata_without_reading_contents() {
        let _environment = crate::toolchains::test_lock();
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".env");
        std::fs::write(&path, "secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
        let first = stamp(temp.path()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Some(first) != stamp(temp.path()));
    }
    #[cfg(unix)]
    #[test]
    fn external_links_disable_reuse_but_internal_links_are_covered() {
        let _environment = crate::toolchains::test_lock();
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("input"), "a").unwrap();
        std::os::unix::fs::symlink(temp.path().join("input"), temp.path().join("link")).unwrap();
        assert!(stamp(temp.path()).is_some());
        std::os::unix::fs::symlink(outside.path(), temp.path().join("outside")).unwrap();
        assert!(stamp(temp.path()).is_none());
    }
}
