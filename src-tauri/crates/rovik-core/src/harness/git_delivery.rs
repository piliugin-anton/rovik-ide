//! Isolated Git-object preparation and conditional delivery. No checkout/index writes.
use super::GitIntegration;
use rovik_harness::{
    approvals::Principal,
    controller::{Controller, Integration, TaskSetup},
    identity::Digest,
    ledger::Ledger,
    runner,
    snapshot::Snapshot,
    Error, Result,
};
use serde::{Deserialize, Serialize};

/// A reconstructed integration tree, which still needs a new full verification run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeCandidate {
    /// Registered repository identity.
    pub repository_id: String,
    /// Exact local destination reference.
    pub destination_reference: String,
    /// Destination head used by the three-way merge.
    pub expected_head: String,
    /// Original approved candidate identity.
    pub source_candidate: Digest,
    /// Destination source snapshot, before applying the proposed change.
    pub base: Snapshot,
    /// Exact reconstructed tree, including retained destination changes.
    pub candidate: Snapshot,
}
/// Concrete commit and ref update presented for a separate delivery approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCandidate {
    /// Accepted verification run, distinct from any earlier pre-merge run.
    pub run_id: String,
    /// Registered repository.
    pub repository_id: String,
    /// Canonical local integration repository selected when preparing this exact delivery.
    /// Approval cannot be replayed against another checkout or bare repository with the same head.
    pub integration_repository: std::path::PathBuf,
    /// Branch to advance, never a checkout path.
    pub destination_reference: String,
    /// Required old head for compare-and-swap.
    pub expected_head: String,
    /// Complete verified candidate identity.
    pub candidate_digest: Digest,
    /// Immutable prepared Git commit identity, including author, tree and message.
    pub commit_id: String,
}
/// Observed conditional ref update; deployment is a separate workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    /// Exact reviewed delivery request.
    pub request_digest: Digest,
    /// Resulting commit at the destination reference.
    pub commit_id: String,
    /// True when an earlier attempt already performed this exact update.
    pub already_delivered: bool,
}
fn git(error: git2::Error) -> Error {
    Error::new(format!("Git integration: {error}"))
}
fn oid(value: &str) -> Result<git2::Oid> {
    git2::Oid::from_str(value).map_err(git)
}
fn tree(repository: &git2::Repository, ledger: &Ledger, snapshot: &Snapshot) -> Result<git2::Oid> {
    snapshot.validate(ledger, true)?;
    let mut index = git2::Index::new().map_err(git)?;
    for (path, file) in &snapshot.files {
        let bytes = ledger.read(&file.content.digest)?;
        let blob = repository.blob(&bytes).map_err(git)?;
        index
            .add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: file.mode,
                uid: 0,
                gid: 0,
                file_size: u32::try_from(bytes.len())
                    .map_err(|_| Error::new("Git blob exceeds index size limit".into()))?,
                id: blob,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            })
            .map_err(git)?;
    }
    index.write_tree_to(repository).map_err(git)
}
fn update_reference(
    repository: &git2::Repository,
    reference: &str,
    expected: git2::Oid,
    next: git2::Oid,
) -> Result<bool> {
    let mut transaction = repository.transaction().map_err(git)?;
    transaction.lock_ref(reference).map_err(git)?;
    let current = repository
        .find_reference(reference)
        .and_then(|r| r.peel_to_commit())
        .map_err(git)?
        .id();
    if current == next {
        return Ok(true);
    }
    if current != expected {
        return Err(Error::new(
            "destination changed; reconstruct and verify a new integration candidate".into(),
        ));
    }
    transaction
        .set_target(reference, next, None, "verified harness delivery")
        .map_err(git)?;
    transaction.commit().map_err(git)?;
    Ok(false)
}
impl GitIntegration {
    /// Merge immutable candidate objects with the exact destination head in memory.
    /// Conflicts block preparation; uncommitted user files and the index are untouched.
    pub fn prepare_merge(
        &self,
        ledger: &Ledger,
        base: &Snapshot,
        candidate: &Snapshot,
        expected_head: &str,
    ) -> Result<MergeCandidate> {
        if self.reference == "HEAD" {
            return Err(Error::new(
                "select a destination branch before preparing a merge candidate".into(),
            ));
        }
        base.validate(ledger, true)?;
        candidate.validate(ledger, true)?;
        if candidate.base_revision != base.base_revision
            || candidate.dependency_assets != base.dependency_assets
        {
            return Err(Error::new(
                "candidate changed its registered base or dependency assets".into(),
            ));
        }
        if self.current_revision(&self.repository_id)? != expected_head {
            return Err(Error::new(
                "destination moved before merge preparation".into(),
            ));
        }
        let repository = git2::Repository::open(&self.root).map_err(git)?;
        let ancestor = repository
            .find_commit(oid(&base.base_revision)?)
            .map_err(git)?;
        let destination = repository.find_commit(oid(expected_head)?).map_err(git)?;
        if ancestor.id() != destination.id()
            && !repository
                .graph_descendant_of(destination.id(), ancestor.id())
                .map_err(git)?
        {
            return Err(Error::new(
                "destination no longer descends from the registered base".into(),
            ));
        }
        let candidate_tree = repository
            .find_tree(tree(&repository, ledger, candidate)?)
            .map_err(git)?;
        let mut merged = repository
            .merge_trees(
                &ancestor.tree().map_err(git)?,
                &destination.tree().map_err(git)?,
                &candidate_tree,
                None,
            )
            .map_err(git)?;
        if merged.has_conflicts() {
            let paths = merged
                .conflicts()
                .map_err(git)?
                .map(|conflict| {
                    let conflict = conflict.map_err(git)?;
                    let entry = conflict
                        .our
                        .or(conflict.their)
                        .or(conflict.ancestor)
                        .ok_or_else(|| Error::new("empty Git conflict".into()))?;
                    String::from_utf8(entry.path)
                        .map_err(|_| Error::new("non-UTF-8 merge conflict path".into()))
                })
                .collect::<Result<Vec<_>>>()?;
            return Err(Error::classified(
                rovik_harness::FailureCategory::Design,
                format!("merge candidate has conflicts: {}", paths.join(", ")),
            ));
        }
        let merged_tree = repository
            .find_tree(merged.write_tree_to(&repository).map_err(git)?)
            .map_err(git)?;
        let mut next_base = Self::tree_snapshot(
            &repository,
            &destination.tree().map_err(git)?,
            ledger,
            expected_head,
        )?;
        next_base.dependency_assets = base.dependency_assets.clone();
        let mut next_candidate =
            Self::tree_snapshot(&repository, &merged_tree, ledger, expected_head)?;
        next_candidate.dependency_assets = candidate.dependency_assets.clone();
        next_candidate.deletions = next_base
            .files
            .keys()
            .filter(|path| !next_candidate.files.contains_key(*path))
            .cloned()
            .collect();
        if self.current_revision(&self.repository_id)? != expected_head {
            return Err(Error::new(
                "destination moved during merge preparation; reconstruct it again".into(),
            ));
        }
        Ok(MergeCandidate {
            repository_id: self.repository_id.clone(),
            destination_reference: self.reference.clone(),
            expected_head: expected_head.into(),
            source_candidate: candidate.digest()?,
            base: next_base,
            candidate: next_candidate,
        })
    }

    /// Produce an unregistered verification intake. All required approvals remain pending.
    /// A changed cited source needs a new design decision; it is never relabeled as reviewed.
    pub fn merge_setup(
        &self,
        original: &TaskSetup,
        merged: &MergeCandidate,
        run_id: &str,
    ) -> Result<TaskSetup> {
        if merged.repository_id != original.repository_id || run_id == original.run_id {
            return Err(Error::new(
                "merge verification needs the same repository and a new run identity".into(),
            ));
        }
        let mut setup = original.clone();
        setup.run_id = run_id.into();
        setup.plan.run_id = run_id.into();
        for citation in &mut setup.design.existing_code_considered {
            if original.base.files.get(&citation.file) != merged.base.files.get(&citation.file) {
                return Err(Error::classified(
                    rovik_harness::FailureCategory::Design,
                    format!(
                        "destination changed cited code {}; obtain a new design",
                        citation.file
                    ),
                ));
            }
            citation.source_revision = merged.base.digest()?;
        }
        setup.base = merged.base.clone();
        setup.initial_candidate = Some(merged.candidate.clone());
        setup.plan.design_digest = Digest::of_json(&setup.design)?;
        Ok(setup)
    }

    /// Write an immutable, unreferenced commit for review. No branch is advanced.
    pub fn prepare_delivery(
        &self,
        controller: &Controller,
        run_id: &str,
        message: &str,
    ) -> Result<DeliveryCandidate> {
        let setup = controller.setup(run_id)?;
        if setup.repository_id != self.repository_id || self.reference == "HEAD" {
            return Err(Error::new(
                "delivery requires the registered repository and explicit destination branch"
                    .into(),
            ));
        }
        if controller.ledger.run(run_id)?.state != rovik_harness::state::State::Accepted
            || controller.verify(run_id, self)?.verdict != rovik_harness::evidence::Verdict::Ready
        {
            return Err(Error::new(
                "delivery requires current acceptance for this integration candidate".into(),
            ));
        }
        if message.trim().is_empty() || message.len() > 8192 {
            return Err(Error::new("delivery needs a bounded commit message".into()));
        }
        let candidate = controller
            .current_candidate(run_id)?
            .ok_or_else(|| Error::new("accepted run has no candidate".into()))?;
        let repository = git2::Repository::open(&self.root).map_err(git)?;
        let tree = repository
            .find_tree(tree(&repository, &controller.ledger, &candidate)?)
            .map_err(git)?;
        let parent = repository
            .find_commit(oid(&candidate.base_revision)?)
            .map_err(git)?;
        let signature = repository.signature().map_err(git)?;
        let commit = repository
            .commit(None, &signature, &signature, message, &tree, &[&parent])
            .map_err(git)?;
        let delivery = DeliveryCandidate {
            run_id: run_id.into(),
            repository_id: self.repository_id.clone(),
            integration_repository: self.root.clone(),
            destination_reference: self.reference.clone(),
            expected_head: candidate.base_revision.clone(),
            candidate_digest: candidate.digest()?,
            commit_id: commit.to_string(),
        };
        controller
            .ledger
            .store_json("delivery", &delivery, runner::now_ms()?)?;
        Ok(delivery)
    }

    /// Advance a bare repository branch only after exact delivery approval and a final gate.
    /// Git's reference transaction provides CAS; no working directory is reset or overwritten.
    pub fn deliver(
        &self,
        controller: &Controller,
        principal: &Principal,
        delivery: &DeliveryCandidate,
    ) -> Result<DeliveryReceipt> {
        let setup = controller.setup(&delivery.run_id)?;
        setup.authority.authorize(principal, "delivery")?;
        if delivery.repository_id != self.repository_id
            || delivery.integration_repository != self.root
            || delivery.destination_reference != self.reference
            || self.reference == "HEAD"
        {
            return Err(Error::new(
                "delivery request belongs to another repository or reference".into(),
            ));
        }
        let candidate = controller
            .current_candidate(&delivery.run_id)?
            .ok_or_else(|| Error::new("delivery has no candidate".into()))?;
        if candidate.digest()? != delivery.candidate_digest
            || candidate.base_revision != delivery.expected_head
        {
            return Err(Error::new(
                "delivery request does not match the selected candidate".into(),
            ));
        }
        let repository = git2::Repository::open(&self.root).map_err(git)?;
        if !repository.is_bare() {
            return Err(Error::new("conditional delivery requires a bare integration repository; export or prepare the commit for a checkout-based workflow".into()));
        }
        let commit = repository
            .find_commit(oid(&delivery.commit_id)?)
            .map_err(git)?;
        if commit.tree_id() != tree(&repository, &controller.ledger, &candidate)?
            || commit.parent_count() != 1
            || commit.parent_id(0).map_err(git)? != oid(&delivery.expected_head)?
        {
            return Err(Error::new(
                "prepared commit has a different tree or parent".into(),
            ));
        }
        let request_digest = Digest::of_json(delivery)?;
        // A retry may see the exact prepared commit after a crash following the ref update.
        // Its tree and parent were checked above. Still require current approvals and the gate.
        struct Destination<'a> {
            integration: &'a GitIntegration,
            delivery: &'a DeliveryCandidate,
        }
        impl Integration for Destination<'_> {
            fn current_revision(&self, repository_id: &str) -> Result<String> {
                let revision = self.integration.current_revision(repository_id)?;
                Ok(if revision == self.delivery.commit_id {
                    self.delivery.expected_head.clone()
                } else {
                    revision
                })
            }
        }
        let destination = Destination {
            integration: self,
            delivery,
        };
        let already = controller.deliver_authorized(
            principal,
            &delivery.run_id,
            &request_digest,
            &destination,
            || {
                update_reference(
                    &repository,
                    &self.reference,
                    oid(&delivery.expected_head)?,
                    commit.id(),
                )
            },
        )?;
        let receipt = DeliveryReceipt {
            request_digest,
            commit_id: delivery.commit_id.clone(),
            already_delivered: already,
        };
        controller
            .ledger
            .store_json("delivery-receipt", &receipt, runner::now_ms()?)?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_harness::fixtures;
    fn commit(
        repo: &git2::Repository,
        ledger: &Ledger,
        snapshot: &Snapshot,
        parent: Option<git2::Oid>,
    ) -> git2::Oid {
        let tree = repo
            .find_tree(tree(repo, ledger, snapshot).unwrap())
            .unwrap();
        let parent = parent.map(|id| repo.find_commit(id).unwrap());
        let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        repo.commit(
            Some("refs/heads/main"),
            &signature,
            &signature,
            "fixture",
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .unwrap()
    }
    #[test]
    fn merging_immutable_objects_preserves_dirty_files_index_and_unrelated_destination_edits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let repo = git2::Repository::init(&root).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let ledger = Ledger::open(&dir.path().join("state")).unwrap();
        let original = fixtures::snapshot(
            &ledger,
            "uncommitted",
            &[("source.rs", "original\n"), ("other.rs", "original\n")],
        )
        .unwrap();
        let base_id = commit(&repo, &ledger, &original, None);
        let integration =
            GitIntegration::for_reference("fixture".into(), &root, "refs/heads/main").unwrap();
        let base = integration.snapshot(&ledger).unwrap();
        let candidate = fixtures::snapshot(
            &ledger,
            &base_id.to_string(),
            &[("source.rs", "candidate\n"), ("other.rs", "original\n")],
        )
        .unwrap();
        let destination = fixtures::snapshot(
            &ledger,
            &base_id.to_string(),
            &[
                ("source.rs", "original\n"),
                ("other.rs", "destination change\n"),
            ],
        )
        .unwrap();
        let head = commit(&repo, &ledger, &destination, Some(base_id));
        std::fs::write(root.join("source.rs"), "user's uncommitted source").unwrap();
        std::fs::write(root.join("untracked.txt"), "user's untracked data").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("source.rs")).unwrap();
        index.write().unwrap();
        let before = std::fs::read(repo.path().join("index")).unwrap();
        let merged = integration
            .prepare_merge(&ledger, &base, &candidate, &head.to_string())
            .unwrap();
        assert_eq!(merged.candidate.base_revision, head.to_string());
        assert_eq!(
            ledger
                .read(&merged.candidate.files["source.rs"].content.digest)
                .unwrap(),
            b"candidate\n"
        );
        assert_eq!(
            ledger
                .read(&merged.candidate.files["other.rs"].content.digest)
                .unwrap(),
            b"destination change\n"
        );
        assert_eq!(
            std::fs::read(root.join("source.rs")).unwrap(),
            b"user's uncommitted source"
        );
        assert_eq!(
            std::fs::read(root.join("untracked.txt")).unwrap(),
            b"user's untracked data"
        );
        assert_eq!(std::fs::read(repo.path().join("index")).unwrap(), before);
        assert_eq!(repo.head().unwrap().target(), Some(head));
        assert!(integration
            .prepare_merge(&ledger, &base, &candidate, &base_id.to_string())
            .unwrap_err()
            .message
            .contains("moved"));
        let conflict = fixtures::snapshot(
            &ledger,
            &base_id.to_string(),
            &[
                ("source.rs", "conflicting destination\n"),
                ("other.rs", "original\n"),
            ],
        )
        .unwrap();
        let conflict_head = commit(&repo, &ledger, &conflict, Some(head));
        assert_eq!(
            integration
                .prepare_merge(&ledger, &base, &candidate, &conflict_head.to_string())
                .unwrap_err()
                .category,
            rovik_harness::FailureCategory::Design
        );
    }
    #[test]
    fn conditional_updates_are_idempotent_and_never_overwrite_a_different_destination_head() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init_bare(dir.path().join("repo")).unwrap();
        let ledger = Ledger::open(&dir.path().join("state")).unwrap();
        let base = fixtures::snapshot(&ledger, "base", &[("source.rs", "base")]).unwrap();
        let first = commit(&repo, &ledger, &base, None);
        let changed =
            fixtures::snapshot(&ledger, &first.to_string(), &[("source.rs", "changed")]).unwrap();
        let second = commit(&repo, &ledger, &changed, Some(first));
        repo.reference("refs/heads/main", first, true, "fixture")
            .unwrap();
        assert!(!update_reference(&repo, "refs/heads/main", first, second).unwrap());
        assert!(update_reference(&repo, "refs/heads/main", first, second).unwrap());
        let third = commit(&repo, &ledger, &base, Some(second));
        assert!(update_reference(&repo, "refs/heads/main", first, second).is_err());
        assert_eq!(
            repo.find_reference("refs/heads/main").unwrap().target(),
            Some(third)
        );
    }
}
