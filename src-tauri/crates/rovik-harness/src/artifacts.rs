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

/// Maximum source asset size, compatible with Git's index size field.
pub const MAX_SOURCE_BYTES: u64 = u32::MAX as u64;
/// In-memory control artifacts and text reads keep their independent bound.
pub const MAX_BUFFER_BYTES: u64 = 64 * 1024 * 1024;

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
    "baseline-plan",
    "baseline-report",
    "preparation",
    "role-draft",
    "merge-candidate",
    "delivery",
    "delivery-receipt",
];

/// Controller-owned store. A worker must never receive this root as a mount.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    limit: u64,
    source_limit: u64,
}

/// Create private storage, resolving only macOS's root-owned system aliases.
/// Return the checked path so later access does not traverse an alias again.
pub(crate) fn private_directory(path: &Path) -> Result<PathBuf> {
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
        // A Windows drive/UNC prefix is not a directory until RootDir is appended.
        // In particular, querying a verbatim drive prefix alone fails with ERROR_INVALID_FUNCTION.
        if matches!(part, std::path::Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                #[cfg(target_os = "macos")]
                if meta.file_type().is_symlink() {
                    use std::os::unix::fs::MetadataExt;
                    let target = match current.to_str() {
                        Some("/var") => Some(Path::new("/private/var")),
                        Some("/tmp") => Some(Path::new("/private/tmp")),
                        _ => None,
                    };
                    if let Some(target) = target {
                        require(
                            meta.uid() == 0
                                && Path::new("/").join(fs::read_link(&current)?) == target,
                            "controller storage system alias changed",
                        )?;
                        // Check the resolved ancestors too; arbitrary user symlinks
                        // remain forbidden, even when their target is private.
                        for ancestor in target.ancestors() {
                            let metadata = fs::symlink_metadata(ancestor)?;
                            require(
                                metadata.is_dir() && metadata.uid() == 0,
                                "controller storage system alias is not root-owned",
                            )?;
                        }
                        current = target.into();
                        continue;
                    }
                }
                require(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "controller storage contains a symlink or non-directory",
                )?;
            }
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
            fs::metadata(&current)?.permissions().mode() & 0o077 == 0,
            "controller directory must be private (mode 0700)",
        )?;
    }
    Ok(current)
}

impl ArtifactStore {
    /// Open private storage with an explicit per-blob bound.
    pub fn open(root: &Path, maximum_bytes: u64) -> Result<Self> {
        Self::with_source_limit(root, maximum_bytes, maximum_bytes)
    }
    pub(crate) fn with_source_limit(
        root: &Path,
        maximum_bytes: u64,
        source_limit: u64,
    ) -> Result<Self> {
        require(maximum_bytes > 0, "artifact limit must be positive")?;
        let root = private_directory(root)?;
        Ok(Self {
            root,
            limit: maximum_bytes,
            source_limit,
        })
    }
    fn path(&self, digest: &Digest) -> PathBuf {
        self.root.join(digest.hex())
    }
    fn content_limit(&self, kind: &str) -> u64 {
        if kind == "source" {
            self.source_limit
        } else {
            self.limit
        }
    }
    /// Stream a source asset into private durable storage, preserving its exact bytes.
    pub fn put_source(&self, reader: impl Read) -> Result<ArtifactRef> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        let (digest, byte_count) = Digest::copy(reader, &mut temporary, self.source_limit)?;
        let reference = ArtifactRef {
            digest,
            byte_count,
            kind: "source".into(),
            media_type: "application/octet-stream".into(),
        };
        self.publish(temporary, &reference)?;
        Ok(reference)
    }
    fn publish(&self, temporary: tempfile::NamedTempFile, reference: &ArtifactRef) -> Result<()> {
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(self.path(&reference.digest)) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.copy_to(reference, std::io::sink())?;
            }
            Err(e) => return Err(e.error.into()),
        }
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    /// Publish bytes atomically, synchronizing the file before any ledger reference.
    pub fn put(&self, kind: &str, media_type: &str, bytes: &[u8]) -> Result<ArtifactRef> {
        require(KINDS.contains(&kind), "unregistered artifact kind")?;
        require(
            ["application/json", "text/plain", "application/octet-stream"].contains(&media_type),
            "unsupported artifact media type",
        )?;
        require(
            bytes.len() as u64 <= self.content_limit(kind),
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
        self.publish(temporary, &reference)?;
        Ok(reference)
    }
    /// Persist canonical metadata before it is referenced in a transaction.
    pub fn put_json(&self, kind: &str, value: &impl serde::Serialize) -> Result<ArtifactRef> {
        self.put(kind, "application/json", &crate::wire::canonical(value)?)
    }
    /// Read bounded text/control data. Large source assets must use `copy_to`.
    pub fn read(&self, reference: &ArtifactRef) -> Result<Vec<u8>> {
        require(
            reference.byte_count <= self.limit,
            "artifact exceeds the in-memory read limit; use a streaming source operation",
        )?;
        let mut bytes = Vec::new();
        self.copy_to(reference, &mut bytes)?;
        Ok(bytes)
    }
    /// Copy exact bytes while checking length and digest, using bounded memory.
    /// The destination must remain private until this returns successfully.
    pub fn copy_to(&self, reference: &ArtifactRef, writer: impl Write) -> Result<()> {
        require(
            reference.byte_count <= self.content_limit(&reference.kind),
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
        let (digest, byte_count) = Digest::copy(File::open(path)?, writer, reference.byte_count)?;
        require(
            byte_count == reference.byte_count && digest == reference.digest,
            "artifact corruption detected",
        )?;
        Ok(())
    }
    /// Classify binary content without buffering a full asset; still verify every byte.
    pub fn is_binary(&self, reference: &ArtifactRef) -> Result<bool> {
        let mut classifier = TextClassifier::default();
        self.copy_to(reference, &mut classifier)?;
        Ok(classifier.binary || !classifier.pending.is_empty())
    }
}

#[derive(Default)]
struct TextClassifier {
    binary: bool,
    pending: Vec<u8>,
}
impl Write for TextClassifier {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if !self.binary {
            self.pending.extend_from_slice(bytes);
            self.binary = bytes.contains(&0);
            match std::str::from_utf8(&self.pending) {
                Ok(_) => self.pending.clear(),
                Err(error) if error.error_len().is_none() => {
                    self.pending.drain(..error.valid_up_to());
                }
                Err(_) => {
                    self.binary = true;
                    self.pending.clear();
                }
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_sources_exceed_text_limits_without_weakening_integrity_or_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::with_source_limit(&dir.path().join("blobs"), 16, 128).unwrap();
        let bytes = [0xfe; 100];
        let reference = store.put_source(bytes.as_slice()).unwrap();
        assert_eq!(reference.digest, Digest::of(&bytes));
        assert_eq!(reference.byte_count, 100);
        assert!(store.read(&reference).is_err());
        assert!(store.put("log", "text/plain", &bytes).is_err());
        let mut output = Vec::new();
        store.copy_to(&reference, &mut output).unwrap();
        assert_eq!(output, bytes);
        assert_eq!(store.put_source(bytes.as_slice()).unwrap(), reference);
        assert!(store.is_binary(&reference).unwrap());
        assert!(store.put_source([0u8; 129].as_slice()).is_err());
        assert_eq!(
            fs::read_dir(&store.root).unwrap().count(),
            1,
            "failed streams leave no temporary blob"
        );
        fs::write(store.path(&reference.digest), [0xff; 100]).unwrap();
        assert!(store.copy_to(&reference, std::io::sink()).is_err());
        assert!(
            store.put_source(bytes.as_slice()).is_err(),
            "a duplicate cannot overwrite a corrupt publication"
        );
    }

    #[test]
    fn binary_classification_handles_utf8_across_stream_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&dir.path().join("blobs"), 100_000).unwrap();
        let mut text = vec![b'a'; 65_535];
        text.extend_from_slice("🍎".as_bytes());
        let reference = store.put_source(text.as_slice()).unwrap();
        assert!(!store.is_binary(&reference).unwrap());
        text.pop();
        assert!(store
            .is_binary(&store.put_source(text.as_slice()).unwrap())
            .unwrap());
        assert!(store
            .is_binary(&store.put_source(b"a\0b".as_slice()).unwrap())
            .unwrap());
    }
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

    #[test]
    fn storage_reopens_through_native_and_canonical_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/blobs");
        let store = ArtifactStore::open(&path, 100).unwrap();
        let reference = store.put("source", "text/plain", b"same storage").unwrap();
        let canonical = ArtifactStore::open(&path.canonicalize().unwrap(), 100).unwrap();
        assert_eq!(canonical.read(&reference).unwrap(), b"same storage");
        assert!(ArtifactStore::open(&path.join("../other"), 100).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_temp_aliases_resolve_without_admitting_user_symlinks() {
        use std::os::unix::fs::symlink;
        for base in ["/var/tmp", "/tmp"] {
            let dir = tempfile::tempdir_in(base).unwrap();
            let store = ArtifactStore::open(&dir.path().join("blobs"), 100).unwrap();
            assert_eq!(store.root, dir.path().canonicalize().unwrap().join("blobs"));
            symlink(&store.root, dir.path().join("user-link")).unwrap();
            assert!(ArtifactStore::open(&dir.path().join("user-link"), 100).is_err());
        }
    }
}
