//! Automatic preparation has a private writable workspace and no approval authority.
//! Only an exact, current review can promote its immutable output into implementation.

use super::*;
use crate::restricted_files::Denied;
use rovik_harness::{
    controller::WorkerLimits, preparation::PreparedEnvironment, snapshot::FileEntry,
};
use rovik_protocol::{agent::ModelProfile, harness::ImplementationProvider};
use std::{fs, io::Read};

const DRAFTS: &str = ".rovik-preparation";
const STAGED_FILE_LIMIT: u64 = 16 * 1024 * 1024;
const PREPARATION_LIFETIME: u64 = 3_600_000;

/// Trusted context for the preparation-only agent; never accepted from message metadata.
#[derive(Clone, Debug)]
pub struct PreparationContext {
    /// Service-issued source copy; ordinary project writes never target this directory.
    pub workspace_root: PathBuf,
    /// Instructions and exact host facts for preparing the submission artifact.
    pub instructions: String,
    /// Deadline captured before preparation begins.
    pub deadline_ms: u64,
}

/// A staged document together with the current trusted project restrictions.
pub struct PreparationSubmission {
    /// Document inside the service-issued preparation workspace.
    pub input_path: String,
    /// Read/write exclusions resolved by the runtime from the original project.
    pub denied: Denied,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Runtime {
    engine: Engine,
    assets: PreparedEnvironment,
    dependencies: Option<PathBuf>,
    #[serde(default)]
    dependency_manifests: BTreeSet<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreparationRecord {
    id: String,
    dialog_id: String,
    execution_root: PathBuf,
    baseline_reference: Option<String>,
    tracked_paths: Option<BTreeSet<String>>,
    plan: plan::SavedPlan,
    snapshot_digest: Digest,
    baseline_digest: Digest,
    untracked: BTreeSet<String>,
    #[serde(default)]
    automatic_untracked: bool,
    provider: plan::ProviderBinding,
    worker: WorkerLimits,
    budgets: BTreeMap<String, u64>,
    deadline_ms: u64,
    status: String,
    error: Option<String>,
    runtime: Option<Runtime>,
    #[serde(default)]
    denied_read: Vec<String>,
    #[serde(default)]
    denied_write: Vec<String>,
    review: Option<Value>,
    setup_id: Option<Digest>,
    implementation: Option<Digest>,
    run_id: Option<String>,
    #[serde(default)]
    promoted: bool,
}

/// One explicitly staged file. Its source stays beneath the issued workspace.
#[derive(Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename = "PreparationStagedFile")]
struct StagedFile {
    path: String,
    source: String,
}

/// Machine-authored preparation proposal, not an approval or a verdict.
#[derive(Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename = "PreparationRecipe")]
struct Submission {
    setup: crate::harness::ide_prepare::WorkspaceSetupInput,
    #[serde(default)]
    #[ts(as = "Option<_>", optional)]
    oracle_files: Vec<StagedFile>,
    /// Real source edits exercised by the controller's negative-control check.
    negative_control_files: Vec<StagedFile>,
}

#[path = "ide_preparation_types.rs"]
mod types;
pub use types::typescript_preparation_contract;

fn pattern(path: &str) -> String {
    path.strip_suffix('/')
        .map(|p| format!("{p}/**"))
        .unwrap_or_else(|| path.into())
}

fn record_json(record: &PreparationRecord) -> Result<String> {
    serde_json::to_string(record).map_err(|e| Error::new(e.to_string()))
}

impl IdeProject {
    fn preparation_workspace(&self, id: &str) -> Result<PathBuf> {
        let uuid = id
            .strip_prefix("prep-")
            .ok_or_else(|| Error::new("Invalid preparation identity.".into()))?;
        uuid::Uuid::parse_str(uuid)
            .map_err(|_| Error::new("Invalid preparation identity.".into()))?;
        Ok(self.state.join("preparations").join(id).join("workspace"))
    }

    fn load_preparation(&self, id: &str) -> Result<PreparationRecord> {
        self.preparation_workspace(id)?;
        let text: String = self
            .database()?
            .query_row("SELECT value FROM preparations WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .map_err(|_| {
                Error::new("This preparation is unavailable. Prepare the saved plan again.".into())
            })?;
        wire::decode(text.as_bytes())
    }

    fn save_preparation(&self, record: &PreparationRecord) -> Result<()> {
        self.database()?.execute("INSERT INTO preparations(id,value) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET value=excluded.value", params![record.id, record_json(record)?])?;
        Ok(())
    }

    fn save_active_preparation(&self, record: &PreparationRecord) -> Result<()> {
        let updated=self.database()?.execute("UPDATE preparations SET value=?2 WHERE id=?1 AND json_extract(value,'$.status') IN ('preparing','blocked','ready') AND (json_extract(value,'$.runId') IS NULL OR json_extract(value,'$.runId')=json_extract(?2,'$.runId'))",params![record.id,record_json(record)?])?;
        check(updated == 1, "Preparation was cancelled.")
    }

    fn preparation_project(&self, record: &PreparationRecord) -> Result<Self> {
        check(
            self.root == record.execution_root,
            "Return to the project and host that own this preparation.",
        )?;
        let mut project = self.clone();
        project.baseline_reference = record.baseline_reference.clone();
        project.tracked_paths = record.tracked_paths.clone();
        Ok(project)
    }

    fn restore_preparation_source(
        &self,
        record: &PreparationRecord,
        workspace: &Path,
    ) -> Result<()> {
        let metadata = fs::symlink_metadata(workspace)?;
        check(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Preparation workspace must remain a private directory.",
        )?;
        let ledger = self.ledger()?;
        let source: Snapshot = wire::decode(&ledger.read(&record.snapshot_digest)?)?;
        rovik_harness::scope::validate_tree_paths(source.files.keys().map(String::as_str), true)?;
        for (path, entry) in &source.files {
            if crate::restricted_files::looks_secret(path)
                || record
                    .denied_read
                    .iter()
                    .any(|pattern| rovik_harness::scope::matches(pattern, path))
            {
                continue;
            }
            // Only captured source can be restored. Agent drafts are never part
            // of this snapshot, and existing source files remain untouched.
            let parts = path.split('/').collect::<Vec<_>>();
            let mut parent = workspace.to_path_buf();
            for part in &parts[..parts.len() - 1] {
                parent.push(part);
                match fs::create_dir(&parent) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                let metadata = fs::symlink_metadata(&parent)?;
                check(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "Preparation source cannot traverse a symlink or non-directory.",
                )?;
            }
            let destination = workspace.join(path);
            match fs::symlink_metadata(&destination) {
                Ok(metadata) => {
                    check(
                        metadata.is_file() && !metadata.file_type().is_symlink(),
                        "Preparation source must remain a regular file.",
                    )?;
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let mut restored = tempfile::NamedTempFile::new_in(&parent)?;
            ledger.copy_to(&entry.content.digest, &mut restored)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                restored
                    .as_file()
                    .set_permissions(fs::Permissions::from_mode(if entry.mode == 0o100755 {
                        0o700
                    } else {
                        0o600
                    }))?;
            }
            restored
                .persist_noclobber(destination)
                .map_err(|error| error.error)?;
        }
        Ok(())
    }

    fn require_preparation_metadata(&self, record: &PreparationRecord) -> Result<Self> {
        check(
            record.deadline_ms > runner::now_ms()?,
            "The preparation expired. Prepare this plan again.",
        )?;
        let project = self.preparation_project(record)?;
        if record.automatic_untracked {
            let now = project.untracked_inputs()?;
            check(
                now == record.untracked,
                "The project's untracked files changed. Prepare the current project again.",
            )?;
        }
        check(
            project.read_plan(&record.plan.path)?.digest == record.plan.digest,
            "The saved plan changed. Prepare its current version again.",
        )?;
        Ok(project)
    }

    fn invalidate_preparation(&self, record: &PreparationRecord, error: &Error) -> Result<()> {
        // Storage failure is not evidence that the source or plan changed.
        if matches!(
            error.category,
            rovik_harness::FailureCategory::Infrastructure
                | rovik_harness::FailureCategory::Internal
        ) {
            return Ok(());
        }
        self.database()?.execute("UPDATE preparations SET value=json_set(value,'$.status','stale','$.error',?2) WHERE id=?1 AND json_extract(value,'$.status') IN ('preparing','ready','blocked') AND json_extract(value,'$.runId') IS NULL", params![record.id, error.to_string()])?;
        Ok(())
    }

    fn require_fresh_preparation(&self, record: &PreparationRecord) -> Result<()> {
        let result = self.check_preparation_source(record);
        if let Err(error) = &result {
            self.invalidate_preparation(record, error)?;
        }
        result
    }

    fn check_preparation_source(&self, record: &PreparationRecord) -> Result<()> {
        let project = self.require_preparation_metadata(record)?;
        check(
            project
                .observe_snapshot(&record.untracked, None)?
                .digest()?
                == record.snapshot_digest,
            "The project changed during preparation. Prepare the current changes again.",
        )?;
        check(
            project
                .integration()?
                .snapshot(&project.ledger()?)?
                .digest()?
                == record.baseline_digest,
            "The project revision changed during preparation. Prepare this plan again.",
        )
    }

    fn preparation_view(&self, record: &PreparationRecord) -> Value {
        json!({"preparationId":record.id,"status":record.status,"plan":record.plan,
            "snapshotDigest":record.snapshot_digest,"untracked":record.untracked,"providerBinding":record.provider,
            "review":record.review,"error":record.error,"runId":if record.promoted {record.run_id.as_ref()} else {None}})
    }

    pub(super) fn begin_preparation(
        &self,
        dialog_id: &str,
        path: &str,
        provider: ImplementationProvider,
        saved_buffers: bool,
        untracked: Vec<String>,
    ) -> Result<Value> {
        check(
            saved_buffers,
            "Save the project buffers before preparing this plan.",
        )?;
        check(
            !dialog_id.trim().is_empty() && dialog_id.len() <= 256,
            "Select the plan's chat before preparation.",
        )?;
        let binding = plan::binding(&provider)?;
        let plan = self.read_plan(path)?;
        let automatic_untracked = untracked.is_empty();
        let untracked: BTreeSet<String> = if automatic_untracked {
            self.untracked_inputs()?
        } else {
            untracked.into_iter().collect()
        };
        let snapshot = self.capture(&untracked)?;
        check(!snapshot.files.keys().any(|p| p == DRAFTS || p.starts_with(&format!("{DRAFTS}/"))), "The project uses Rovik's private preparation directory name. Rename that directory before preparing.")?;
        let ledger = self.ledger()?;
        let baseline_digest = self.integration()?.snapshot(&ledger)?.digest()?;
        let mut worker = WorkerLimits {
            model_id: binding.model.clone(),
            context_bytes: 65_536,
            output_tokens: 8192,
            timeout_ms: 180_000,
            token_reservation: 8192,
            cost_reservation: 0,
            format_repairs: 1,
            infrastructure_retries_per_check: 1,
        };
        let budgets = BTreeMap::from([
            ("model_calls".into(), 16),
            ("tokens".into(), 2_000_000),
            ("cost_microunits".into(), 10_000_000),
            ("checks".into(), 128),
        ]);
        plan::reserve_provider_limits(&mut worker, &binding.rates, &budgets)?;
        let record = PreparationRecord {
            id: format!("prep-{}", uuid::Uuid::new_v4()),
            dialog_id: dialog_id.into(),
            execution_root: self.root.clone(),
            baseline_reference: self.baseline_reference.clone(),
            tracked_paths: self.tracked_paths.clone(),
            plan,
            snapshot_digest: snapshot.digest()?,
            baseline_digest,
            untracked,
            automatic_untracked,
            provider: binding,
            worker,
            budgets,
            deadline_ms: runner::now_ms()?
                .checked_add(PREPARATION_LIFETIME)
                .ok_or_else(|| Error::new("Preparation deadline overflow.".into()))?,
            status: "preparing".into(),
            error: None,
            runtime: None,
            denied_read: vec![],
            denied_write: vec![],
            review: None,
            setup_id: None,
            implementation: None,
            run_id: None,
            promoted: false,
        };
        let workspace = self.preparation_workspace(&record.id)?;
        fs::create_dir_all(
            workspace
                .parent()
                .ok_or_else(|| Error::new("Preparation directory has no parent.".into()))?,
        )?;
        let mut visible = snapshot;
        visible
            .files
            .retain(|path, _| !crate::restricted_files::looks_secret(path));
        visible.materialize(&ledger, &workspace)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700))?;
            for (path, entry) in &visible.files {
                fs::set_permissions(
                    workspace.join(path),
                    fs::Permissions::from_mode(if entry.mode == 0o100755 { 0o700 } else { 0o600 }),
                )?;
            }
        }
        fs::create_dir(workspace.join(DRAFTS))?;
        fs::create_dir(workspace.join(DRAFTS).join("dependencies"))?;
        self.save_preparation(&record)?;
        Ok(self.preparation_view(&record))
    }

    pub(super) fn preparation_status(&self, id: &str) -> Result<Value> {
        let mut record = self.load_preparation(id)?;
        if matches!(record.status.as_str(), "preparing" | "ready" | "blocked")
            && record.run_id.is_none()
        {
            if let Err(error) = self.require_preparation_metadata(&record) {
                // A storage failure says nothing about the source's freshness.
                // Keep the issued drafts and review available for a later retry.
                if matches!(
                    error.category,
                    rovik_harness::FailureCategory::Infrastructure
                        | rovik_harness::FailureCategory::Internal
                ) {
                    return Err(error);
                }
                self.invalidate_preparation(&record, &error)?;
                record = self.load_preparation(id)?;
            }
        }
        Ok(self.preparation_view(&record))
    }

    pub(super) fn cancel_preparation(&self, id: &str) -> Result<Value> {
        let mut record = self.load_preparation(id)?;
        check(
            record.run_id.is_none(),
            "Implementation already started; stop its run instead.",
        )?;
        let updated=self.database()?.execute("UPDATE preparations SET value=json_set(value,'$.status','cancelled','$.error',NULL) WHERE id=?1 AND json_extract(value,'$.runId') IS NULL",[id])?;
        check(
            updated == 1,
            "Implementation already started; stop its run instead.",
        )?;
        record = self.load_preparation(id)?;
        if let Some(cancel) = active_runs()
            .lock()
            .map_err(|_| Error::new("Preparation cancellation is unavailable.".into()))?
            .get(&(self.state.clone(), id.into()))
        {
            cancel.cancel();
        }
        Ok(self.preparation_view(&record))
    }

    pub(super) fn require_setup_baseline(&self, setup: &TaskSetup) -> Result<()> {
        use rusqlite::OptionalExtension;
        let trusted: Option<String> = self
            .database()?
            .query_row(
                "SELECT baseline_digest FROM preparation_baselines WHERE setup_id=?1",
                [Digest::of_json(setup)?.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let expected = trusted
            .map(|digest| Digest::parse(&digest))
            .transpose()?
            .unwrap_or(setup.base.digest()?);
        check(
            self.integration()?.snapshot(&self.ledger()?)?.digest()? == expected,
            "The destination revision changed. Prepare this plan again.",
        )
    }
}

fn restrictions(record: &mut PreparationRecord, denied: Denied) -> Result<()> {
    record.denied_read = denied.read.iter().map(|p| pattern(p)).collect();
    record.denied_write = denied.write.iter().map(|p| pattern(p)).collect();
    record.denied_read.sort();
    record.denied_read.dedup();
    record.denied_write.sort();
    record.denied_write.dedup();
    check(
        !record
            .denied_read
            .iter()
            .any(|p| rovik_harness::scope::matches(p, &record.plan.path)),
        "The selected plan is read-restricted. Allow reading it before preparation.",
    )?;
    for value in record.denied_read.iter().chain(&record.denied_write) {
        rovik_harness::scope::validate_pattern(value)?;
    }
    Ok(())
}

fn bound_project(
    state_root: &Path,
    root: &Path,
    project_id: &str,
    id: &str,
    dialog: &str,
    profile: &ModelProfile,
) -> Result<(IdeProject, PreparationRecord)> {
    let project = IdeProject::open_for_project(state_root, root, project_id)?;
    let record = project.load_preparation(id)?;
    check(
        record.dialog_id == dialog,
        "This preparation belongs to a different chat.",
    )?;
    check(
        record.provider
            == plan::binding(&ImplementationProvider {
                profile: profile.clone(),
                rates: record.provider.rates.clone(),
            })?,
        "Use the model selected when preparation started.",
    )?;
    check(
        record.status == "preparing" || record.status == "blocked",
        "This preparation is no longer accepting agent work.",
    )?;
    project.require_fresh_preparation(&record)?;
    Ok((project.preparation_project(&record)?, record))
}

/// Resolve a preparation from its original project and chat, then bootstrap trusted tools.
/// The returned directory cannot be chosen by an agent or frontend message.
pub async fn preparation_context(
    state_root: PathBuf,
    project_root: PathBuf,
    project_id: String,
    preparation_id: String,
    dialog_id: String,
    profile: ModelProfile,
    denied: Denied,
) -> Result<PreparationContext> {
    crate::rt::spawn_blocking(move || {
        let (project, mut record) = bound_project(
            &state_root,
            &project_root,
            &project_id,
            &preparation_id,
            &dialog_id,
            &profile,
        )?;
        let active = ActivePreparation::begin(&project, &record.id)?;
        restrictions(&mut record, denied)?;
        project.save_active_preparation(&record)?;
        if record.runtime.is_none() {
            let built: Result<_> = (|| {
                check(
                    cfg!(target_os = "linux"),
                    "Choose a connected Linux host for isolated verification.",
                )?;
                let candidates = [EngineKind::Podman, EngineKind::Docker]
                    .into_iter()
                    .filter_map(|kind| {
                        crate::process::find_on_path(if kind == EngineKind::Docker {
                            "docker"
                        } else {
                            "podman"
                        })
                        .map(|path| Engine {
                            kind,
                            executable: path.to_string_lossy().into_owned(),
                        })
                    })
                    .collect();
                let engine = select_preparation_engine(
                    project.configuration()?.map(|c| c.engine),
                    candidates,
                    &active.cancel,
                    |engine| project.backend(engine)?.validate_host(&active.cancel),
                )?;
                let environment = project.backend(&engine)?.prepare_environment(
                    &project.state.join("preparation-runtime"),
                    &active.cancel,
                )?;
                Ok((engine, environment))
            })();
            let current = project.load_preparation(&record.id)?;
            check(current.status != "cancelled", "Preparation was cancelled.")?;
            match built {
                Ok((engine, environment)) => {
                    record.runtime = Some(Runtime {
                        engine,
                        assets: environment,
                        dependencies: None,
                        dependency_manifests: BTreeSet::new(),
                    })
                }
                Err(error) => {
                    record.status = "blocked".into();
                    record.error = Some(error.to_string());
                    project.save_active_preparation(&record)?;
                    return Err(error);
                }
            }
        }
        project.require_fresh_preparation(&record)?;
        record.status = "preparing".into();
        record.error = None;
        project.save_active_preparation(&record)?;
        let workspace = project.preparation_workspace(&record.id)?;
        project.restore_preparation_source(&record, &workspace)?;
        let instructions = write_template(&project, &record, &workspace)?;
        Ok(PreparationContext {
            workspace_root: workspace,
            instructions,
            deadline_ms: record.deadline_ms,
        })
    })
    .await
    .map_err(|e| Error::new(format!("Preparation task failed: {e}")))?
}

fn select_preparation_engine(
    configured: Option<Engine>,
    discovered: Vec<Engine>,
    cancel: &Cancellation,
    mut qualify: impl FnMut(&Engine) -> Result<()>,
) -> Result<Engine> {
    // A reviewed engine is part of the execution identity. Only automatic first
    // use may choose another engine when an installed client cannot isolate work.
    check(!cancel.is_cancelled(), "Preparation was cancelled.")?;
    if let Some(engine) = configured {
        qualify(&engine)?;
        check(!cancel.is_cancelled(), "Preparation was cancelled.")?;
        return Ok(engine);
    }
    let mut failures = Vec::new();
    for engine in discovered {
        check(!cancel.is_cancelled(), "Preparation was cancelled.")?;
        match qualify(&engine) {
            Ok(()) => {
                check(!cancel.is_cancelled(), "Preparation was cancelled.")?;
                return Ok(engine);
            }
            Err(error) => failures.push(format!("{:?}: {error}", engine.kind)),
        }
    }
    check(!cancel.is_cancelled(), "Preparation was cancelled.")?;
    Err(Error::new(format!(
        "Start a local Docker engine or configure delegated rootless Podman on this execution host, then retry preparation. {}",
        failures.join("; ")
    )))
}

fn submission_example(template: &Value, facts: &BTreeMap<String, String>) -> Result<String> {
    let mut example: Value = wire::decode(include_bytes!("preparation-submission-example.json"))?;
    for field in [
        "environment",
        "worker",
        "budgets",
        "durationMs",
        "protectedPaths",
    ] {
        example["setup"][field] = template["setup"][field].clone();
    }
    for field in ["node_version", "npm_version"] {
        let value = facts.get(field).ok_or_else(|| {
            Error::new(format!("Prepared runtime is missing the measured {field}."))
        })?;
        example["setup"]["profiles"][0]["facts"][field] = json!(value);
    }
    let bytes = serde_json::to_vec_pretty(&example).map_err(|e| Error::new(e.to_string()))?;
    // Guidance must obey the same closed schema as actual submissions.
    wire::decode::<Submission>(&bytes)?;
    String::from_utf8(bytes).map_err(|e| Error::new(e.to_string()))
}

fn write_template(
    project: &IdeProject,
    record: &PreparationRecord,
    workspace: &Path,
) -> Result<String> {
    let runtime = record
        .runtime
        .as_ref()
        .ok_or_else(|| Error::new("Preparation runtime is unavailable.".into()))?;
    let ledger = project.ledger()?;
    let mut source: Snapshot = wire::decode(&ledger.read(&record.snapshot_digest)?)?;
    source.files.retain(|path, _| {
        !record
            .denied_read
            .iter()
            .any(|pattern| rovik_harness::scope::matches(pattern, path))
    });
    let inventory = rovik_harness::onboarding::inspect(&ledger, &source)?;
    let template = json!({"setup":{"name":"Plan implementation","intent":record.plan.content,"boundedContext":"","writePaths":[],"protectedPaths":[record.plan.path],"profiles":[],"excludedBuildRoots":[],"requirements":[],"design":{"components":{},"existingCode":[],"publicApiChanges":[],"resourceContracts":[],"errorContracts":[],"concurrencyContract":"","mechanismPressure":"","mechanismAlternatives":[],"mechanismCost":""},"environment":{"imageDigest":runtime.assets.image_digest,"engine":runtime.engine.kind,"toolchainManifestPath":runtime.assets.toolchain_manifest,"dependencyDirectory":format!("{DRAFTS}/dependencies")},"negativeControlRevision":"","negativeControlCases":[],"baselineFailures":[],"worker":record.worker,"budgets":record.budgets,"durationMs":record.deadline_ms.saturating_sub(runner::now_ms()?)},"oracleFiles":[],"negativeControlFiles":[]});
    let template_path = workspace.join(DRAFTS).join("template.json");
    if !template_path.exists() {
        fs::write(
            &template_path,
            serde_json::to_vec_pretty(&template).map_err(|e| Error::new(e.to_string()))?,
        )?;
    }
    let example = submission_example(&template, &runtime.assets.facts)?;
    // Refresh service-owned guidance on retry while preserving the agent's drafts.
    fs::write(workspace.join(DRAFTS).join("example.json"), &example)?;
    fs::write(
        workspace.join(DRAFTS).join("schema.ts"),
        typescript_preparation_contract()?,
    )?;
    Ok(format!(
        include_str!("preparation-workspace-guide.txt"),
        serde_json::to_string(&runtime.assets.facts).map_err(|e| Error::new(e.to_string()))?,
        serde_json::to_string(&inventory).map_err(|e| Error::new(e.to_string()))?,
        serde_json::to_string(&BTreeMap::from([
            (
                "rust",
                rovik_harness::profile::Language::Rust.required_fields()
            ),
            ("c", rovik_harness::profile::Language::C.required_fields()),
            (
                "cpp",
                rovik_harness::profile::Language::Cpp.required_fields()
            ),
            (
                "javascript",
                rovik_harness::profile::Language::JavaScript.required_fields()
            ),
            (
                "typescript",
                rovik_harness::profile::Language::TypeScript.required_fields()
            ),
            (
                "python",
                rovik_harness::profile::Language::Python.required_fields()
            )
        ]))
        .map_err(|e| Error::new(e.to_string()))?,
        record.denied_read.join(", "),
        record.denied_write.join(", "),
        DRAFTS = DRAFTS,
        example = example
    ))
}

/// Submit only staged preparation artifacts for this exact agent/project/chat binding.
/// No approval, implementation, delivery or arbitrary filesystem path is accepted.
pub async fn submit_preparation(
    state_root: PathBuf,
    project_root: PathBuf,
    project_id: String,
    preparation_id: String,
    dialog_id: String,
    profile: ModelProfile,
    submission: PreparationSubmission,
) -> Result<Value> {
    crate::rt::spawn_blocking(move || {
        let (project, mut record) = bound_project(
            &state_root,
            &project_root,
            &project_id,
            &preparation_id,
            &dialog_id,
            &profile,
        )?;
        restrictions(&mut record, submission.denied)?;
        match project.submit_prepared_assets(&mut record, &submission.input_path) {
            Ok(value)=>Ok(value),
            Err(error) if !record.denied_read.is_empty()=>{
                record.error=Some(error.to_string());
                if let Err(save)=project.save_active_preparation(&record) { log::warn!("Could not retain preparation diagnostic: {save}"); }
                Err(Error::new("Preparation validation or execution failed. Native details are withheld because project read restrictions may protect their content. Inspect the preparation error in Project checks or adjust the draft using accessible source.".into()))
            }
            Err(error)=>Err(error),
        }
    })
    .await
    .map_err(|e| Error::new(format!("Preparation submission failed: {e}")))?
}

impl IdeProject {
    fn submit_prepared_assets(
        &self,
        record: &mut PreparationRecord,
        input_path: &str,
    ) -> Result<Value> {
        let workspace = self.preparation_workspace(&record.id)?;
        let mut submission: Submission =
            wire::decode(&read_staged(&workspace, input_path, STAGED_FILE_LIMIT)?).map_err(
                |error| Error::classified(error.category, format!(
                    "Invalid preparation submission {input_path:?}: {}. Repair the named field using {DRAFTS}/example.json and resubmit the same path.",
                    error.message,
                )),
            )?;
        let mut runtime = record.runtime.clone().ok_or_else(|| {
            Error::new("Prepare the trusted execution environment before submitting.".into())
        })?;
        check(
            submission.oracle_files.len() <= 128
                && !submission.negative_control_files.is_empty()
                && submission.negative_control_files.len() <= 128,
            "Stage a bounded oracle and a real negative control before submitting.",
        )?;
        let ledger = self.ledger()?;
        let source: Snapshot = wire::decode(&ledger.read(&record.snapshot_digest)?)?;
        let mut oracle = BTreeMap::new();
        for staged in &submission.oracle_files {
            validate_staged_target(&staged.path, record)?;
            check(
                !oracle.contains_key(&staged.path),
                "An oracle path appears twice.",
            )?;
            let content = ledger.store(
                "source",
                "text/plain",
                &read_staged(&workspace, &staged.source, STAGED_FILE_LIMIT)?,
                runner::now_ms()?,
            )?;
            oracle.insert(
                staged.path.clone(),
                FileEntry {
                    mode: 0o100644,
                    content,
                },
            );
        }
        for citation in &submission.setup.design.existing_code {
            validate_staged_target(&citation.file, record)?;
        }
        for profile in &submission.setup.profiles {
            for path in profile
                .protected_test_paths
                .iter()
                .chain(profile.typescript_project.iter())
            {
                validate_staged_target(path, record)?;
            }
        }
        let selected: BTreeSet<_> = submission
            .setup
            .profiles
            .iter()
            .flat_map(|p| {
                p.protected_test_paths
                    .iter()
                    .chain(p.typescript_project.iter())
            })
            .cloned()
            .collect();
        check(oracle.keys().all(|p|selected.contains(p)),"Every staged oracle asset must be a selected protected test or compiler configuration.")?;
        let mut control = source.clone();
        let mut changed = BTreeSet::new();
        for staged in &submission.negative_control_files {
            validate_staged_target(&staged.path, record)?;
            check(
                changed.insert(staged.path.clone()),
                "A negative-control source path appears twice.",
            )?;
            check(
                !selected.contains(&staged.path)
                    && !rovik_harness::snapshot::is_build_input(&staged.path),
                "A negative control must preserve protected tests and build inputs.",
            )?;
            check(
                source.files.contains_key(&staged.path),
                "A negative control must mutate existing implementation source.",
            )?;
            let content = ledger.store(
                "source",
                "text/plain",
                &read_staged(&workspace, &staged.source, STAGED_FILE_LIMIT)?,
                runner::now_ms()?,
            )?;
            check(
                source.files[&staged.path].content.digest != content.digest,
                "The negative control must change its source bytes.",
            )?;
            control.files.insert(
                staged.path.clone(),
                FileEntry {
                    mode: source.files[&staged.path].mode,
                    content,
                },
            );
        }
        let inventory = rovik_harness::onboarding::inspect(&ledger, &source)?;
        crate::harness::ide_prepare::scoped_inventory(&inventory, &submission.setup)?;
        let selected_manifests: BTreeSet<String> = submission
            .setup
            .profiles
            .iter()
            .map(|profile| profile.manifest.clone())
            .collect();
        check(
            !selected_manifests.is_empty()
                && selected_manifests.len() == submission.setup.profiles.len(),
            "Select unique build manifests before dependency preparation.",
        )?;
        for profile in &submission.setup.profiles {
            check(
                inventory.units.iter().any(|unit| {
                    unit.manifest == profile.manifest && unit.languages.contains(&profile.language)
                }),
                "A selected build profile does not match the captured project manifest.",
            )?;
        }
        let active = ActivePreparation::begin(self, &record.id)?;
        let dependencies = match &runtime.dependencies {
            Some(path) if runtime.dependency_manifests == selected_manifests => path.clone(),
            _ => self.backend(&runtime.engine)?.prepare_dependencies(
                &self.state.join("preparation-runtime"),
                &ledger,
                &source,
                &selected_manifests,
                &runtime.assets,
                &active.cancel,
            )?,
        };
        runtime.dependencies = Some(dependencies.clone());
        runtime.dependency_manifests = selected_manifests;
        record.runtime = Some(runtime.clone());
        self.save_active_preparation(record)?;
        submission.setup.environment.image_digest = runtime.assets.image_digest.clone();
        submission.setup.environment.engine = if runtime.engine.kind == EngineKind::Docker {
            "docker"
        } else {
            "podman"
        }
        .into();
        submission.setup.environment.toolchain_manifest_path = runtime
            .assets
            .toolchain_manifest
            .to_string_lossy()
            .into_owned();
        submission.setup.environment.dependency_directory =
            dependencies.to_string_lossy().into_owned();
        submission.setup.worker = record.worker.clone();
        submission.setup.budgets = record.budgets.clone();
        submission.setup.duration_ms = record.deadline_ms.saturating_sub(runner::now_ms()?);
        submission.setup.protected_paths.extend(
            record
                .denied_read
                .iter()
                .chain(&record.denied_write)
                .cloned(),
        );
        submission.setup.protected_paths.sort();
        submission.setup.protected_paths.dedup();
        let mut setup = crate::harness::ide_prepare::build_with_assets(
            &self.root,
            &ledger,
            &self.repository_id,
            &source,
            &submission.setup,
            &oracle,
            Some(control),
        )?;
        setup.policy.deny_read = record.denied_read.clone();
        setup.policy.facts.insert(
            "ide.preparation_baseline".into(),
            record.baseline_digest.to_string(),
        );
        setup.plan.policy_digest = Digest::of_json(&setup.policy)?;
        setup.validate(&ledger)?;
        let summary = self.register_setup(setup)?;
        let setup_id = Digest::parse(
            summary["id"]
                .as_str()
                .ok_or_else(|| Error::new("Prepared setup has no identity.".into()))?,
        )?;
        self.database()?.execute("INSERT INTO preparation_baselines(setup_id,baseline_digest) VALUES(?1,?2) ON CONFLICT(setup_id) DO NOTHING",params![setup_id.to_string(),record.baseline_digest.to_string()])?;
        let config = Configuration {
            setup_id: setup_id.clone(),
            engine: runtime.engine.clone(),
            snapshot_digest: record.snapshot_digest.clone(),
        };
        let mut final_setup =
            self.implementation_setup(&config, source, &record.plan, &record.provider)?;
        final_setup.run_id = record.id.clone();
        final_setup.plan.run_id = record.id.clone();
        final_setup.deadline_ms = record.deadline_ms;
        let mut controller = self.controller(Some(&runtime.engine))?;
        let issued = controller.ledger.contains_run(&record.id)?;
        if !issued {
            rovik_harness::roles::Preparation {
                schema_version: 1,
                run_id: record.id.clone(),
                repository_id: final_setup.repository_id.clone(),
                request: record.plan.content.clone(),
                base: final_setup.base.clone(),
                registry: rovik_harness::contract::ContractRegistry {
                    profiles: final_setup.profiles.iter().map(|p| p.id.clone()).collect(),
                    configurations: final_setup
                        .profiles
                        .iter()
                        .flat_map(|p| &p.configurations)
                        .map(|c| c.digest())
                        .collect::<Result<_>>()?,
                    suites: final_setup.suites.clone(),
                    constraints: final_setup.constraint_names.clone(),
                    risks: final_setup.risk_names.clone(),
                },
                authority: final_setup.authority.clone(),
                worker: record.worker.clone(),
                budgets: record.budgets.clone(),
                deadline_ms: record.deadline_ms,
            }
            .create(&mut controller.ledger)?;
        }
        let trial =
            controller.trial_preparation(&Principal::current()?, &final_setup, &active.cancel)?;
        if trial["ready"] != true {
            record.status = "preparing".into();
            record.error=Some("Preparation trials need correction. Inspect the executed cases and diagnostics, then submit revised protected tests or control.".into());
            check(
                self.load_preparation(&record.id)?.status != "cancelled",
                "Preparation was cancelled.",
            )?;
            self.save_active_preparation(record)?;
            let mut response = self.preparation_view(record);
            response["trial"] = trial;
            return Ok(response);
        }
        let final_artifact = ledger
            .store_json("operation", &final_setup, runner::now_ms()?)?
            .digest;
        let mut warnings=vec!["Preparation trials passed. All protected checks run again after approval; delivery requires a separate decision.".to_string()];
        warnings.extend(
            submission
                .setup
                .excluded_build_roots
                .iter()
                .map(|exclusion| {
                    format!(
                        "Unverified and protected build root {}: {}",
                        exclusion.manifest, exclusion.rationale
                    )
                }),
        );
        let review = json!({"setupId":setup_id,"label":final_setup.contract.id,"summary":submission.setup.intent,"scope":{"writable":final_setup.contract.scope.allow_write,"protected":final_setup.contract.scope.protected},"requirements":final_setup.contract.invariants.iter().map(|i|i.statement.clone()).collect::<Vec<_>>(),"checks":final_setup.plan.required_final().map(|c|json!({"id":c.check_id,"label":c.check_id,"configuration":c.configuration_digest})).collect::<Vec<_>>(),"warnings":warnings,"trial":trial,"budgets":record.budgets,"provider":{"model":record.provider.model,"rates":record.provider.rates},"planDigest":record.plan.digest,"snapshotDigest":record.snapshot_digest,"implementationDigest":final_artifact});
        let mut review = review;
        review["digest"] = json!(Digest::of_json(&review)?);
        self.require_fresh_preparation(record)?;
        check(
            self.load_preparation(&record.id)?.status != "cancelled",
            "Preparation was cancelled before submission completed.",
        )?;
        record.status = "ready".into();
        record.error = None;
        record.setup_id = Some(setup_id);
        record.implementation = Some(final_artifact);
        record.review = Some(review);
        self.save_active_preparation(record)?;
        Ok(self.preparation_view(record))
    }

    pub(super) fn start_prepared_implementation(
        &self,
        id: &str,
        expected_review: &str,
        saved_buffers: bool,
        provider: ImplementationProvider,
    ) -> Result<Value> {
        check(
            saved_buffers,
            "Save all buffers before implementing this plan.",
        )?;
        let mutex = ownership(&self.state.join("preparations").join(id))?;
        let _owner = mutex
            .lock()
            .map_err(|_| Error::new("Preparation promotion is unavailable.".into()))?;
        let mut record = self.load_preparation(id)?;
        check(
            record.status == "ready",
            "Finish preparation before starting implementation.",
        )?;
        check(
            record.review.as_ref().and_then(|r| r["digest"].as_str()) == Some(expected_review),
            "The preparation review changed. Review its current summary before starting.",
        )?;
        check(
            record.provider == plan::binding(&provider)?,
            "The selected provider or prices changed. Prepare this plan again.",
        )?;
        if let Some(run_id) = &record.run_id {
            let controller = self.controller(None)?;
            let current = controller.ledger.run(run_id)?;
            if !matches!(current.state, State::Intake | State::Baseline) {
                return self.status(run_id);
            }
        }
        self.require_fresh_preparation(&record)?;
        let setup: TaskSetup = wire::decode(
            &self
                .ledger()?
                .read(record.implementation.as_ref().ok_or_else(|| {
                    Error::new("Preparation has no validated implementation.".into())
                })?)?,
        )?;
        let runtime = record
            .runtime
            .as_ref()
            .ok_or_else(|| Error::new("Preparation runtime is unavailable.".into()))?;
        let config = Configuration {
            setup_id: record
                .setup_id
                .clone()
                .ok_or_else(|| Error::new("Preparation has no setup.".into()))?,
            engine: runtime.engine.clone(),
            snapshot_digest: record.snapshot_digest.clone(),
        };
        let backend = self.backend(&config.engine)?;
        backend.require_environment(&setup.environment)?;
        backend.validate_image(&setup.environment)?;
        self.database()?.execute("INSERT INTO configuration(singleton,value) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET value=excluded.value",[serde_json::to_string(&config).map_err(|e|Error::new(e.to_string()))?])?;
        let mut controller = self.controller(Some(&config.engine))?;
        // Claim promotion durably before changing the controller. A retry can
        // finish either side of the ledger/catalog write without another run.
        record.run_id = Some(setup.run_id.clone());
        self.save_active_preparation(&record)?;
        if controller.ledger.run(&setup.run_id)?.input_digest
            != *record
                .implementation
                .as_ref()
                .ok_or_else(|| Error::new("Missing reviewed implementation.".into()))?
        {
            controller.promote_preparation(&Principal::current()?, &setup)?;
        }
        let input = RunInput {
            plan: Some(plan::PlanBinding {
                path: record.plan.path.clone(),
                digest: record.plan.digest.clone(),
                provider: record.provider.clone(),
            }),
            execution_root: self.root.clone(),
            baseline_reference: self.baseline_reference.clone(),
            tracked_paths: self.tracked_paths.clone(),
            configuration_digest: Digest::of_json(&config)?,
            configuration: config,
            snapshot_digest: record.snapshot_digest.clone(),
            untracked: record.untracked.clone(),
        };
        self.database()?.execute(
            "INSERT INTO runs(id,input,created_ms) VALUES(?1,?2,?3) ON CONFLICT(id) DO NOTHING",
            params![
                setup.run_id,
                serde_json::to_string(&input).map_err(|e| Error::new(e.to_string()))?,
                runner::now_ms()? as i64
            ],
        )?;
        for requirement in setup.mandatory_approvals()? {
            controller.ledger.grant(
                &Principal::current()?,
                &setup.authority,
                ApprovalRequest {
                    run_id: Some(setup.run_id.clone()),
                    subject_digest: requirement.subject_digest,
                    approval_type: requirement.approval_type,
                    policy_digest: Digest::of_json(&setup.policy)?,
                    scope_digest: Digest::of_json(&setup.contract.scope)?,
                    expires_ms: Some(record.deadline_ms),
                },
                runner::now_ms()?,
            )?;
        }
        record.promoted = true;
        self.save_active_preparation(&record)?;
        self.resume_implementation(&setup.run_id, provider, None)
    }
}

fn validate_staged_target(path: &str, record: &PreparationRecord) -> Result<()> {
    rovik_harness::scope::validate_path(path)?;
    check(
        !path.split('/').any(|p| p.eq_ignore_ascii_case(".git"))
            && !path.starts_with(&format!("{DRAFTS}/")),
        "Preparation assets cannot target administration files.",
    )?;
    check(
        !record
            .denied_read
            .iter()
            .chain(&record.denied_write)
            .any(|p| rovik_harness::scope::matches(p, path)),
        "Preparation cannot use or replace a restricted file.",
    )
}

fn read_staged(root: &Path, path: &str, limit: u64) -> Result<Vec<u8>> {
    let relative = Path::new(path)
        .strip_prefix(root)
        .unwrap_or_else(|_| Path::new(path));
    let relative = relative
        .to_str()
        .ok_or_else(|| Error::new("Staged paths must be UTF-8.".into()))?;
    rovik_harness::scope::validate_path(relative)?;
    let mut file = open_staged(root, relative)?;
    let before = file.metadata()?;
    check(
        before.is_file() && before.len() <= limit,
        "Staged assets must be bounded regular files.",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        check(
            before.nlink() == 1,
            "Staged assets cannot be hard-linked outside preparation.",
        )?;
    }
    let mut bytes = Vec::new();
    file.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    check(
        bytes.len() as u64 == before.len()
            && after.len() == before.len()
            && after.modified()? == before.modified()?,
        "A staged asset changed while being captured.",
    )?;
    Ok(bytes)
}

#[cfg(unix)]
fn open_staged(root: &Path, relative: &str) -> Result<fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    };
    let mut directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)?;
    let parts = relative.split('/').collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        let name =
            std::ffi::CString::new(*part).map_err(|_| Error::new("Invalid staged path.".into()))?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if index + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // Each component is opened relative to an already held directory, so
        // a concurrent rename cannot redirect capture through an outside symlink.
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // openat returned a new owned descriptor, transferred exactly once.
        directory = unsafe { fs::File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn open_staged(root: &Path, relative: &str) -> Result<fs::File> {
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
        check(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Staged paths cannot traverse symlinks.",
        )?;
    }
    Ok(fs::File::open(path)?)
}

/// Interrupt active preparation work while retaining its durable drafts for retry.
pub fn cancel_active_preparation(
    state_root: &Path,
    _project_root: &Path,
    project_id: &str,
    preparation_id: &str,
) -> Result<()> {
    let state = state_root.join(Digest::of(project_id.as_bytes()).hex());
    let state = state.canonicalize().unwrap_or(state);
    if let Some(cancel) = active_runs()
        .lock()
        .map_err(|_| Error::new("Preparation cancellation is unavailable.".into()))?
        .get(&(state, preparation_id.into()))
    {
        cancel.cancel();
    }
    Ok(())
}

struct ActivePreparation {
    state: PathBuf,
    id: String,
    cancel: Cancellation,
}
impl ActivePreparation {
    fn begin(project: &IdeProject, id: &str) -> Result<Self> {
        let mut active = active_runs()
            .lock()
            .map_err(|_| Error::new("Preparation ownership is unavailable.".into()))?;
        let key = (project.state.clone(), id.into());
        check(
            !active.contains_key(&key),
            "Preparation is already running. Stop or wait for that operation before retrying.",
        )?;
        let cancel = Cancellation::default();
        active.insert(key, cancel.clone());
        Ok(Self {
            state: project.state.clone(),
            id: id.into(),
            cancel,
        })
    }
}
impl Drop for ActivePreparation {
    fn drop(&mut self) {
        if let Ok(mut active) = active_runs().lock() {
            active.remove(&(self.state.clone(), self.id.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_engine_selection_qualifies_hosts_but_never_replaces_an_explicit_choice() {
        let podman = Engine {
            kind: EngineKind::Podman,
            executable: "/usr/bin/podman".into(),
        };
        let docker = Engine {
            kind: EngineKind::Docker,
            executable: "/usr/bin/docker".into(),
        };
        let qualify = |engine: &Engine| {
            if engine.kind == EngineKind::Podman {
                Err(Error::new("missing delegated cgroup parent".into()))
            } else {
                Ok(())
            }
        };
        let selected = select_preparation_engine(
            None,
            vec![podman.clone(), docker.clone()],
            &Cancellation::default(),
            qualify,
        )
        .unwrap();
        assert_eq!(selected.kind, EngineKind::Docker);
        assert!(select_preparation_engine(
            Some(podman.clone()),
            vec![docker],
            &Cancellation::default(),
            qualify
        )
        .unwrap_err()
        .to_string()
        .contains("delegated"));
        let selected =
            select_preparation_engine(None, vec![podman.clone()], &Cancellation::default(), |_| {
                Ok(())
            })
            .unwrap();
        assert_eq!(selected.kind, EngineKind::Podman);
        let missing =
            select_preparation_engine(None, vec![podman], &Cancellation::default(), qualify)
                .unwrap_err();
        assert!(missing.to_string().contains("delegated"));
        assert!(
            select_preparation_engine(None, vec![], &Cancellation::default(), |_| panic!(
                "no engine"
            ))
            .is_err()
        );
    }

    #[test]
    fn cancelling_host_qualification_does_not_try_another_engine() {
        let cancel = Cancellation::default();
        let engines = vec![
            Engine {
                kind: EngineKind::Podman,
                executable: "/podman".into(),
            },
            Engine {
                kind: EngineKind::Docker,
                executable: "/docker".into(),
            },
        ];
        let mut attempted = 0;
        let result = select_preparation_engine(None, engines.clone(), &cancel, |_| {
            attempted += 1;
            cancel.cancel();
            Err(Error::new("probe interrupted".into()))
        });
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert_eq!(attempted, 1);
        assert!(
            select_preparation_engine(None, engines, &cancel, |_| panic!("already cancelled"))
                .is_err()
        );
    }

    fn provider() -> ImplementationProvider {
        serde_json::from_value(json!({"profile":{"id":"local::fixture-scripted","label":"Fixture","model":"fixture-scripted","baseUrl":"http://localhost:8000/v1","apiKey":"must-not-persist","contextTokens":64000,"firstByteTimeoutMs":null},"rates":{"input":0,"cachedInput":0,"output":0}})).unwrap()
    }

    fn issued() -> (tempfile::TempDir, IdeProject, PreparationRecord) {
        let (directory, project) = super::super::tests::project();
        fs::create_dir(project.root.join("plans")).unwrap();
        fs::write(
            project.root.join("plans/task.md"),
            "Include the equality boundary.",
        )
        .unwrap();
        let view = project
            .begin_preparation("chat", "plans/task.md", provider(), true, vec![])
            .unwrap();
        let record = project
            .load_preparation(view["preparationId"].as_str().unwrap())
            .unwrap();
        (directory, project, record)
    }

    fn guidance() -> (tempfile::TempDir, IdeProject, PreparationRecord, String) {
        let (directory, project, mut record) = issued();
        record.runtime = Some(Runtime {
            engine: Engine {
                kind: EngineKind::Docker,
                executable: "unused-example-engine".into(),
            },
            assets: PreparedEnvironment {
                image_digest: Digest::of(b"example-image"),
                toolchain_manifest: directory.path().join("toolchain.json"),
                recipe_digest: Digest::of(b"example-recipe"),
                backend_version: "example-backend".into(),
                facts: BTreeMap::from([
                    ("node_version".into(), "24.4.0".into()),
                    ("npm_version".into(), "11.4.2".into()),
                ]),
            },
            dependencies: None,
            dependency_manifests: BTreeSet::new(),
        });
        let workspace = project.preparation_workspace(&record.id).unwrap();
        let instructions = write_template(&project, &record, &workspace).unwrap();
        (directory, project, record, instructions)
    }

    #[test]
    fn complete_submission_guidance_matches_the_schema_and_refreshes_without_losing_drafts() {
        let (_directory, project, record, instructions) = guidance();
        assert!(instructions.contains("set rust_version to the measured rust_version"));
        assert!(instructions.contains("never the linux-x86_64 runtime label"));
        assert!(instructions.contains(".rovik-verification.tsconfig.json"));
        let workspace = project.preparation_workspace(&record.id).unwrap();
        let example_path = workspace.join(DRAFTS).join("example.json");
        let example = fs::read_to_string(&example_path).unwrap();
        let submitted: Submission = wire::decode(example.as_bytes()).unwrap();
        submitted.setup.worker.validate().unwrap();
        assert_eq!(submitted.setup.worker.model_id, record.worker.model_id);
        assert_eq!(submitted.setup.budgets, record.budgets);
        assert_eq!(
            submitted.setup.protected_paths.as_slice(),
            std::slice::from_ref(&record.plan.path)
        );
        assert_eq!(submitted.setup.profiles[0].facts["node_version"], "24.4.0");
        assert_eq!(
            submitted.setup.baseline_failures[0].case_ids,
            BTreeSet::from(["zero".into()])
        );
        assert_eq!(submitted.oracle_files.len(), 1);
        assert_eq!(submitted.negative_control_files.len(), 1);
        assert!(!example.contains("COPY_MEASURED"));
        assert!(!example.contains("must-not-persist"));
        let inline = instructions
            .split_once("Complete worked submission.json example")
            .unwrap()
            .1
            .split_once("```json\n")
            .unwrap()
            .1
            .split_once("\n```")
            .unwrap()
            .0;
        assert_eq!(
            inline, example,
            "the agent must see the complete on-disk example in its instructions"
        );

        let draft = workspace.join(DRAFTS).join("submission.json");
        fs::write(&draft, "the agent's existing draft").unwrap();
        fs::write(&example_path, "old guidance").unwrap();
        write_template(&project, &record, &workspace).unwrap();
        wire::decode::<Submission>(&fs::read(example_path).unwrap()).unwrap();
        assert_eq!(
            fs::read_to_string(draft).unwrap(),
            "the agent's existing draft"
        );
    }

    #[test]
    fn malformed_submission_errors_name_the_exact_value_instead_of_guessing_at_collections() {
        let (_directory, project, mut record, _) = guidance();
        let workspace = project.preparation_workspace(&record.id).unwrap();
        let example: Value =
            wire::decode(&fs::read(workspace.join(DRAFTS).join("example.json")).unwrap()).unwrap();
        let path = format!("{DRAFTS}/submission.json");
        for (pointer, expected, value) in [
            (
                "/setup/design/components/classification",
                "setup.design.components.classification",
                json!({"responsibility":"Own classification"}),
            ),
            (
                "/setup/design/publicApiChanges/0",
                "setup.design.publicApiChanges[0]",
                json!({"description":"Preserve API"}),
            ),
            (
                "/setup/design/resourceContracts/0",
                "setup.design.resourceContracts[0]",
                json!({"owner":"classification"}),
            ),
            (
                "/setup/design/errorContracts/0",
                "setup.design.errorContracts[0]",
                json!({"error":"none"}),
            ),
            (
                "/setup/design/mechanismAlternatives/0",
                "setup.design.mechanismAlternatives[0]",
                json!({"alternative":"strategy"}),
            ),
            (
                "/setup/design/concurrencyContract",
                "setup.design.concurrencyContract",
                json!({"synchronization":"none"}),
            ),
            (
                "/setup/profiles/0/facts/node_version",
                "setup.profiles[0].facts.node_version",
                json!({"version":"24.4.0"}),
            ),
            (
                "/setup/baselineFailures/0",
                "setup.baselineFailures[0]",
                json!("zero"),
            ),
            (
                "/setup/baselineFailures/0/caseIds/0",
                "setup.baselineFailures[0].caseIds[0]",
                json!({"id":"zero"}),
            ),
        ] {
            let mut invalid = example.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            let bytes = serde_json::to_vec(&invalid).unwrap();
            fs::write(workspace.join(&path), &bytes).unwrap();
            let error = project
                .submit_prepared_assets(&mut record, &path)
                .unwrap_err();
            assert!(error.message.contains(expected), "{pointer}: {error}");
            assert!(error.message.contains(&path), "{error}");
            assert!(
                error.message.contains(".rovik-preparation/example.json"),
                "{error}"
            );
            assert_eq!(fs::read(workspace.join(&path)).unwrap(), bytes);
            assert_eq!(record.status, "preparing");
        }
        // Correcting the bad value restores a decodable recipe without deleting
        // the required design or converting baseline records to strings.
        wire::decode::<Submission>(&serde_json::to_vec(&example).unwrap()).unwrap();
    }

    #[test]
    fn decimal_model_prices_prepare_within_the_default_limit_and_wrong_units_are_actionable() {
        let (_directory, project, original) = issued();
        let mut selected = provider();
        selected.rates = rovik_protocol::harness::ModelRates {
            input: 225_000,
            cached_input: 4_500,
            output: 900_000,
        };
        for cached in [false, true] {
            let mut mispriced = selected.clone();
            if cached {
                mispriced.rates.cached_input = 225_000_000;
            } else {
                mispriced.rates.input = 225_000_000;
            }
            let error = project
                .begin_preparation("priced", &original.plan.path, mispriced, true, vec![])
                .unwrap_err();
            assert_eq!(error.category, rovik_harness::FailureCategory::Budget);
            assert!(error.message.contains("spending limit is 10"));
            assert!(error.message.contains("requires 18.770798"));
            assert!(error.message.contains(if cached {
                "input 0.225, cached input 225, output 0.9"
            } else {
                "input 225, cached input 0.0045, output 0.9"
            }));
            assert!(error
                .message
                .contains("Model providers → Verification pricing"));
            assert!(!error.message.contains("microunits"));
        }
        let count: i64 = project
            .database()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM preparations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "unfunded preparation must not start an agent session"
        );
        let view = project
            .begin_preparation(
                "priced",
                &original.plan.path,
                selected.clone(),
                true,
                vec![],
            )
            .unwrap();
        let record = project
            .load_preparation(view["preparationId"].as_str().unwrap())
            .unwrap();
        assert_eq!(record.status, "preparing");
        assert_eq!(record.provider.rates, selected.rates);
        assert_eq!(record.worker.cost_reservation, 26_137);
        assert_eq!(record.budgets["cost_microunits"], 10_000_000);
        assert_eq!(record.snapshot_digest, original.snapshot_digest);
    }

    #[test]
    fn beginning_preparation_captures_untracked_source_without_creating_approvals_or_editing_project(
    ) {
        let (_directory, project, record) = issued();
        assert!(record.untracked.contains("plans/task.md"));
        assert_eq!(record.status, "preparing");
        let workspace = project.preparation_workspace(&record.id).unwrap();
        assert_eq!(
            fs::read(workspace.join("plans/task.md")).unwrap(),
            fs::read(project.root.join("plans/task.md")).unwrap()
        );
        assert!(!project.root.join(DRAFTS).exists());
        assert!(project.ledger().unwrap().run(&record.id).is_err());
        assert!(!record_json(&record).unwrap().contains("must-not-persist"));
        let view = project.preparation_view(&record);
        assert!(view.get("workspaceRoot").is_none());
        assert!(view.get("instructions").is_none());
        assert_eq!(view["providerBinding"]["model"], "fixture-scripted");
        assert!(!view.to_string().contains("must-not-persist"));
    }

    #[test]
    fn cancellation_cannot_be_overwritten_by_in_flight_preparation_and_duplicate_work_is_rejected()
    {
        let (_directory, project, mut record) = issued();
        let active = ActivePreparation::begin(&project, &record.id).unwrap();
        assert!(ActivePreparation::begin(&project, &record.id).is_err());
        project.cancel_preparation(&record.id).unwrap();
        assert!(active.cancel.is_cancelled());
        for status in ["ready", "blocked"] {
            record.status = status.into();
            record.error = Some("in-flight host qualification failed".into());
            assert!(project.save_active_preparation(&record).is_err());
        }
        assert_eq!(
            project.load_preparation(&record.id).unwrap().status,
            "cancelled"
        );
        drop(active);
        assert!(active_runs()
            .lock()
            .unwrap()
            .get(&(project.state.clone(), record.id))
            .is_none());
    }

    #[tokio::test]
    async fn host_qualification_failure_persists_a_blocked_retryable_preparation() {
        let (directory, project, record) = issued();
        let configuration = super::super::tests::prepared(
            &project,
            Digest::of(b"unused image"),
            &directory.path().join("missing-engine").to_string_lossy(),
        );
        project
            .database()
            .unwrap()
            .execute(
                "INSERT INTO configuration(singleton,value) VALUES(1,?1)",
                [serde_json::to_string(&configuration).unwrap()],
            )
            .unwrap();
        let project_id = Digest::of(project.root.as_os_str().as_encoded_bytes()).to_string();
        let error = preparation_context(
            directory.path().join("private"),
            project.root.clone(),
            project_id,
            record.id.clone(),
            "chat".into(),
            provider().profile,
            Denied::default(),
        )
        .await
        .unwrap_err();
        let saved = project.load_preparation(&record.id).unwrap();
        assert_eq!(saved.status, "blocked", "{error}");
        assert!(error.to_string().contains(if cfg!(target_os = "linux") {
            "registered container client is missing"
        } else {
            "Choose a connected Linux host"
        }));
        assert_eq!(saved.error.as_deref(), Some(error.to_string().as_str()));
        assert!(saved.runtime.is_none());
        assert!(!project.state.join("preparation-runtime").exists());
        assert!(ActivePreparation::begin(&project, &record.id).is_ok());
    }

    #[test]
    fn retry_restores_newly_readable_source_without_replacing_agent_drafts() {
        let (_directory, project, mut record) = issued();
        let workspace = project.preparation_workspace(&record.id).unwrap();
        let ledger = project.ledger().unwrap();
        let mut snapshot: Snapshot =
            wire::decode(&ledger.read(&record.snapshot_digest).unwrap()).unwrap();
        snapshot.files.insert(
            ".env".into(),
            FileEntry {
                mode: 0o100644,
                content: ledger
                    .store(
                        "source",
                        "text/plain",
                        b"SECRET=never-expose",
                        runner::now_ms().unwrap(),
                    )
                    .unwrap(),
            },
        );
        record.snapshot_digest = ledger
            .store_json("snapshot", &snapshot, runner::now_ms().unwrap())
            .unwrap()
            .digest;
        let path = "implementations/quota/lib.rs";
        let original = fs::read(workspace.join(path)).unwrap();
        let draft = workspace.join(DRAFTS).join("submission.json");
        fs::write(&draft, "saved agent draft").unwrap();
        fs::remove_file(workspace.join(path)).unwrap();
        restrictions(
            &mut record,
            Denied {
                read: vec![path.into()],
                write: vec![],
            },
        )
        .unwrap();
        project
            .restore_preparation_source(&record, &workspace)
            .unwrap();
        assert!(
            !workspace.join(path).exists(),
            "still-restricted source must remain hidden"
        );
        restrictions(&mut record, Denied::default()).unwrap();
        project
            .restore_preparation_source(&record, &workspace)
            .unwrap();
        assert_eq!(fs::read(workspace.join(path)).unwrap(), original);
        assert!(
            !workspace.join(".env").exists(),
            "secret-pattern files stay hidden even after explicit restrictions are relaxed"
        );
        assert_eq!(fs::read_to_string(&draft).unwrap(), "saved agent draft");
        fs::write(workspace.join(path), "existing private source").unwrap();
        project
            .restore_preparation_source(&record, &workspace)
            .unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join(path)).unwrap(),
            "existing private source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_restoration_rejects_a_redirected_parent_directory() {
        let (directory, project, record) = issued();
        let workspace = project.preparation_workspace(&record.id).unwrap();
        fs::remove_dir_all(workspace.join("implementations")).unwrap();
        let outside = directory.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("implementations")).unwrap();
        assert!(project
            .restore_preparation_source(&record, &workspace)
            .is_err());
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn status_observation_does_not_hash_or_publish_source_but_submission_still_checks_it() {
        let (_directory, project, record) = issued();
        fs::write(
            project.root.join("implementations/quota/lib.rs"),
            "changed source",
        )
        .unwrap();
        assert_eq!(
            project.preparation_status(&record.id).unwrap()["status"],
            "preparing"
        );
        assert!(project
            .require_fresh_preparation(&record)
            .unwrap_err()
            .message
            .contains("project changed"));
        assert_eq!(
            project.preparation_status(&record.id).unwrap()["status"],
            "stale"
        );
    }

    #[test]
    fn storage_failures_preserve_preparation_drafts_and_allow_retry_after_recovery() {
        let (directory, project, mut record) = issued();
        record.error = Some("The previous trial needs correction.".into());
        project.save_active_preparation(&record).unwrap();
        let draft = project
            .preparation_workspace(&record.id)
            .unwrap()
            .join(DRAFTS)
            .join("submission.json");
        fs::write(&draft, "draft from the failed attempt").unwrap();
        let before = record_json(&record).unwrap();
        let database = Connection::open(project.state.join("ledger.sqlite3")).unwrap();
        database.execute_batch("CREATE TRIGGER fail_publication BEFORE INSERT ON artifacts BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;").unwrap();

        // Polling must not import source or write to the artifact ledger.
        assert_eq!(
            project.preparation_status(&record.id).unwrap()["status"],
            "preparing"
        );
        let error = project.require_fresh_preparation(&record).unwrap_err();
        assert_eq!(error.category, rovik_harness::FailureCategory::Internal);
        assert!(error.message.contains("injected storage failure"));
        assert_eq!(
            record_json(&project.load_preparation(&record.id).unwrap()).unwrap(),
            before
        );
        assert_eq!(
            fs::read_to_string(&draft).unwrap(),
            "draft from the failed attempt"
        );

        database
            .execute_batch("DROP TRIGGER fail_publication")
            .unwrap();
        assert_eq!(
            project.preparation_status(&record.id).unwrap()["status"],
            "preparing"
        );
        let project_id = Digest::of(project.root.as_os_str().as_encoded_bytes()).to_string();
        let (_, resumed) = bound_project(
            &directory.path().join("private"),
            &project.root,
            &project_id,
            &record.id,
            "chat",
            &provider().profile,
        )
        .unwrap();
        assert_eq!(resumed.id, record.id);
        assert!(ActivePreparation::begin(&project, &record.id).is_ok());
    }

    #[test]
    fn source_or_plan_changes_make_the_captured_review_stale() {
        let (_directory, project, record) = issued();
        project.require_fresh_preparation(&record).unwrap();
        fs::write(
            project.root.join("implementations/quota/lib.rs"),
            "changed source",
        )
        .unwrap();
        assert!(project
            .require_fresh_preparation(&record)
            .unwrap_err()
            .message
            .contains("project changed"));
        assert_eq!(
            project.preparation_status(&record.id).unwrap()["status"],
            "stale"
        );
        let (_directory, project, record) = issued();
        fs::write(project.root.join("plans/task.md"), "Different plan").unwrap();
        assert!(project
            .require_fresh_preparation(&record)
            .unwrap_err()
            .message
            .contains("plan changed"));
    }

    #[test]
    fn restrictions_are_deduplicated_and_exclude_plan_tests_controls_and_citations() {
        let (_directory, _project, mut record) = issued();
        restrictions(
            &mut record,
            Denied {
                read: vec!["private/".into(), "private/".into()],
                write: vec!["frozen.rs".into()],
            },
        )
        .unwrap();
        assert_eq!(record.denied_read, ["private/**"]);
        assert!(validate_staged_target("private/lib.rs", &record).is_err());
        assert!(validate_staged_target("frozen.rs", &record).is_err());
        assert!(validate_staged_target(".GIT/config", &record).is_err());
        assert!(restrictions(
            &mut record,
            Denied {
                read: vec!["plans/".into()],
                write: vec![]
            }
        )
        .is_err());
    }

    #[test]
    fn staged_reads_are_bounded_and_cannot_escape_the_issued_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("draft.json"), "12345").unwrap();
        assert_eq!(read_staged(&root, "draft.json", 5).unwrap(), b"12345");
        assert!(read_staged(&root, "draft.json", 4).is_err());
        assert!(read_staged(&root, "../draft.json", 20).is_err());
        assert!(read_staged(&root, "/etc/passwd", 20).is_err());
        #[cfg(unix)]
        {
            let outside = directory.path().join("secret");
            fs::write(&outside, "secret").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            assert!(read_staged(&root, "link", 100).is_err());
            fs::hard_link(&outside, root.join("hard-link")).unwrap();
            assert!(read_staged(&root, "hard-link", 100).is_err());
            std::os::unix::fs::symlink(directory.path(), root.join("parent")).unwrap();
            assert!(read_staged(&root, "parent/secret", 100).is_err());
        }
    }

    #[test]
    fn draft_submissions_are_closed_and_cannot_supply_approvals_or_verdicts() {
        assert!(wire::decode::<Submission>(
            br#"{"setup":{},"negativeControlFiles":[],"approved":true}"#
        )
        .is_err());
        assert!(wire::decode::<StagedFile>(
            br#"{"path":"src/lib.rs","source":"draft","verdict":"pass"}"#
        )
        .is_err());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires ROVIK_HARNESS_PREPARATION_NATIVE=1 and a local Docker/Podman engine"]
    async fn native_automatic_preparation_and_exact_candidate_review_verify_without_checkout_edits()
    {
        use tokio::io::AsyncWriteExt;
        assert_eq!(
            std::env::var("ROVIK_HARNESS_PREPARATION_NATIVE").unwrap(),
            "1"
        );
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        fs::create_dir_all(root.join("src")).unwrap();
        let source = "export function classify(value) {\n  return value > 0;\n}\n";
        let fixed = "export function classify(value) {\n  return value >= 0;\n}\n";
        let repository = git2::Repository::init(&root).unwrap();
        let mut index = repository.index().unwrap();
        for (path, content) in [
            ("src/classify.mjs", source),
            (
                "package.json",
                r#"{"name":"preparation-fixture","version":"1.0.0","type":"module"}"#,
            ),
            (
                "package-lock.json",
                r#"{"name":"preparation-fixture","version":"1.0.0","lockfileVersion":3,"packages":{"":{"name":"preparation-fixture","version":"1.0.0"}}}"#,
            ),
        ] {
            fs::write(root.join(path), content).unwrap();
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let author = git2::Signature::now("Preparation fixture", "fixture@rovik.invalid").unwrap();
        repository
            .commit(
                Some("HEAD"),
                &author,
                &author,
                "initial project",
                &tree,
                &[],
            )
            .unwrap();
        fs::write(
            root.join("plan.md"),
            "Include zero in classification; preserve positive and negative behavior.",
        )
        .unwrap();
        let state_root = directory.path().join("private");
        let project =
            IdeProject::open_for_project(&state_root, &root, "automatic-fixture").unwrap();
        assert!(project.configuration().unwrap().is_none());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut selected = provider();
        selected.profile.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (_, body) = crate::testing::read_request(&mut socket).await;
                let request: rovik_harness::provider::ModelRequest =
                    serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                let output = json!({"schema_version":1,"request_id":request.request_id,"tool_requests":[],"questions":[],"proposal":{"base_digest":request.snapshot_digest,"changes":[{"path":"src/classify.mjs","content":fixed,"mode":33188}],"summary":"Include equality without changing the API.","requirement_mapping":{"BOUNDARY":["src/classify.mjs"]}}});
                let chunk = json!({"choices":[{"delta":{"content":output.to_string()},"finish_reason":null}]});
                let finish = json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":100}});
                let stream = format!("data: {chunk}\n\ndata: {finish}\n\ndata: [DONE]\n\n");
                let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{stream}",stream.len());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        let begun = project
            .begin_preparation("chat", "plan.md", selected.clone(), true, vec![])
            .unwrap();
        let id = begun["preparationId"].as_str().unwrap().to_string();
        let context = preparation_context(
            state_root.clone(),
            root.clone(),
            "automatic-fixture".into(),
            id.clone(),
            "chat".into(),
            selected.profile.clone(),
            Denied::default(),
        )
        .await
        .unwrap();
        let record = project.load_preparation(&id).unwrap();
        let runtime = record.runtime.as_ref().unwrap();
        if std::env::var("ROVIK_HARNESS_REQUIRE_PODMAN").as_deref() == Ok("1") {
            assert_eq!(
                runtime.engine.kind,
                EngineKind::Podman,
                "CI must exercise delegated rootless Podman"
            );
        }
        // Exercise the full example supplied to the agent through real trials.
        let proposal: Value = wire::decode(
            &fs::read(context.workspace_root.join(DRAFTS).join("example.json")).unwrap(),
        )
        .unwrap();
        let drafts = context.workspace_root.join(DRAFTS);
        fs::create_dir(drafts.join("oracle")).unwrap();
        fs::create_dir(drafts.join("control")).unwrap();
        fs::write(drafts.join("oracle/acceptance.mjs"),"import {test} from 'node:test'; import assert from 'node:assert/strict'; import {classify} from '/build/project/src/classify.mjs'; test('positive',()=>assert.equal(classify(1),true)); test('zero',()=>assert.equal(classify(0),true)); test('negative',()=>assert.equal(classify(-1),false));\n").unwrap();
        fs::write(
            drafts.join("control/classify.mjs"),
            source.replace("value > 0", "value > 10"),
        )
        .unwrap();
        fs::write(
            drafts.join("submission.json"),
            serde_json::to_vec(&proposal).unwrap(),
        )
        .unwrap();
        let ready = submit_preparation(
            state_root,
            root.clone(),
            "automatic-fixture".into(),
            id.clone(),
            "chat".into(),
            selected.profile.clone(),
            PreparationSubmission {
                input_path: format!("{DRAFTS}/submission.json"),
                denied: Denied::default(),
            },
        )
        .await
        .unwrap();
        assert_eq!(ready["status"], "ready", "{ready:#}");
        assert!(project
            .ledger()
            .unwrap()
            .approval_history(&id)
            .unwrap()
            .is_empty());
        assert_eq!(
            project.ledger().unwrap().budgets(&id).unwrap()["checks"].spent,
            2
        );
        let index_before = fs::read(root.join(".git/index")).unwrap();
        let review = ready["review"]["digest"].as_str().unwrap().to_string();
        project
            .start_prepared_implementation(&id, &review, true, selected.clone())
            .unwrap();
        let started = std::time::Instant::now();
        while project.is_running(&id).unwrap() {
            assert!(started.elapsed() < std::time::Duration::from_secs(240));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let candidate_review = project.status(&id).unwrap();
        assert_eq!(
            candidate_review["status"], "blocked",
            "{candidate_review:#}"
        );
        let pending = candidate_review["pendingApprovals"].as_array().unwrap();
        assert_eq!(pending.len(), 1, "{candidate_review:#}");
        assert_eq!(pending[0]["request"]["approval_type"], "design-review");
        let controller = project.controller(None).unwrap();
        let candidate = controller.current_candidate(&id).unwrap().unwrap();
        assert_eq!(
            pending[0]["request"]["subject_digest"],
            json!(candidate.digest().unwrap())
        );
        assert!(controller.ledger.budgets(&id).unwrap()["checks"].spent >= 4);
        project
            .dispatch(Operation::Approve {
                run_id: id.clone(),
                request: pending[0]["request"].clone(),
            })
            .unwrap();
        project
            .dispatch(Operation::ResumeImplementation {
                run_id: id.clone(),
                provider: selected,
            })
            .unwrap();
        while project.is_running(&id).unwrap() {
            assert!(started.elapsed() < std::time::Duration::from_secs(300));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let status = project.status(&id).unwrap();
        assert_eq!(status["status"], "verified", "{status:#}");
        assert_eq!(
            fs::read_to_string(root.join("src/classify.mjs")).unwrap(),
            source
        );
        assert_eq!(fs::read(root.join(".git/index")).unwrap(), index_before);
        assert!(!root.join("tests/acceptance.mjs").exists());
        assert_eq!(project.preparation_status(&id).unwrap()["runId"], id);
        assert!(status["budgets"]["checks"]["spent"].as_u64().unwrap() >= 5);
        server.abort();
    }
}
