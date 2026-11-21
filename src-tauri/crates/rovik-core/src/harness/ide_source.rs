//! Desktop-authenticated source bindings shared by verification and strict Autopilot.
//!
//! A shadow worktree's HEAD includes untracked files, so it cannot establish the
//! source baseline or tracked-file selection. Only a trusted synchronization
//! adapter may bind those inputs. Readers never manufacture or migrate bindings.

use git2::{Oid, Repository};
use rovik_harness::{identity::Digest, scope, wire, Error, Result};
use rovik_protocol::harness::SourceBinding;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
enum StoredBinding {
    Bound {
        root: PathBuf,
        source: SourceBinding,
    },
    Invalidated {
        previous: Option<BoundSource>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundSource {
    root: PathBuf,
    source: SourceBinding,
}

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::new(message.into()))
    }
}

fn paths(data_dir: &Path, dialog_id: &str) -> Result<(PathBuf, String)> {
    require(
        data_dir.is_absolute(),
        "Source bindings need an absolute private data directory.",
    )?;
    require(
        !dialog_id.is_empty() && dialog_id.len() <= 256,
        "Select a registered chat before binding its source.",
    )?;
    let digest = Digest::of(dialog_id.as_bytes());
    let key = digest.hex();
    Ok((
        data_dir.join("harness-sources").join(format!("{key}.json")),
        format!("refs/heads/rovik-harness-baseline-{key}"),
    ))
}

fn repository(root: &Path) -> Result<(PathBuf, Repository)> {
    let root = root.canonicalize()?;
    let repository = Repository::open(&root).map_err(|error| {
        Error::new(format!(
            "Open the Git repository root and synchronize it before strict execution: {error}"
        ))
    })?;
    require(
        repository
            .workdir()
            .map(Path::canonicalize)
            .transpose()?
            .as_ref()
            == Some(&root),
        "Strict execution needs the Git repository root, not a nested project folder.",
    )?;
    Ok((root, repository))
}

fn regular_metadata_path(metadata: &Path) -> Result<()> {
    let parent = metadata
        .parent()
        .ok_or_else(|| Error::new("Invalid private source binding path.".into()))?;
    match fs::symlink_metadata(parent) {
        Ok(attributes) => require(
            attributes.is_dir() && !attributes.file_type().is_symlink(),
            "The private source binding directory cannot be a symlink.",
        )?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match fs::symlink_metadata(metadata) {
        Ok(attributes) => {
            require(
                attributes.is_file() && !attributes.file_type().is_symlink(),
                "The private source binding must be a regular file.",
            )?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                require(
                    attributes.nlink() == 1,
                    "The private source binding cannot be a hard link.",
                )?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn private_path(metadata: &Path, root: &Path) -> Result<()> {
    regular_metadata_path(metadata)?;
    let parent = metadata
        .parent()
        .ok_or_else(|| Error::new("Invalid private source binding path.".into()))?;
    match parent.canonicalize() {
        Ok(parent) => require(
            !parent.starts_with(root),
            "Source bindings must stay outside the project.",
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_source(repository: &Repository, source: &SourceBinding) -> Result<Oid> {
    let head = Oid::from_str(&source.head).map_err(|error| Error::new(error.to_string()))?;
    let snapshot =
        Oid::from_str(&source.snapshot).map_err(|error| Error::new(error.to_string()))?;
    require(
        head.to_string() == source.head && snapshot.to_string() == source.snapshot,
        "Source identities must name exact Git commits.",
    )?;
    repository.find_commit(head).map_err(|error| {
        Error::new(format!(
            "The source baseline is missing; synchronize again: {error}"
        ))
    })?;
    let candidate = repository.find_commit(snapshot).map_err(|error| {
        Error::new(format!(
            "The source snapshot is missing; synchronize again: {error}"
        ))
    })?;
    require(
        candidate.parent_count() == 1
            && candidate
                .parent_id(0)
                .map_err(|error| Error::new(error.to_string()))?
                == head,
        "The synchronized candidate is not based on its authenticated source HEAD.",
    )?;
    let mut seen = BTreeSet::new();
    for path in &source.tracked_paths {
        scope::validate_path(path)?;
        require(
            !path
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git")),
            "Git administrative paths cannot enter the source index.",
        )?;
        require(
            seen.insert(path),
            "The authenticated source index contains duplicate paths.",
        )?;
    }
    scope::validate_tree_paths(source.tracked_paths.iter().map(String::as_str), true)?;
    Ok(head)
}

/// Read a trusted binding without changing refs, metadata, the index or the checkout.
/// Absence means no binding was established; invalidated or legacy bindings fail closed.
pub fn resolve(
    data_dir: &Path,
    dialog_id: &str,
    root: &Path,
) -> Result<Option<(String, Vec<String>)>> {
    resolve_inner(data_dir, dialog_id, root, false)
}

/// Read an existing binding only for nonfresh run management, such as cancellation.
/// This cannot authorize intake, resume, verification or application after invalidation.
pub fn resolve_existing(
    data_dir: &Path,
    dialog_id: &str,
    root: &Path,
) -> Result<Option<(String, Vec<String>)>> {
    resolve_inner(data_dir, dialog_id, root, true)
}

fn read_bytes(metadata: &Path) -> Result<Option<Vec<u8>>> {
    let mut file = match fs::File::open(metadata) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    (&mut file)
        .take(wire::MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() <= wire::MAX_JSON_BYTES,
        "Source binding is too large; synchronize the project again.",
    )?;
    Ok(Some(bytes))
}

fn resolve_inner(
    data_dir: &Path,
    dialog_id: &str,
    root: &Path,
    existing: bool,
) -> Result<Option<(String, Vec<String>)>> {
    let (metadata, reference) = paths(data_dir, dialog_id)?;
    let (root, repository) = repository(root)?;
    private_path(&metadata, &root)?;
    let Some(bytes) = read_bytes(&metadata)? else {
        return Ok(None);
    };
    let stored: StoredBinding = wire::decode(&bytes).map_err(|_| Error::new(
        "The source binding is unavailable or from an older controller. Inspect and synchronize the project again.".into()))?;
    let (bound_root, source) = match stored {
        StoredBinding::Bound { root, source } => (root, source),
        StoredBinding::Invalidated { previous: Some(source) } if existing => (source.root, source.source),
        StoredBinding::Invalidated { .. } => return Err(Error::new("The synchronized source changed. Inspect and synchronize the project again before strict execution.".into())),
    };
    require(
        bound_root == root,
        "This source binding belongs to another execution workspace. Synchronize this chat again.",
    )?;
    let head = validate_source(&repository, &source)?;
    let current = repository.find_reference(&reference).map_err(|error| {
        Error::new(format!(
            "The bound baseline is unavailable; synchronize again: {error}"
        ))
    })?;
    require(
        current.target() == Some(head),
        "The bound source baseline changed. Synchronize the project again.",
    )?;
    Ok(Some((reference, source.tracked_paths)))
}

/// Install inputs authenticated by the desktop's synchronization adapter.
/// This is the only path which migrates legacy metadata or replaces invalidation.
pub fn bind(
    data_dir: &Path,
    dialog_id: &str,
    root: &Path,
    source: SourceBinding,
) -> Result<SourceBinding> {
    let (metadata, reference) = paths(data_dir, dialog_id)?;
    let (root, repository) = repository(root)?;
    require(
        !metadata.starts_with(&root),
        "Source bindings must stay outside the project.",
    )?;
    private_path(&metadata, &root)?;
    let head = validate_source(&repository, &source)?;
    let parent = metadata
        .parent()
        .ok_or_else(|| Error::new("Invalid private source binding path.".into()))?;
    fs::create_dir_all(parent)?;
    private_path(&metadata, &root)?;
    let stored = StoredBinding::Bound {
        root,
        source: source.clone(),
    };
    let bytes = wire::canonical(&stored)?;
    repository
        .reference(
            &reference,
            head,
            true,
            "Bind strict execution to desktop source HEAD",
        )
        .map_err(|error| Error::new(error.to_string()))?;
    crate::atomic_file::write_atomic_private(&metadata, &bytes)?;
    Ok(source)
}

/// Invalidate future strict intake before synchronized source changes.
/// A tombstone prevents a remote workspace from silently falling back to local HEAD.
pub fn invalidate(data_dir: &Path, dialog_id: &str) -> Result<()> {
    let (metadata, _) = paths(data_dir, dialog_id)?;
    regular_metadata_path(&metadata)?;
    let previous = match read_bytes(&metadata)? {
        Some(bytes) => match wire::decode::<StoredBinding>(&bytes) {
            Ok(StoredBinding::Bound { root, source }) => Some(BoundSource { root, source }),
            Ok(StoredBinding::Invalidated { previous }) => previous,
            // Legacy or corrupt documents carry no reusable authority. Only a
            // new authenticated bind may replace this empty tombstone.
            Err(_) => None,
        },
        None => None,
    };
    crate::atomic_file::write_atomic_private(
        &metadata,
        &wire::canonical(&StoredBinding::Invalidated { previous })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(repository: &Repository, content: &str, parent: Option<Oid>) -> Oid {
        let blob = repository.blob(content.as_bytes()).unwrap();
        let mut builder = repository.treebuilder(None).unwrap();
        builder.insert("source.txt", blob, 0o100644).unwrap();
        let tree = repository.find_tree(builder.write().unwrap()).unwrap();
        let signature = git2::Signature::now("binding test", "test@example.invalid").unwrap();
        let parent = parent.map(|id| repository.find_commit(id).unwrap());
        repository
            .commit(
                None,
                &signature,
                &signature,
                "source",
                &tree,
                &parent.iter().collect::<Vec<_>>(),
            )
            .unwrap()
    }

    fn fixture() -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        Repository,
        SourceBinding,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        fs::create_dir_all(&root).unwrap();
        let repository = Repository::init(&root).unwrap();
        let head = commit(&repository, "committed baseline", None);
        let snapshot = commit(&repository, "selected working tree", Some(head));
        repository
            .reference("refs/heads/main", snapshot, true, "fixture")
            .unwrap();
        repository.set_head("refs/heads/main").unwrap();
        fs::write(root.join("source.txt"), "selected working tree").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("source.txt")).unwrap();
        index.write().unwrap();
        let data = directory.path().join("private");
        let source = SourceBinding {
            head: head.to_string(),
            snapshot: snapshot.to_string(),
            tracked_paths: vec!["source.txt".into()],
        };
        (directory, data, root, repository, source)
    }

    #[test]
    fn authenticated_binding_preserves_checkout_and_index_and_resolves_without_writes() {
        let (_directory, data, root, repository, source) = fixture();
        assert_eq!(resolve(&data, "chat", &root).unwrap(), None);
        let index = fs::read(repository.path().join("index")).unwrap();
        let head = repository.head().unwrap().target();
        assert_eq!(bind(&data, "chat", &root, source.clone()).unwrap(), source);
        let (metadata, reference) = paths(&data, "chat").unwrap();
        let before = fs::read(&metadata).unwrap();
        let modified = fs::metadata(&metadata).unwrap().modified().unwrap();
        assert_eq!(
            resolve(&data, "chat", &root).unwrap(),
            Some((reference.clone(), source.tracked_paths.clone()))
        );
        assert_eq!(fs::read(&metadata).unwrap(), before);
        assert_eq!(
            fs::metadata(&metadata).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(
            repository.find_reference(&reference).unwrap().target(),
            Some(Oid::from_str(&source.head).unwrap())
        );
        assert_eq!(repository.head().unwrap().target(), head);
        assert_eq!(fs::read(repository.path().join("index")).unwrap(), index);
        assert_eq!(
            fs::read_to_string(root.join("source.txt")).unwrap(),
            "selected working tree"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(metadata).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn source_parent_workspace_and_pinned_reference_must_remain_exact() {
        let (_directory, data, root, repository, source) = fixture();
        bind(&data, "chat", &root, source.clone()).unwrap();
        let other = root.with_file_name("other-worktree");
        fs::create_dir_all(&other).unwrap();
        Repository::init(&other).unwrap();
        assert!(resolve(&data, "chat", &other)
            .unwrap_err()
            .message
            .contains("another execution workspace"));
        let (metadata, reference) = paths(&data, "chat").unwrap();
        repository
            .reference(
                &reference,
                Oid::from_str(&source.snapshot).unwrap(),
                true,
                "tamper",
            )
            .unwrap();
        assert!(resolve(&data, "chat", &root)
            .unwrap_err()
            .message
            .contains("baseline changed"));
        bind(&data, "chat", &root, source.clone()).unwrap();
        repository
            .reference_symbolic(&reference, "refs/heads/main", true, "tamper")
            .unwrap();
        assert!(resolve(&data, "chat", &root).is_err());
        assert!(resolve_existing(&data, "chat", &root).is_err());
        bind(&data, "chat", &root, source.clone()).unwrap();
        let unrelated = commit(&repository, "unrelated", None);
        let forged = SourceBinding {
            head: unrelated.to_string(),
            ..source.clone()
        };
        assert!(bind(&data, "chat", &root, forged.clone()).is_err());
        crate::atomic_file::write_atomic_private(
            &metadata,
            &wire::canonical(&StoredBinding::Bound {
                root: root.canonicalize().unwrap(),
                source: forged,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(resolve(&data, "chat", &root)
            .unwrap_err()
            .message
            .contains("authenticated source HEAD"));
    }

    #[test]
    fn source_index_paths_cannot_escape_duplicate_or_name_git_administration() {
        let (_directory, data, root, _repository, source) = fixture();
        for tracked_paths in [
            vec!["../outside".into()],
            vec!["/outside".into()],
            vec![".git/config".into()],
            vec![".GIT/config".into()],
            vec!["src/./file".into()],
            vec!["src//file".into()],
            vec!["source.txt".into(), "source.txt".into()],
            vec!["src".into(), "src/file".into()],
        ] {
            let invalid = SourceBinding {
                tracked_paths,
                ..source.clone()
            };
            assert!(
                bind(&data, "chat", &root, invalid.clone()).is_err(),
                "{:?}",
                invalid.tracked_paths
            );
        }
        bind(&data, "chat", &root, source.clone()).unwrap();
        let (metadata, _) = paths(&data, "chat").unwrap();
        let invalid = SourceBinding {
            tracked_paths: vec!["source.txt".into(), "source.txt".into()],
            ..source
        };
        crate::atomic_file::write_atomic_private(
            &metadata,
            &wire::canonical(&StoredBinding::Bound {
                root: root.canonicalize().unwrap(),
                source: invalid,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(resolve(&data, "chat", &root).is_err());
    }

    #[test]
    fn legacy_or_invalidated_metadata_requires_a_new_authenticated_binding() {
        let (_directory, data, root, _repository, source) = fixture();
        let (metadata, _) = paths(&data, "chat").unwrap();
        crate::atomic_file::write_atomic_private(&metadata, &wire::canonical(&source).unwrap())
            .unwrap();
        let old = fs::read(&metadata).unwrap();
        assert!(resolve(&data, "chat", &root).is_err());
        assert!(resolve_existing(&data, "chat", &root).is_err());
        assert_eq!(
            fs::read(&metadata).unwrap(),
            old,
            "readers cannot migrate legacy authority"
        );
        bind(&data, "chat", &root, source.clone()).unwrap();
        assert!(resolve(&data, "chat", &root).unwrap().is_some());
        invalidate(&data, "chat").unwrap();
        assert!(
            resolve(&data, "chat", &root).is_err(),
            "invalidated cloud input cannot become local fallback"
        );
        let (_, reference) = paths(&data, "chat").unwrap();
        assert_eq!(
            resolve_existing(&data, "chat", &root).unwrap(),
            Some((reference, source.tracked_paths.clone())),
            "cancellation may retain the previous exact workspace binding"
        );
        invalidate(&data, "chat").unwrap();
        assert!(resolve_existing(&data, "chat", &root).unwrap().is_some());
        bind(&data, "chat", &root, source).unwrap();
        assert!(resolve(&data, "chat", &root).unwrap().is_some());
        invalidate(&data, "not-yet-bound").unwrap();
        assert!(resolve(&data, "not-yet-bound", &root).is_err());
        assert!(resolve_existing(&data, "not-yet-bound", &root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn bindings_reject_candidate_storage_and_symlinked_private_metadata() {
        let (directory, data, root, _repository, source) = fixture();
        assert!(bind(&root.join("private"), "chat", &root, source.clone()).is_err());
        bind(&data, "chat", &root, source.clone()).unwrap();
        let (metadata, _) = paths(&data, "chat").unwrap();
        let moved = directory.path().join("moved.json");
        fs::rename(&metadata, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &metadata).unwrap();
        assert!(resolve(&data, "chat", &root).is_err());
        assert!(bind(&data, "chat", &root, source).is_err());
        assert!(invalidate(&data, "chat").is_err());
    }
}
