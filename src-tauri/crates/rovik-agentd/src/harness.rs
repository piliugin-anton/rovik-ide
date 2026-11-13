//! Trusted source-baseline bindings for verification in synchronized chat worktrees.

use crate::{daemon::Daemon, work};
use git2::{Oid, Repository};
use rovik_core::chat_db::{projects, store};
use rovik_protocol::harness::{Operation, SourceBinding};
use rovik_protocol::{Fail, FailCode, Response};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Serve an already authenticated chat using only its owned project and worktree.
pub fn serve(
    daemon: &Daemon,
    dialog_id: String,
    source: Option<SourceBinding>,
    operation: Operation,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Fail>> + Send + '_>> {
    Box::pin(async move {
        let fresh = matches!(
            operation,
            Operation::Inspect { .. }
                | Operation::Verify { .. }
                | Operation::Configure { .. }
                | Operation::Status { .. }
                | Operation::Resume { .. }
        );
        // Share the worktree mutation guard with WorkSync and ordinary chat runs.
        // Private-job cancellation and exact approval decisions remain available.
        let _files = fresh
            .then(|| work::hold_files(daemon, &dialog_id))
            .transpose()?;
        let place = daemon
            .store
            .place(&dialog_id)
            .ok_or_else(|| invalid("Synchronize this chat's project before using verification"))?;
        let root = work::root(daemon, &dialog_id)
            .ok_or_else(|| invalid("This chat has no owned project worktree"))?;
        let id = dialog_id.clone();
        let checked_root = root.clone();
        let project = place.project;
        let expected_project = project.clone();
        let authorized = daemon
            .core
            .db
            .read(move |db| {
                let actual = store::dialog_project(db, &id)?;
                Ok(actual.as_deref() == Some(expected_project.as_str())
                    && projects::chat_may_use(db, actual.as_deref(), &checked_root)?)
            })
            .await
            .map_err(invalid)?;
        if !authorized {
            return Err(invalid(
                "The chat's project binding changed; reopen its execution context",
            ));
        }
        if fresh && source.is_none() {
            return Err(invalid(
                "Synchronize the current desktop source before using verification",
            ));
        }
        let snapshot = daemon.store.base_of(&dialog_id);
        if source
            .as_ref()
            .is_some_and(|source| Some(&source.snapshot) != snapshot.as_ref())
        {
            return Err(invalid(
            "The source binding no longer matches the chat's synchronized snapshot; inspect again",
        ));
        }
        let key: String = Sha256::digest(dialog_id.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let reference = format!("refs/heads/rovik-harness-baseline-{key}");
        let metadata = daemon
            .core
            .paths
            .data_dir
            .join("harness-sources")
            .join(format!("{key}.json"));
        let bound_root = root.clone();
        let bound_reference = reference.clone();
        let binding = rovik_core::rt::spawn_blocking(move || {
            bind_source(&bound_root, &metadata, &bound_reference, source.as_ref())
        })
        .await
        .map_err(|error| invalid(error.to_string()))?
        .map_err(invalid)?;
        if fresh && daemon.store.base_of(&dialog_id).as_deref() != Some(binding.snapshot.as_str()) {
            return Err(invalid(
                "The chat synchronized newer inputs while binding verification; inspect again",
            ));
        }
        rovik_core::harness::ide::execute_bound(
            daemon.core.paths.data_dir.join("harness"),
            root,
            project,
            Some(reference),
            Some(binding.tracked_paths),
            operation,
        )
        .await
        .map(Response::Harness)
        .map_err(|error| invalid(error.to_string()))
    })
}

fn invalid(message: impl Into<String>) -> Fail {
    Fail::new(FailCode::BadRequest, message)
}

/// Persist only desktop-authenticated bindings, outside candidate-controlled files.
fn bind_source(
    root: &Path,
    metadata: &Path,
    reference: &str,
    incoming: Option<&SourceBinding>,
) -> Result<SourceBinding, String> {
    let repository = Repository::open(root).map_err(|error| format!("Verification needs the Git repository root as its project; open that root and inspect again: {error}"))?;
    if let Some(source) = incoming {
        let head = Oid::from_str(&source.head).map_err(|error| error.to_string())?;
        let snapshot = Oid::from_str(&source.snapshot).map_err(|error| error.to_string())?;
        repository
            .find_commit(head)
            .map_err(|error| format!("The synchronized source baseline is missing: {error}"))?;
        let candidate = repository
            .find_commit(snapshot)
            .map_err(|error| error.to_string())?;
        if candidate.parent_count() != 1
            || candidate.parent_id(0).map_err(|error| error.to_string())? != head
        {
            return Err(
                "The synchronized candidate is not based on the authenticated source HEAD".into(),
            );
        }
        for path in &source.tracked_paths {
            if path.is_empty()
                || path.contains('\\')
                || Path::new(path)
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err("The source index contains an unsafe project path".into());
            }
        }
        repository
            .reference(
                reference,
                head,
                true,
                "Bind verification to desktop source HEAD",
            )
            .map_err(|error| error.to_string())?;
        let parent = metadata
            .parent()
            .ok_or("Invalid private source binding path")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        rovik_core::atomic_file::write_atomic_private(
            metadata,
            &serde_json::to_vec(source).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(source.clone())
    } else {
        let source: SourceBinding = serde_json::from_slice(
            &std::fs::read(metadata)
                .map_err(|_| "Inspect this chat's project before managing verification")?,
        )
        .map_err(|error| error.to_string())?;
        let current = repository
            .find_reference(reference)
            .and_then(|reference| reference.peel_to_commit())
            .map_err(|error| error.to_string())?;
        if current.id().to_string() != source.head {
            return Err("The verification baseline changed; inspect the project again".into());
        }
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(repository: &Repository, content: &str, parent: Option<Oid>) -> Oid {
        let blob = repository.blob(content.as_bytes()).unwrap();
        let mut tree = repository.treebuilder(None).unwrap();
        tree.insert("source.txt", blob, 0o100644).unwrap();
        let tree = repository.find_tree(tree.write().unwrap()).unwrap();
        let signature = git2::Signature::now("test", "test@example.com").unwrap();
        let parent = parent.map(|parent| repository.find_commit(parent).unwrap());
        repository
            .commit(
                None,
                &signature,
                &signature,
                "test",
                &tree,
                &parent.iter().collect::<Vec<_>>(),
            )
            .unwrap()
    }

    #[test]
    fn cloud_verification_uses_source_head_and_rejects_unrelated_shadow_commits() {
        let directory =
            std::env::temp_dir().join(format!("rovik-harness-source-{}", uuid::Uuid::new_v4()));
        let root = directory.join("worktree");
        std::fs::create_dir_all(&root).unwrap();
        let repository = Repository::init(&root).unwrap();
        let head = commit(&repository, "committed baseline", None);
        let snapshot = commit(&repository, "dirty candidate", Some(head));
        repository
            .reference("refs/heads/main", snapshot, true, "test")
            .unwrap();
        repository.set_head("refs/heads/main").unwrap();
        let source = SourceBinding {
            head: head.to_string(),
            snapshot: snapshot.to_string(),
            tracked_paths: vec!["source.txt".into()],
        };
        let metadata = directory.join("private/source.json");
        let reference = "refs/heads/rovik-harness-baseline-chat";
        assert_eq!(
            bind_source(&root, &metadata, reference, Some(&source)).unwrap(),
            source
        );
        assert_eq!(
            repository.find_reference(reference).unwrap().target(),
            Some(head)
        );
        assert_eq!(
            repository.head().unwrap().target(),
            Some(snapshot),
            "candidate HEAD stays intact"
        );
        assert_eq!(
            bind_source(&root, &metadata, reference, None).unwrap(),
            source
        );
        let unrelated = commit(&repository, "unrelated", None);
        let forged = SourceBinding {
            head: unrelated.to_string(),
            ..source.clone()
        };
        assert!(bind_source(&root, &metadata, reference, Some(&forged)).is_err());
        let unsafe_path = SourceBinding {
            tracked_paths: vec!["../outside".into()],
            ..source.clone()
        };
        assert!(bind_source(&root, &metadata, reference, Some(&unsafe_path)).is_err());
        repository
            .reference(reference, snapshot, true, "tampered")
            .unwrap();
        assert!(bind_source(&root, &metadata, reference, None).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
