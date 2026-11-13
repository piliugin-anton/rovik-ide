//! Reviewed setup catalog metadata and bounded import of protected setup assets.
//!
//! Import registers bytes, never approvals, receipts, or a previous run's verdict.

use super::GitIntegration;
use rovik_harness::{
    contract::Invariant,
    controller::TaskSetup,
    identity::{ArtifactRef, Digest},
    ledger::Ledger,
    plan::{ApprovalRequirement, Phase},
    profile::{Configuration, Language},
    runner,
    scope::Scope,
    snapshot::Snapshot,
    wire, Error, Result,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMPORT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_IMPORT_ASSETS: usize = 16_384;

/// A catalog entry presents reviewed capabilities without exposing storage paths.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupSummary {
    /// Identity of the complete registered setup; a label never selects a capability.
    pub id: String,
    /// Human-readable contract identity.
    pub label: String,
    /// Requested behavior, shown before selecting this setup.
    pub contract_summary: String,
    /// Exact Git baseline required for this registered setup.
    pub base_revision: String,
    /// Every registered check, including advisory iteration checks.
    pub checks: Vec<CheckSummary>,
    /// Human-readable target/configuration names.
    pub configurations: Vec<String>,
    /// Native build roots, languages, and exact configurations.
    pub profiles: Vec<ProfileSummary>,
    /// Behaviors and protected case identities covered by the setup.
    pub invariants: Vec<Invariant>,
    /// Files supplied as independent protected test/driver inputs.
    pub protected_paths: Vec<String>,
    /// Approved write scope, with protected paths taking precedence.
    pub scope: Scope,
    /// Exact subjects still requiring a current authenticated decision for a new run.
    pub approvals: Vec<ApprovalRequirement>,
    /// Separate review requirements that bind the eventual candidate.
    pub candidate_reviews: Vec<String>,
    /// Relevant failing cases required to establish oracle sensitivity.
    pub negative_control_cases: Vec<String>,
}

/// One native check as presented in the setup review.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckSummary {
    /// Stable registered check ID.
    pub id: String,
    /// Registered command identity, never frontend-supplied shell text.
    pub command: String,
    /// Whether this check runs during iteration or final verification.
    pub phase: Phase,
    /// Exact target/feature/configuration binding.
    pub configuration_digest: Digest,
    /// Protected expected native cases.
    pub case_ids: Vec<String>,
    /// Minimum required executed native cases.
    pub minimum_test_count: u64,
    /// Concrete bounded argv rendered by the registered command definition.
    pub argv: Vec<String>,
}

/// One prepared language/build root in the complete verification matrix.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    /// Stable profile identity.
    pub id: String,
    /// Native language pack.
    pub language: Language,
    /// Repository-relative manifest/build directory.
    pub build_root: String,
    /// Explicit targets, features, flags and runtimes.
    pub configurations: Vec<Configuration>,
}

/// Describe a previously validated setup without treating its old evidence as current.
pub fn summarize(setup: &TaskSetup) -> Result<SetupSummary> {
    Ok(SetupSummary {
        id: Digest::of_json(setup)?.to_string(),
        label: setup.contract.id.clone(),
        contract_summary: setup.contract.intent.clone(),
        base_revision: setup.base.base_revision.clone(),
        checks: setup
            .plan
            .checks
            .iter()
            .map(|check| {
                Ok(CheckSummary {
                    id: check.check_id.clone(),
                    command: check.command_id.clone(),
                    phase: check.phase,
                    configuration_digest: check.configuration_digest.clone(),
                    case_ids: check.expected_case_ids.clone(),
                    minimum_test_count: check.minimum_test_count,
                    argv: setup
                        .registry
                        .get(&check.command_id)?
                        .render(&check.validated_parameters)?,
                })
            })
            .collect::<Result<_>>()?,
        configurations: setup
            .profiles
            .iter()
            .flat_map(|profile| {
                profile.configurations.iter().map(|configuration| {
                    format!(
                        "{} / {} ({})",
                        profile.id, configuration.id, configuration.target
                    )
                })
            })
            .collect(),
        profiles: setup
            .profiles
            .iter()
            .map(|profile| ProfileSummary {
                id: profile.id.clone(),
                language: profile.language,
                build_root: profile.build_root.clone(),
                configurations: profile.configurations.clone(),
            })
            .collect(),
        invariants: setup.contract.invariants.clone(),
        protected_paths: setup.oracle.files.keys().cloned().collect(),
        scope: setup.contract.scope.clone(),
        approvals: setup.approval_requirements(None)?,
        candidate_reviews: setup.plan.review_requirements.clone(),
        negative_control_cases: setup.negative_control_cases.clone(),
    })
}

fn ensure(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::new(message.into()))
    }
}

fn read_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Setup import requires regular files, not symbolic links",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure(
            metadata.nlink() == 1,
            "Setup import rejects externally linked files",
        )?;
    }
    ensure(
        metadata.len() <= limit,
        "Setup import file exceeds its size limit",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    let after = fs::symlink_metadata(path)?;
    ensure(
        bytes.len() as u64 == metadata.len()
            && after.is_file()
            && !after.file_type().is_symlink()
            && after.len() == metadata.len()
            && after.modified()? == metadata.modified()?,
        "Setup import changed while being read; select the bundle again",
    )?;
    Ok(bytes)
}

/// Import a selected setup.json with sibling artifacts/<sha256> files.
/// Only required source/environment/evidence bytes are registered. Imported
/// evidence.json, approvals, receipts and verdicts are never read or installed.
/// The repository label is rebound to the IDE's trusted project identity; the
/// baseline must still exactly match that project's committed HEAD.
pub fn import_setup(
    ledger: &Ledger,
    setup_path: &Path,
    project_root: &Path,
    repository_id: &str,
) -> Result<TaskSetup> {
    let baseline = GitIntegration::new(repository_id.into(), project_root)?.snapshot(ledger)?;
    import_setup_with_baseline(ledger, setup_path, repository_id, &baseline)
}

/// Import against the IDE's independently observed baseline, including the
/// server-owned parent reference of a synchronized cloud shadow checkout.
/// Callers must obtain `baseline` from their trusted Git integration; it is never
/// accepted from an import document or frontend request.
pub fn import_setup_with_baseline(
    ledger: &Ledger,
    setup_path: &Path,
    repository_id: &str,
    baseline: &Snapshot,
) -> Result<TaskSetup> {
    baseline.validate(ledger, true)?;
    ensure(
        setup_path.is_absolute(),
        "Select an absolute setup file path",
    )?;
    let mut setup: TaskSetup = wire::decode(&read_file(setup_path, wire::MAX_JSON_BYTES as u64)?)?;
    let parent = setup_path
        .parent()
        .ok_or_else(|| Error::new("Setup file has no parent".into()))?;
    let assets = parent.join("artifacts");
    let metadata = fs::symlink_metadata(&assets)?;
    ensure(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Setup import needs a regular sibling artifacts directory",
    )?;
    let mut references = BTreeMap::<Digest, Vec<ArtifactRef>>::new();
    let mut opaque = BTreeSet::from([setup.environment.toolchain_digest.clone()]);
    for snapshot in [
        Some(&setup.base),
        Some(&setup.oracle),
        Some(&setup.dependencies),
        Some(&setup.negative_control),
        setup.initial_candidate.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        for file in snapshot.files.values() {
            ensure(
                file.content.kind == "source",
                "Setup source assets must have source references",
            )?;
            let formats = references.entry(file.content.digest.clone()).or_default();
            ensure(
                formats
                    .iter()
                    .all(|previous| previous.byte_count == file.content.byte_count),
                "Setup has inconsistent lengths for the same source bytes",
            )?;
            if !formats.contains(&file.content) {
                formats.push(file.content.clone());
            }
        }
        opaque.extend(snapshot.dependency_assets.iter().cloned());
    }
    for exception in &setup.exceptions {
        opaque.extend(exception.replacement_evidence.iter().cloned());
    }
    for digest in references.keys() {
        opaque.remove(digest);
    }
    ensure(
        references.len().saturating_add(opaque.len()) <= MAX_IMPORT_ASSETS,
        "Setup import contains too many assets",
    )?;
    let mut total = 0_u64;
    for digest in references.keys().chain(&opaque) {
        let path = assets.join(digest.hex());
        let length = fs::symlink_metadata(&path)?.len();
        total = total
            .checked_add(length)
            .ok_or_else(|| Error::new("Setup asset size overflow".into()))?;
        ensure(
            length <= MAX_ASSET_BYTES && total <= MAX_IMPORT_BYTES,
            "Setup import exceeds the protected asset budget",
        )?;
    }
    // Recheck the aggregate using the bytes actually read: files may have grown
    // between the initial size inventory and opening each bounded input.
    let mut imported_bytes = 0_u64;
    for (digest, formats) in &references {
        let bytes = read_file(&assets.join(digest.hex()), MAX_ASSET_BYTES)?;
        imported_bytes += bytes.len() as u64;
        ensure(
            imported_bytes <= MAX_IMPORT_BYTES,
            "Setup import exceeds the protected asset budget",
        )?;
        ensure(
            Digest::of(&bytes) == *digest
                && formats
                    .iter()
                    .all(|reference| bytes.len() as u64 == reference.byte_count),
            "Setup asset digest or length mismatch",
        )?;
        for reference in formats {
            ensure(
                ledger.store("source", &reference.media_type, &bytes, runner::now_ms()?)?
                    == *reference,
                "Setup source registration differs from its reference",
            )?;
        }
    }
    for digest in opaque {
        let bytes = read_file(&assets.join(digest.hex()), MAX_ASSET_BYTES)?;
        imported_bytes += bytes.len() as u64;
        ensure(
            imported_bytes <= MAX_IMPORT_BYTES,
            "Setup import exceeds the protected asset budget",
        )?;
        ensure(
            Digest::of(&bytes) == digest,
            "Setup supporting asset digest mismatch",
        )?;
        ledger.store(
            "environment",
            "application/octet-stream",
            &bytes,
            runner::now_ms()?,
        )?;
    }
    setup.repository_id = repository_id.into();
    ensure(
        baseline == &setup.base,
        "This setup targets another baseline; prepare it against the project's current committed revision",
    )?;
    setup.validate(ledger)?;
    Ok(setup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_harness::fixtures;

    fn project_bundle() -> (tempfile::TempDir, TaskSetup) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        let repository = git2::Repository::init(&project).unwrap();
        let path = "implementations/quota/lib.rs";
        fs::create_dir_all(project.join("implementations/quota")).unwrap();
        fs::write(
            project.join(path),
            fixtures::QUOTA_RUST
                .replace("requested > self.remaining", "requested >= self.remaining"),
        )
        .unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Harness test", "harness@example.invalid").unwrap();
        let revision = repository
            .commit(Some("HEAD"), &signature, &signature, "baseline", &tree, &[])
            .unwrap()
            .to_string();
        let ledger = Ledger::open(&root.join("source-state")).unwrap();
        let setup =
            fixtures::quota_rust(&ledger, "prepared", Digest::of(b"image"), &revision).unwrap();
        setup.validate(&ledger).unwrap();
        let bundle = root.join("bundle");
        fs::create_dir_all(bundle.join("artifacts")).unwrap();
        fs::write(bundle.join("setup.json"), wire::canonical(&setup).unwrap()).unwrap();
        for entry in fs::read_dir(ledger.root().join("artifacts")).unwrap() {
            let entry = entry.unwrap();
            fs::copy(
                entry.path(),
                bundle.join("artifacts").join(entry.file_name()),
            )
            .unwrap();
        }
        (directory, setup)
    }

    #[test]
    fn imported_assets_create_a_reviewable_catalog_entry_without_importing_run_authority() {
        let (directory, original) = project_bundle();
        let root = directory.path();
        // This file deliberately resembles a portable review export. Neither its
        // claimed verdict nor its approval record can populate the new ledger.
        fs::write(
            root.join("bundle/evidence.json"),
            br#"{"verdict":"ready","approvals":[{"actor_id":"admin"}]}"#,
        )
        .unwrap();
        let ledger = Ledger::open(&root.join("ide-state")).unwrap();
        let setup = import_setup(
            &ledger,
            &root.join("bundle/setup.json"),
            &root.join("project"),
            "ide-project",
        )
        .unwrap();
        assert_eq!(setup.repository_id, "ide-project");
        assert_eq!(setup.base, original.base);
        assert_eq!(setup.authority.actors, original.authority.actors);
        assert!(ledger.run(&setup.run_id).is_err());
        assert!(!ledger.root().join("evidence.json").exists());
        let summary = summarize(&setup).unwrap();
        assert_eq!(summary.id, Digest::of_json(&setup).unwrap().to_string());
        assert_eq!(summary.invariants, setup.contract.invariants);
        assert_eq!(summary.protected_paths.len(), setup.oracle.files.len());
        assert_eq!(
            summary.checks[0].case_ids,
            setup.plan.checks[0].expected_case_ids
        );
        assert!(summary
            .approvals
            .iter()
            .any(|approval| approval.approval_type == "oracle"));
        assert!(serde_json::to_value(summary)
            .unwrap()
            .get("contractSummary")
            .is_some());
    }

    #[test]
    fn missing_and_modified_protected_assets_cannot_be_registered_as_a_setup() {
        let (directory, setup) = project_bundle();
        let root = directory.path();
        let reference = &setup.oracle.files.values().next().unwrap().content;
        let path = root.join("bundle/artifacts").join(reference.digest.hex());
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"different protected test").unwrap();
        let ledger = Ledger::open(&root.join("ide-state")).unwrap();
        assert!(import_setup(
            &ledger,
            &root.join("bundle/setup.json"),
            &root.join("project"),
            "ide-project"
        )
        .unwrap_err()
        .message
        .contains("digest"));
        fs::write(&path, original).unwrap();
        fs::remove_file(path).unwrap();
        assert!(import_setup(
            &ledger,
            &root.join("bundle/setup.json"),
            &root.join("project"),
            "ide-project"
        )
        .is_err());
        assert!(ledger.run(&setup.run_id).is_err());
    }

    #[test]
    fn identical_bytes_keep_each_explicit_source_media_type_after_import() {
        let (directory, mut setup) = project_bundle();
        let root = directory.path();
        let mut alternate = setup.base.files.values().next().unwrap().clone();
        alternate.content.media_type = "text/plain".into();
        setup
            .oracle
            .files
            .insert("source-review.txt".into(), alternate.clone());
        setup.plan.oracle_digest = setup.oracle.digest().unwrap();
        fs::write(
            root.join("bundle/setup.json"),
            wire::canonical(&setup).unwrap(),
        )
        .unwrap();
        let ledger = Ledger::open(&root.join("ide-state")).unwrap();
        let imported = import_setup(
            &ledger,
            &root.join("bundle/setup.json"),
            &root.join("project"),
            "ide-project",
        )
        .unwrap();
        ledger.verify_reference(&alternate.content).unwrap();
        ledger
            .verify_reference(&imported.base.files.values().next().unwrap().content)
            .unwrap();
    }

    #[test]
    fn a_prepared_setup_for_another_committed_tree_cannot_be_attached_to_this_project() {
        let (directory, mut setup) = project_bundle();
        let root = directory.path();
        setup.base.base_revision = "another-revision".into();
        fs::write(
            root.join("bundle/setup.json"),
            wire::canonical(&setup).unwrap(),
        )
        .unwrap();
        let ledger = Ledger::open(&root.join("ide-state")).unwrap();
        let error = import_setup(
            &ledger,
            &root.join("bundle/setup.json"),
            &root.join("project"),
            "ide-project",
        )
        .unwrap_err();
        assert!(error.message.contains("another baseline"));
    }

    #[test]
    fn a_cloud_shadow_uses_its_trusted_parent_baseline_instead_of_its_synthetic_head() {
        let (directory, original) = project_bundle();
        let root = directory.path();
        let project = root.join("project");
        let ledger = Ledger::open(&root.join("ide-state")).unwrap();
        let repository = git2::Repository::open(&project).unwrap();
        let parent = repository.head().unwrap().peel_to_commit().unwrap();
        repository
            .reference(
                "refs/heads/desktop-base",
                parent.id(),
                false,
                "trusted sync parent",
            )
            .unwrap();
        fs::write(
            project.join("implementations/quota/lib.rs"),
            fixtures::QUOTA_RUST,
        )
        .unwrap();
        let mut index = repository.index().unwrap();
        index
            .add_path(Path::new("implementations/quota/lib.rs"))
            .unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Harness test", "harness@example.invalid").unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "synchronized dirty candidate",
                &tree,
                &[&parent],
            )
            .unwrap();
        let setup_path = root.join("bundle/setup.json");
        assert!(import_setup(&ledger, &setup_path, &project, "ide-project").is_err());
        let trusted_baseline = GitIntegration::for_reference(
            "ide-project".into(),
            &project,
            "refs/heads/desktop-base",
        )
        .unwrap()
        .snapshot(&ledger)
        .unwrap();
        let imported =
            import_setup_with_baseline(&ledger, &setup_path, "ide-project", &trusted_baseline)
                .unwrap();
        assert_eq!(imported.base, original.base);
        assert_ne!(
            imported.base.base_revision,
            repository.head().unwrap().target().unwrap().to_string()
        );
        let mut mismatched = trusted_baseline;
        mismatched.files.clear();
        assert!(
            import_setup_with_baseline(&ledger, &setup_path, "ide-project", &mismatched).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_bundle_cannot_read_protected_assets_through_symlinks_or_external_hardlinks() {
        use std::os::unix::fs::symlink;
        let (directory, setup) = project_bundle();
        let root = directory.path();
        let reference = &setup.oracle.files.values().next().unwrap().content;
        let path = root.join("bundle/artifacts").join(reference.digest.hex());
        let outside = root.join("outside");
        fs::rename(&path, &outside).unwrap();
        symlink(&outside, &path).unwrap();
        let ledger = Ledger::open(&root.join("ide-state")).unwrap();
        assert!(import_setup(
            &ledger,
            &root.join("bundle/setup.json"),
            &root.join("project"),
            "ide-project"
        )
        .is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&outside, &path).unwrap();
        assert!(import_setup(
            &ledger,
            &root.join("bundle/setup.json"),
            &root.join("project"),
            "ide-project"
        )
        .is_err());
    }
}
