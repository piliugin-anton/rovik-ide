//! Bounded source streams and local Git LFS resolution. Never run clean/smudge filters.

use rovik_harness::{
    artifacts::{MAX_BUFFER_BYTES, MAX_SOURCE_BYTES},
    identity::{ArtifactRef, Digest},
    ledger::Ledger,
    runner, Error, Result,
};
use std::{fs, io::Read, path::Path};

fn size_check(path: &str, size: u64) -> Result<()> {
    if size > MAX_SOURCE_BYTES {
        return Err(Error::new(format!(
            "Source file {path:?} is {size} bytes; the snapshot limit is {MAX_SOURCE_BYTES} bytes per file."
        )));
    }
    Ok(())
}

/// Observe every byte of a stable regular file without publishing it.
pub(super) fn observe(file: &Path, path: &str) -> Result<ArtifactRef> {
    let before = fs::symlink_metadata(file)?;
    size_check(path, before.len())?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(Error::new(format!(
            "Source file {path:?} must be a regular file."
        )));
    }
    let (digest, byte_count) =
        Digest::copy(fs::File::open(file)?, std::io::sink(), MAX_SOURCE_BYTES).map_err(
            |error| Error::new(format!("Capture source file {path:?}: {}", error.message)),
        )?;
    let after = fs::symlink_metadata(file)?;
    if !after.is_file()
        || after.file_type().is_symlink()
        || byte_count != before.len()
        || before.len() != after.len()
        || before.modified()? != after.modified()?
        || before.permissions() != after.permissions()
    {
        return Err(Error::new(format!(
            "Source file {path:?} changed during capture. Inspect the project again."
        )));
    }
    Ok(ArtifactRef {
        digest,
        byte_count,
        kind: "source".into(),
        media_type: "application/octet-stream".into(),
    })
}

/// Store a previously observed file, rejecting changes between the two reads.
pub(super) fn publish(
    ledger: &Ledger,
    file: &Path,
    path: &str,
    expected: &ArtifactRef,
) -> Result<()> {
    let content = ledger
        .store_source(fs::File::open(file)?, runner::now_ms()?)
        .map_err(|error| Error::new(format!("Store source file {path:?}: {}", error.message)))?;
    if &content != expected {
        return Err(Error::new(format!(
            "Source file {path:?} changed during capture. Inspect the project again."
        )));
    }
    Ok(())
}

fn pointer(bytes: &[u8], path: &str) -> Result<Option<ArtifactRef>> {
    const VERSION: &str = "version https://git-lfs.github.com/spec/v1";
    if !bytes.starts_with(VERSION.as_bytes()) {
        return Ok(None);
    }
    let invalid = || {
        Error::new(format!("Invalid or unsupported Git LFS pointer at {path:?}; restore the asset with Git LFS before verification."))
    };
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut lines = text.lines();
    if lines.next() != Some(VERSION) {
        return Err(invalid());
    }
    let digest = lines
        .next()
        .and_then(|line| line.strip_prefix("oid "))
        .ok_or_else(invalid)
        .and_then(|value| Digest::parse(value).map_err(|_| invalid()))?;
    let byte_count = lines
        .next()
        .and_then(|line| line.strip_prefix("size "))
        .and_then(|size| size.parse::<u64>().ok())
        .ok_or_else(invalid)?;
    if lines.next().is_some() {
        return Err(invalid());
    }
    size_check(path, byte_count)?;
    Ok(Some(ArtifactRef {
        digest,
        byte_count,
        kind: "source".into(),
        media_type: "application/octet-stream".into(),
    }))
}

/// Require actual working-tree assets rather than unhydrated LFS pointers.
pub(super) fn require_hydrated(file: &Path, path: &str, size: u64) -> Result<()> {
    let mut bytes = Vec::new();
    if size <= 1024 {
        fs::File::open(file)?.take(1025).read_to_end(&mut bytes)?;
    }
    if size <= 1024 && pointer(&bytes, path)?.is_some() {
        return Err(Error::new(format!("Git LFS asset {path:?} is still a pointer. Run git lfs pull (or git lfs checkout if already downloaded), then verify again.")));
    }
    Ok(())
}

fn contained_file(root: &Path, file: &Path) -> Result<bool> {
    if !file.starts_with(root) {
        return Ok(false);
    }
    let mut current = root.to_path_buf();
    for part in file
        .strip_prefix(root)
        .map_err(|error| Error::new(error.to_string()))?
        .components()
    {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Ok(false);
        }
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(false),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(fs::metadata(file)?.is_file() && file.canonicalize()?.starts_with(root.canonicalize()?))
}

fn lfs_storage(repository: &git2::Repository, common: &Path) -> Result<std::path::PathBuf> {
    let configured = repository
        .config()
        .map_err(|error| Error::new(error.to_string()))?
        .get_string("lfs.storage");
    match configured {
        Ok(path) => {
            let path = std::path::PathBuf::from(path);
            Ok(if path.is_absolute() {
                path
            } else {
                common.join(path)
            })
        }
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(common.join("lfs")),
        Err(error) => Err(Error::new(format!(
            "Read Git LFS storage configuration: {error}"
        ))),
    }
}

fn lfs_asset(
    repository: &git2::Repository,
    ledger: &Ledger,
    path: &str,
    expected: ArtifactRef,
) -> Result<ArtifactRef> {
    // A verified CAS copy can also retain an older baseline after the LFS cache is pruned.
    if ledger.reference(&expected.digest).is_ok() {
        ledger.verify_reference(&expected)?;
        return Ok(expected);
    }
    let common = repository.commondir().canonicalize()?;
    let storage = lfs_storage(repository, &common)?;
    let hex = expected.digest.hex();
    let cached = storage
        .join("objects")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(hex);
    let mut candidates = vec![(common, cached)];
    if let Some(root) = repository.workdir() {
        candidates.push((root.to_path_buf(), root.join(path)));
    }
    for (root, file) in candidates {
        if !contained_file(&root, &file)? {
            continue;
        }
        let observed = observe(&file, path)?;
        if observed != expected {
            continue;
        }
        publish(ledger, &file, path, &expected)?;
        return Ok(expected);
    }
    Err(Error::new(format!("Git LFS asset {path:?} ({}, {} bytes) is missing or corrupt locally. Fetch that revision's LFS objects, then verify again; verification never downloads assets automatically.", expected.digest, expected.byte_count)))
}

/// Capture a pinned Git blob, resolving LFS content from local verified bytes.
pub(super) fn blob(
    repository: &git2::Repository,
    ledger: &Ledger,
    oid: git2::Oid,
    path: &str,
) -> Result<ArtifactRef> {
    let odb = repository
        .odb()
        .map_err(|error| Error::new(error.to_string()))?;
    let (size, kind) = odb
        .read_header(oid)
        .map_err(|error| Error::new(format!("Read source file {path:?}: {error}")))?;
    size_check(path, size as u64)?;
    if kind != git2::ObjectType::Blob {
        return Err(Error::new(format!(
            "Source file {path:?} is not a Git blob."
        )));
    }
    if size as u64 <= MAX_BUFFER_BYTES {
        let blob = repository
            .find_blob(oid)
            .map_err(|error| Error::new(error.to_string()))?;
        if size <= 1024 {
            if let Some(expected) = pointer(blob.content(), path)? {
                return lfs_asset(repository, ledger, path, expected);
            }
        }
        return ledger.store_source(blob.content(), runner::now_ms()?);
    }
    if let Ok((reader, _, _)) = odb.reader(oid) {
        let content = ledger.store_source(reader, runner::now_ms()?)?;
        if content.byte_count != size as u64 {
            return Err(Error::new(format!("Incomplete Git source file {path:?}.")));
        }
        return Ok(content);
    }
    // libgit2 cannot stream packed objects. Git's object reader handles packed/delta
    // storage without applying repository filters or modifying the checkout/index.
    let now = runner::now_ms()?;
    let mut child = std::process::Command::new("git")
        .arg("--no-replace-objects")
        .arg("--git-dir")
        .arg(repository.path())
        .args(["cat-file", "blob"])
        .arg(oid.to_string())
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| {
            Error::new(format!(
                "Read packed source file {path:?}: {error}. Install Git and retry."
            ))
        })?;
    let result = match child.stdout.take() {
        Some(stdout) => ledger.store_source(stdout.take(size as u64 + 1), now),
        None => Err(Error::new("Git source stream is unavailable".into())),
    };
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let content = result?;
    if !status.success() || content.byte_count != size as u64 {
        return Err(Error::new(format!("Git could not read the complete source file {path:?}; restore its local objects and retry.")));
    }
    Ok(content)
}

/// Preserve an existing LFS path as a pointer when constructing a delivery tree.
pub(super) fn delivery_blob(
    repository: &git2::Repository,
    ledger: &Ledger,
    revision: &str,
    path: &str,
    content: &ArtifactRef,
) -> Result<git2::Oid> {
    let baseline = match git2::Oid::from_str(revision) {
        Ok(oid) => {
            let commit = repository
                .find_commit(oid)
                .map_err(|error| Error::new(error.to_string()))?;
            let tree = commit
                .tree()
                .map_err(|error| Error::new(error.to_string()))?;
            match tree.get_path(Path::new(path)) {
                Ok(entry) => Some(entry),
                Err(error) if error.code() == git2::ErrorCode::NotFound => None,
                Err(error) => return Err(Error::new(error.to_string())),
            }
        }
        Err(_) => None,
    };
    let lfs = if let Some(entry) =
        baseline.filter(|entry| entry.kind() == Some(git2::ObjectType::Blob))
    {
        let odb = repository
            .odb()
            .map_err(|error| Error::new(error.to_string()))?;
        let (size, _) = odb
            .read_header(entry.id())
            .map_err(|error| Error::new(error.to_string()))?;
        if size <= 1024 {
            let blob = repository
                .find_blob(entry.id())
                .map_err(|error| Error::new(error.to_string()))?;
            pointer(blob.content(), path)?.is_some()
        } else {
            false
        }
    } else {
        false
    };
    if lfs {
        let common = repository.commondir().canonicalize()?;
        let storage = lfs_storage(repository, &common)?;
        let hex = content.digest.hex();
        let destination = storage
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..4])
            .join(hex);
        let relative = destination.strip_prefix(&common).map_err(|_| Error::new("Git delivery requires LFS storage inside the Git directory. Apply the verified working tree in the IDE and commit with Git LFS for external storage.".into()))?;
        let mut directory = common.clone();
        for part in relative
            .parent()
            .ok_or_else(|| Error::new("Invalid LFS object path".into()))?
            .components()
        {
            if !matches!(part, std::path::Component::Normal(_)) {
                return Err(Error::new(
                    "Git LFS storage cannot traverse outside the Git directory".into(),
                ));
            }
            directory.push(part);
            match fs::symlink_metadata(&directory) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(Error::new(
                        "Git LFS storage contains a symlink or non-directory".into(),
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&directory)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        ledger.copy_to(&content.digest, temporary.as_file_mut())?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if observe(&destination, path)? != *content {
                    return Err(Error::new(format!("Existing Git LFS object for {path:?} is corrupt; restore it before delivery.")));
                }
            }
            Err(error) => return Err(error.error.into()),
        }
        let text = format!(
            "version https://git-lfs.github.com/spec/v1\noid {}\nsize {}\n",
            content.digest, content.byte_count
        );
        return repository
            .blob(text.as_bytes())
            .map_err(|error| Error::new(error.to_string()));
    }
    let mut writer = repository
        .blob_writer(None)
        .map_err(|error| Error::new(error.to_string()))?;
    ledger.copy_to(&content.digest, &mut writer)?;
    writer
        .commit()
        .map_err(|error| Error::new(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_delivery_keeps_lfs_pointers_and_publishes_exact_updated_assets() {
        let directory = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(directory.path().join("repo")).unwrap();
        let ledger = Ledger::open(&directory.path().join("state")).unwrap();
        let original = b"original asset";
        let pointer = format!(
            "version https://git-lfs.github.com/spec/v1\noid {}\nsize {}\n",
            Digest::of(original),
            original.len()
        );
        let oid = repository.blob(pointer.as_bytes()).unwrap();
        let mut builder = repository.treebuilder(None).unwrap();
        builder.insert("image.png", oid, 0o100644).unwrap();
        let tree = repository.find_tree(builder.write().unwrap()).unwrap();
        let signature = git2::Signature::now("Asset tests", "tests@rovik.invalid").unwrap();
        let commit = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "LFS baseline",
                &tree,
                &[],
            )
            .unwrap();
        for bytes in [
            original.as_slice(),
            b"updated asset with different bytes".as_slice(),
        ] {
            let content = ledger.store_source(bytes, 1).unwrap();
            let result = delivery_blob(
                &repository,
                &ledger,
                &commit.to_string(),
                "image.png",
                &content,
            )
            .unwrap();
            let git_bytes = repository.find_blob(result).unwrap();
            assert!(git_bytes
                .content()
                .starts_with(b"version https://git-lfs.github.com/spec/v1\n"));
            let fresh = Ledger::open(
                &directory
                    .path()
                    .join(format!("fresh-{}", content.digest.hex())),
            )
            .unwrap();
            assert_eq!(
                blob(&repository, &fresh, result, "image.png").unwrap(),
                content,
                "delivery must leave the object available without the controller CAS"
            );
        }
    }

    #[test]
    #[ignore = "requires the real Git CLI; set ROVIK_REAL_GIT=1"]
    fn packed_git_assets_stream_without_changing_the_index_or_checkout() {
        if std::env::var("ROVIK_REAL_GIT").as_deref() != Ok("1") {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        let repository = git2::Repository::init(&root).unwrap();
        let ledger = Ledger::open(&directory.path().join("state")).unwrap();
        let bytes = vec![0xab; 70 * 1024 * 1024];
        let expected = Digest::of(&bytes);
        let oid = repository.blob(&bytes).unwrap();
        drop(bytes);
        let mut builder = repository.packbuilder().unwrap();
        builder.insert_object(oid, Some("image.png")).unwrap();
        builder
            .write(&repository.path().join("objects/pack"), 0)
            .unwrap();
        let hex = oid.to_string();
        fs::remove_file(
            repository
                .path()
                .join("objects")
                .join(&hex[..2])
                .join(&hex[2..]),
        )
        .unwrap();
        drop(builder);
        drop(repository);
        let repository = git2::Repository::open(&root).unwrap();
        assert!(repository.odb().unwrap().reader(oid).is_err());
        let content = blob(&repository, &ledger, oid, "image.png").unwrap();
        assert_eq!(content.digest, expected);
        assert_eq!(content.byte_count, 70 * 1024 * 1024);
        assert!(!root.join("image.png").exists());
        assert!(!repository.path().join("index").exists());
    }

    #[test]
    fn lfs_baselines_require_exact_local_bytes_and_never_accept_pointer_contents() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        let repository = git2::Repository::init(&root).unwrap();
        let ledger = Ledger::open(&directory.path().join("state")).unwrap();
        let bytes = b"\x89PNG\r\n\x1a\nasset";
        let digest = Digest::of(bytes);
        let text = format!(
            "version https://git-lfs.github.com/spec/v1\noid {digest}\nsize {}\n",
            bytes.len()
        );
        let oid = repository.blob(text.as_bytes()).unwrap();
        let path = "image.png";
        let mut builder = repository.treebuilder(None).unwrap();
        builder.insert(path, oid, 0o100644).unwrap();
        let tree = repository.find_tree(builder.write().unwrap()).unwrap();
        let signature = git2::Signature::now("Asset tests", "tests@rovik.invalid").unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "LFS baseline",
                &tree,
                &[],
            )
            .unwrap();
        fs::write(root.join(path), &text).unwrap();
        let error = super::super::GitIntegration::new("lfs".into(), &root)
            .unwrap()
            .snapshot(&ledger)
            .unwrap_err();
        assert!(
            error.message.contains(path)
                && error.message.contains("Fetch that revision's LFS objects"),
            "Git traversal must retain the actionable source error: {}",
            error.message
        );
        assert!(require_hydrated(&root.join(path), path, text.len() as u64)
            .unwrap_err()
            .message
            .contains("git lfs pull"));
        assert!(blob(&repository, &ledger, oid, path)
            .unwrap_err()
            .message
            .contains("missing or corrupt"));
        fs::write(root.join(path), b"wrong asset").unwrap();
        assert!(blob(&repository, &ledger, oid, path).is_err());
        let cached = repository
            .path()
            .join("lfs/objects")
            .join(&digest.hex()[..2])
            .join(&digest.hex()[2..4])
            .join(digest.hex());
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&cached, bytes).unwrap();
        let content = blob(&repository, &ledger, oid, path).unwrap();
        assert_eq!(content.digest, digest);
        assert_eq!(ledger.read(&content.digest).unwrap(), bytes);
        assert_eq!(
            fs::read(root.join(path)).unwrap(),
            b"wrong asset",
            "baseline capture never smudges the checkout"
        );
        fs::remove_file(cached).unwrap();
        assert_eq!(
            blob(&repository, &ledger, oid, path).unwrap(),
            content,
            "an exact stored baseline survives cache pruning"
        );
        let fresh = Ledger::open(&directory.path().join("fresh-state")).unwrap();
        fs::write(root.join(path), bytes).unwrap();
        assert_eq!(
            blob(&repository, &fresh, oid, path).unwrap(),
            content,
            "a matching checkout can supply a missing cache object"
        );
        let wrong_size = text.replace(&format!("size {}", bytes.len()), "size 1");
        let wrong_oid = repository.blob(wrong_size.as_bytes()).unwrap();
        assert!(blob(&repository, &fresh, wrong_oid, path).is_err());
    }

    #[test]
    fn source_limit_errors_identify_the_path_size_and_limit_before_reading() {
        let error = size_check("assets/oversized.png", MAX_SOURCE_BYTES + 1).unwrap_err();
        // Unix can create a sparse oversized file without reserving 4 GiB on CI.
        #[cfg(unix)]
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("oversized.png");
            fs::File::create(&path)
                .unwrap()
                .set_len(MAX_SOURCE_BYTES + 1)
                .unwrap();
            assert_eq!(
                observe(&path, "assets/oversized.png").unwrap_err().message,
                error.message
            );
        }
        assert!(error.message.contains("assets/oversized.png"));
        assert!(error.message.contains(&(MAX_SOURCE_BYTES + 1).to_string()));
        assert!(error.message.contains(&MAX_SOURCE_BYTES.to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn lfs_resolution_rejects_symlinked_assets_outside_the_repository() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        let repository = git2::Repository::init(&root).unwrap();
        let ledger = Ledger::open(&directory.path().join("state")).unwrap();
        let bytes = b"external asset";
        let external = directory.path().join("external");
        fs::write(&external, bytes).unwrap();
        std::os::unix::fs::symlink(external, root.join("image.png")).unwrap();
        let text = format!(
            "version https://git-lfs.github.com/spec/v1\noid {}\nsize {}\n",
            Digest::of(bytes),
            bytes.len()
        );
        let oid = repository.blob(text.as_bytes()).unwrap();
        assert!(blob(&repository, &ledger, oid, "image.png").is_err());
    }
}
