//! Durable content-addressed bytes outside all worker mounts.

use crate::{
    identity::{ArtifactRef, Digest},
    require, Result,
};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// Registered blob formats. New kinds require a controller release.
pub const KINDS: &[&str] = &[
    "contract",
    "design",
    "policy",
    "profile",
    "plan",
    "environment",
    "snapshot",
    "source",
    "patch",
    "oracle",
    "event",
    "result",
    "receipt",
    "approval",
    "revocation",
    "operation",
    "log",
    "report",
    "context",
    "provider-response",
    "evaluation",
    "graph",
    "exception",
    "interop",
];

/// Controller-owned store. A worker must never receive this root as a mount.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    limit: u64,
}

/// Create a private directory and reject symlinks in existing path components.
pub(crate) fn private_directory(path: &Path) -> Result<()> {
    require(
        path.is_absolute(),
        "controller storage needs an absolute path",
    )?;
    let mut current = PathBuf::new();
    for part in path.components() {
        require(
            !matches!(part, std::path::Component::ParentDir),
            "controller storage contains traversal",
        )?;
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) => require(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "controller storage contains a symlink or non-directory",
            )?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder.create(&current)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        require(
            fs::metadata(path)?.permissions().mode() & 0o077 == 0,
            "controller directory must be private (mode 0700)",
        )?;
    }
    Ok(())
}

impl ArtifactStore {
    /// Open private storage with an explicit per-blob bound.
    pub fn open(root: &Path, maximum_bytes: u64) -> Result<Self> {
        require(maximum_bytes > 0, "artifact limit must be positive")?;
        private_directory(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            limit: maximum_bytes,
        })
    }
    fn path(&self, digest: &Digest) -> PathBuf {
        self.root.join(digest.hex())
    }
    /// Publish bytes atomically, synchronizing the file before any ledger reference.
    pub fn put(&self, kind: &str, media_type: &str, bytes: &[u8]) -> Result<ArtifactRef> {
        require(KINDS.contains(&kind), "unregistered artifact kind")?;
        require(
            ["application/json", "text/plain", "application/octet-stream"].contains(&media_type),
            "unsupported artifact media type",
        )?;
        require(
            bytes.len() as u64 <= self.limit,
            "artifact exceeds size limit",
        )?;
        if media_type == "application/json" {
            let _: serde_json::Value = crate::wire::decode(bytes)?;
        }
        let reference = ArtifactRef {
            digest: Digest::of(bytes),
            byte_count: bytes.len() as u64,
            kind: kind.into(),
            media_type: media_type.into(),
        };
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(self.path(&reference.digest)) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                require(
                    self.read(&reference)? == bytes,
                    "artifact identity collision",
                )?;
            }
            Err(e) => return Err(e.error.into()),
        }
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(reference)
    }
    /// Persist canonical metadata before it is referenced in a transaction.
    pub fn put_json(&self, kind: &str, value: &impl serde::Serialize) -> Result<ArtifactRef> {
        self.put(kind, "application/json", &crate::wire::canonical(value)?)
    }
    /// Verify length and digest on every read, including existing publications.
    pub fn read(&self, reference: &ArtifactRef) -> Result<Vec<u8>> {
        require(
            reference.byte_count <= self.limit,
            "artifact exceeds read limit",
        )?;
        let path = self.path(&reference.digest);
        let metadata = fs::symlink_metadata(&path)?;
        require(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "artifact is not a regular file",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            require(metadata.nlink() == 1, "artifact has external hard links")?;
        }
        require(
            metadata.len() == reference.byte_count,
            "artifact length mismatch",
        )?;
        let mut bytes = Vec::new();
        File::open(path)?
            .take(self.limit.saturating_add(1))
            .read_to_end(&mut bytes)?;
        require(
            bytes.len() as u64 == reference.byte_count && Digest::of(&bytes) == reference.digest,
            "artifact corruption detected",
        )?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_publication_is_idempotent_and_corruption_never_overwrites_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("blobs"), 100).unwrap();
        let reference = store
            .put("source", "text/plain", b"exact\r\nbytes")
            .unwrap();
        assert_eq!(
            store
                .put("source", "text/plain", b"exact\r\nbytes")
                .unwrap(),
            reference
        );
        fs::write(store.path(&reference.digest), b"corrupt").unwrap();
        assert!(store.read(&reference).is_err());
        assert!(store
            .put("source", "text/plain", b"exact\r\nbytes")
            .is_err());
        assert!(store.put("source", "text/plain", &[0; 101]).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn storage_and_artifact_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        symlink(dir.path(), dir.path().join("link")).unwrap();
        assert!(ArtifactStore::open(&dir.path().join("link/blobs"), 100).is_err());
        let store = ArtifactStore::open(&dir.path().join("blobs"), 100).unwrap();
        let r = store.put("source", "text/plain", b"bytes").unwrap();
        fs::remove_file(store.path(&r.digest)).unwrap();
        symlink(dir.path().join("outside"), store.path(&r.digest)).unwrap();
        assert!(store.read(&r).is_err());
    }
}
