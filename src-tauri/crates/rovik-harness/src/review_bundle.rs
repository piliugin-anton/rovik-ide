//! Portable review bundles with exact patches, immutable observations and approvals.

use crate::{
    controller::Controller,
    evidence::CheckResult,
    identity::Digest,
    require,
    snapshot::{FileEntry, Snapshot},
    state::State,
    wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// One file change, including deletions, mode changes and binary content identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileChange {
    /// Exact preimage, absent for an addition.
    pub before: Option<FileEntry>,
    /// Exact resulting file, absent for a deletion.
    pub after: Option<FileEntry>,
}

/// A portable content-addressed patch; blobs live in the bundle's artifacts directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotPatch {
    /// Exactly 1.
    pub schema_version: u32,
    /// Required complete baseline identity.
    pub base_digest: Digest,
    /// Exact complete candidate identity.
    pub candidate_digest: Digest,
    /// Complete intended target metadata, validated against the delta on application.
    pub candidate: Snapshot,
    /// Changed paths only; unchanged content remains bound by the base identity.
    pub changes: BTreeMap<String, FileChange>,
}

impl SnapshotPatch {
    /// Construct a deterministic patch without relying on a mutable checkout.
    pub fn between(base: &Snapshot, candidate: &Snapshot) -> Result<Self> {
        require(
            base.base_revision == candidate.base_revision,
            "patch changes its Git base",
        )?;
        require(
            base.dependency_assets == candidate.dependency_assets,
            "patch changes dependency assets",
        )?;
        let deleted: BTreeSet<_> = base
            .files
            .keys()
            .filter(|path| !candidate.files.contains_key(*path))
            .collect();
        require(
            deleted == candidate.deletions.iter().collect()
                && deleted.len() == candidate.deletions.len(),
            "patch deletion manifest is incomplete",
        )?;
        let paths: BTreeSet<_> = base.files.keys().chain(candidate.files.keys()).collect();
        let changes = paths
            .into_iter()
            .filter_map(|path| {
                let before = base.files.get(path);
                let after = candidate.files.get(path);
                (before != after).then(|| {
                    (
                        path.clone(),
                        FileChange {
                            before: before.cloned(),
                            after: after.cloned(),
                        },
                    )
                })
            })
            .collect();
        Ok(Self {
            schema_version: 1,
            base_digest: base.digest()?,
            candidate_digest: candidate.digest()?,
            candidate: candidate.clone(),
            changes,
        })
    }

    /// Check every preimage and the complete result before any delivery adapter writes it.
    pub fn apply(&self, base: &Snapshot) -> Result<Snapshot> {
        require(
            self.schema_version == 1 && self.base_digest == base.digest()?,
            "patch has a different base",
        )?;
        let mut files = base.files.clone();
        for (path, change) in &self.changes {
            crate::scope::validate_path(path)?;
            require(
                files.get(path) == change.before.as_ref(),
                "patch preimage mismatch",
            )?;
            require(
                change.before != change.after,
                "patch contains an unchanged entry",
            )?;
            match &change.after {
                Some(file) => {
                    files.insert(path.clone(), file.clone());
                }
                None => {
                    files.remove(path);
                }
            }
        }
        require(
            files == self.candidate.files
                && self.candidate.digest()? == self.candidate_digest
                && base.base_revision == self.candidate.base_revision
                && base.dependency_assets == self.candidate.dependency_assets,
            "patch does not reconstruct the complete candidate",
        )?;
        require(
            Self::between(base, &self.candidate)?.changes == self.changes,
            "patch delta is incomplete",
        )?;
        Ok(self.candidate.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fixtures, ledger::Ledger, runner};

    #[test]
    fn a_portable_patch_reconstructs_binary_edits_modes_additions_and_deletions() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&directory.path().join("state")).unwrap();
        let base = fixtures::snapshot(
            &ledger,
            "base",
            &[("old.txt", "old"), ("data.bin", "initial")],
        )
        .unwrap();
        let mut candidate = base.clone();
        candidate.files.remove("old.txt");
        candidate.deletions.push("old.txt".into());
        let binary = ledger
            .store(
                "source",
                "application/octet-stream",
                &[0, 255, 17],
                runner::now_ms().unwrap(),
            )
            .unwrap();
        candidate.files.get_mut("data.bin").unwrap().content = binary;
        candidate.files.get_mut("data.bin").unwrap().mode = 0o100755;
        let added = ledger
            .store(
                "source",
                "application/octet-stream",
                b"added",
                runner::now_ms().unwrap(),
            )
            .unwrap();
        candidate.files.insert(
            "new.txt".into(),
            FileEntry {
                mode: 0o100644,
                content: added,
            },
        );
        let patch = SnapshotPatch::between(&base, &candidate).unwrap();
        assert_eq!(patch.apply(&base).unwrap(), candidate);
        let mut tampered = patch.clone();
        tampered.changes.remove("data.bin");
        assert!(tampered.apply(&base).is_err());
        candidate.deletions.clear();
        assert!(SnapshotPatch::between(&base, &candidate).is_err());
        let other = fixtures::snapshot(&ledger, "base", &[("old.txt", "another")]).unwrap();
        assert!(patch.apply(&other).is_err());
    }
}

/// Review bundle identity; acceptance remains tied to the recorded run and destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    /// Exactly 1.
    pub schema_version: u32,
    /// Run whose artifacts were exported.
    pub run_id: String,
    /// Durable event sequence captured by the export transaction.
    pub revision: u64,
    /// Current durable state, including incomplete or rejected outcomes.
    pub state: State,
    /// Exact candidate source identity.
    pub candidate_digest: Digest,
    /// Exact machine-applicable patch bytes.
    pub patch_digest: Digest,
    /// Acceptance with explicit residual risk has a qualified label.
    pub qualified_acceptance: bool,
    /// Exported file name to digest. The manifest does not hash itself.
    pub files: BTreeMap<String, Digest>,
}

impl Controller {
    /// Export one consistent review bundle into a new private directory.
    /// Failed and incomplete attempts remain visible alongside final observations.
    pub fn export_bundle(&self, run_id: &str, destination: &Path) -> Result<BundleManifest> {
        require(
            destination.is_absolute() && !destination.exists(),
            "review bundle needs a new absolute directory",
        )?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.ledger.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let run = self.ledger.run(run_id)?;
        let setup = self.setup(run_id)?;
        let candidate = self
            .current_candidate(run_id)?
            .ok_or_else(|| Error::new("run has no candidate".into()))?;
        candidate.validate(&self.ledger, true)?;
        let patch = SnapshotPatch::between(&setup.base, &candidate)?;
        patch.apply(&setup.base)?;
        let mut blobs = BTreeSet::from([
            run.input_digest.clone(),
            setup.environment.toolchain_digest.clone(),
            Digest::of_json(&setup.contract)?,
            Digest::of_json(&setup.design)?,
            Digest::of_json(&setup.policy)?,
            Digest::of_json(&setup.plan)?,
            Digest::of_json(&setup.environment)?,
        ]);
        if let Some(binding) = &run.binding_digest {
            blobs.insert(binding.clone());
        }
        for snapshot in [
            &setup.base,
            &candidate,
            &setup.oracle,
            &setup.dependencies,
            &setup.negative_control,
        ] {
            blobs.extend(
                snapshot
                    .files
                    .values()
                    .map(|file| file.content.digest.clone()),
            );
            blobs.extend(snapshot.dependency_assets.iter().cloned());
        }
        let mut results = Vec::new();
        {
            let mut query = self.ledger.connection.prepare("SELECT c.result_digest,r.receipt_digest FROM check_results c JOIN receipts r ON c.receipt_id=r.receipt_id WHERE c.run_id=?1 ORDER BY c.created_ms,c.result_id")?;
            for row in query.query_map([run_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })? {
                let (result, receipt) = row?;
                let digest = Digest::parse(&result)?;
                let result: CheckResult = wire::decode(&self.ledger.read(&digest)?)?;
                result.validate()?;
                blobs.insert(digest);
                blobs.insert(Digest::parse(&receipt)?);
                blobs.extend(result.evidence.iter().cloned());
                let key = &result.evidence_key;
                blobs.extend([
                    key.contract_digest.clone(),
                    key.design_digest.clone(),
                    key.policy_digest.clone(),
                    key.plan_digest.clone(),
                    key.environment_digest.clone(),
                ]);
                for digest in [&key.candidate_digest, &key.oracle_digest] {
                    let snapshot: Snapshot = wire::decode(&self.ledger.read(digest)?)?;
                    snapshot.validate(&self.ledger, true)?;
                    blobs.insert(digest.clone());
                    blobs.extend(
                        snapshot
                            .files
                            .values()
                            .map(|file| file.content.digest.clone()),
                    );
                    blobs.extend(snapshot.dependency_assets);
                }
                results.push(result);
            }
        }
        let operations = self.ledger.operations(run_id)?;
        for operation in &operations {
            blobs.insert(operation.input_digest.clone());
            blobs.extend(operation.output_digest.iter().cloned());
        }
        let mut events = Vec::new();
        {
            let mut query = self
                .ledger
                .connection
                .prepare("SELECT event_digest FROM events WHERE run_id=?1 ORDER BY sequence")?;
            for row in query.query_map([run_id], |row| row.get::<_, String>(0))? {
                let digest = Digest::parse(&row?)?;
                events.push(wire::decode::<serde_json::Value>(
                    &self.ledger.read(&digest)?,
                )?);
                blobs.insert(digest);
            }
        }
        for exception in &setup.exceptions {
            blobs.extend(exception.replacement_evidence.iter().cloned());
        }
        let mut directory = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        // Exclusive creation prevents a concurrent export from being overwritten.
        directory.create(destination)?;
        let mut files = BTreeMap::new();
        let mut write = |name: &str, bytes: &[u8]| -> Result<Digest> {
            let path = destination.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bytes)?;
            let digest = Digest::of(bytes);
            files.insert(name.to_string(), digest.clone());
            Ok(digest)
        };
        let patch_digest = write("patch.json", &wire::canonical(&patch)?)?;
        write("setup.json", &wire::canonical(&setup)?)?;
        write("candidate.json", &wire::canonical(&candidate)?)?;
        write(
            "evidence.json",
            &wire::canonical(&serde_json::json!({
                "run":run,"results":results,"operations":operations,"events":events,
                "budgets":self.ledger.budgets(run_id)?,"approvals":self.ledger.approval_history(run_id)?,
                "infrastructure_recoveries":self.ledger.infrastructure_recoveries(run_id)?,
                "exceptions":setup.exceptions,
            }))?,
        )?;
        for digest in blobs {
            let name = format!("artifacts/{}", digest.hex());
            let path = destination.join(&name);
            std::fs::create_dir_all(destination.join("artifacts"))?;
            self.ledger.copy_to(&digest, std::fs::File::create(path)?)?;
            files.insert(name, digest);
        }
        let manifest = BundleManifest {
            schema_version: 1,
            run_id: run_id.into(),
            revision: run.revision,
            state: run.state,
            candidate_digest: candidate.digest()?,
            patch_digest,
            qualified_acceptance: setup
                .exceptions
                .iter()
                .any(|exception| exception.qualifies_acceptance),
            files,
        };
        std::fs::write(
            destination.join("manifest.json"),
            wire::canonical(&manifest)?,
        )?;
        tx.commit()?;
        Ok(manifest)
    }
}
