//! Trusted IDE service for preparation catalogs and durable, isolated verification.
//!
//! This module is reached only by authenticated desktop/daemon management RPCs.
//! The worker broker never exposes it. The execution host supplies authority and
//! private storage; neither model output nor a frontend status can grant a verdict.

use super::{ide_setup, GitIntegration};
use rovik_harness::{
    approvals::{ApprovalRequest, Principal},
    controller::{Controller, TaskSetup},
    evidence::Verdict,
    identity::Digest,
    ledger::Ledger,
    runner::{self, Cancellation, ContainerBackend},
    snapshot::Snapshot,
    state::State,
    wire, Error, Result,
};
use rovik_protocol::harness::{Engine, EngineKind, Operation};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

#[path = "ide_architecture.rs"]
mod architecture;
#[path = "ide_plan.rs"]
mod plan;

#[path = "ide_application.rs"]
mod application;
pub use application::application_guard;
#[path = "ide_autopilot.rs"]
mod autopilot;
pub use autopilot::{
    ensure_autopilot, validate_autopilot_assets, validate_implementation_provider, AutopilotIntake,
    AutopilotReadiness,
};

type ActiveRuns = BTreeMap<(PathBuf, String), Cancellation>;
static ACTIVE: OnceLock<Mutex<ActiveRuns>> = OnceLock::new();
static OWNERSHIP: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn ownership(path: &Path) -> Result<Arc<Mutex<()>>> {
    let mut map = OWNERSHIP
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| Error::new("Verification ownership registry is unavailable.".into()))?;
    Ok(map.entry(path.into()).or_default().clone())
}

fn active_runs() -> &'static Mutex<ActiveRuns> {
    ACTIVE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn check(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::new(message.into()))
    }
}

fn encode(value: &impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| Error::new(e.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    wire::decode(&wire::canonical(&value)?)
}

/// Tracked paths and the exact local index bytes used to fence capture.
type TrackedInputs = (BTreeSet<String>, Option<(PathBuf, Digest)>);

/// Private state and selected repository, resolved by the authenticated RPC adapter.
#[derive(Clone)]
pub struct IdeProject {
    /// Canonical repository root on this execution host.
    pub root: PathBuf,
    /// Controller state outside the selected repository.
    pub state: PathBuf,
    /// Stable execution-host binding for the repository.
    pub repository_id: String,
    /// Server-created baseline reference for a synchronized execution workspace.
    pub baseline_reference: Option<String>,
    /// Current tracked paths authenticated by the desktop source-binding adapter.
    pub tracked_paths: Option<BTreeSet<String>>,
}

/// Reviewed execution selection; it contains no model credentials.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Configuration {
    /// Exact imported setup selected by the operator.
    pub setup_id: Digest,
    /// Reviewed container engine executable.
    pub engine: Engine,
    /// Snapshot seen during configuration review.
    pub snapshot_digest: Digest,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunInput {
    #[serde(default)]
    plan: Option<plan::PlanBinding>,
    execution_root: PathBuf,
    #[serde(default)]
    baseline_reference: Option<String>,
    #[serde(default)]
    tracked_paths: Option<BTreeSet<String>>,
    configuration_digest: Digest,
    configuration: Configuration,
    snapshot_digest: Digest,
    untracked: BTreeSet<String>,
}

/// Decode the closed management operation and perform filesystem work off the UI runtime.
/// Verification itself starts a background controller and returns its durable run ID.
pub fn execute(
    state_root: PathBuf,
    project_root: PathBuf,
    project_id: String,
    request: Operation,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send>> {
    execute_bound(state_root, project_root, project_id, None, None, request)
}

/// Route a synchronized workspace against its host-authenticated source baseline.
/// `baseline_reference` is supplied by the trusted transport adapter, never a worker.
pub fn execute_bound(
    state_root: PathBuf,
    project_root: PathBuf,
    project_id: String,
    baseline_reference: Option<String>,
    tracked_paths: Option<Vec<String>>,
    request: Operation,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send>> {
    Box::pin(async move {
        crate::rt::spawn_blocking(move || {
            let mut project =
                IdeProject::open_for_project(&state_root, &project_root, &project_id)?;
            check(
                baseline_reference.is_some() == tracked_paths.is_some(),
                "Inspect the execution workspace again to establish its source binding.",
            )?;
            project.baseline_reference = baseline_reference;
            project.tracked_paths = tracked_paths.map(|paths| paths.into_iter().collect());
            if let Some(paths) = &project.tracked_paths {
                for path in paths {
                    rovik_harness::scope::validate_path(path)?;
                }
            }
            project.integration()?;
            project.dispatch(request)
        })
        .await
        .map_err(|e| Error::new(format!("verification service task failed: {e}")))?
    })
}

/// Request cancellation before host shutdown; private jobs retain their durable history.
pub fn cancel_all() {
    match active_runs().lock() {
        Ok(active) => {
            for cancellation in active.values() {
                cancellation.cancel();
            }
        }
        Err(_) => log::error!("Cannot signal verification cancellation during shutdown"),
    }
}

impl IdeProject {
    /// Bind private state to one canonical project without accepting a frontend state path.
    pub fn open(state_root: &Path, project_root: &Path) -> Result<Self> {
        let identity = Digest::of(project_root.canonicalize()?.as_os_str().as_encoded_bytes());
        Self::open_for_project(state_root, project_root, identity.as_str())
    }

    /// Reuse the project catalog across trusted chat worktrees on the same execution host.
    /// The project ID must come from the desktop/daemon database, never a worker request.
    pub fn open_for_project(
        state_root: &Path,
        project_root: &Path,
        project_id: &str,
    ) -> Result<Self> {
        check(
            !project_id.is_empty() && project_id.len() <= 256,
            "Select a registered IDE project.",
        )?;
        let root = project_root.canonicalize()?;
        check(root.is_dir(), "Select an existing project directory.")?;
        let repository = git2::Repository::open(&root).map_err(|error| {
            Error::new(format!(
                "Open the Git repository root to configure strict verification: {error}"
            ))
        })?;
        let workdir = repository.workdir().map(Path::canonicalize).transpose()?;
        check(workdir.as_ref() == Some(&root), "Open the Git repository root to configure strict verification; nested build roots are discovered inside that repository.")?;
        check(
            state_root.is_absolute(),
            "Verification state must use an absolute private path.",
        )?;
        check(
            !state_root.starts_with(&root),
            "Verification state must be outside the project.",
        )?;
        let identity = Digest::of(project_id.as_bytes());
        let repository_id = format!("project-{}", &identity.hex()[..24]);
        let state = state_root.join(identity.hex());
        let ledger = Ledger::open(&state)?;
        let state = ledger.root().canonicalize()?;
        check(
            !state.starts_with(&root),
            "Verification state must be outside the project.",
        )?;
        let project = Self {
            root,
            state,
            repository_id,
            baseline_reference: None,
            tracked_paths: None,
        };
        project.database()?;
        Ok(project)
    }

    fn database(&self) -> Result<Connection> {
        let path = self.state.join("ide.sqlite3");
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            check(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Verification index must be a regular private file.",
            )?;
        }
        let database = Connection::open(path)?;
        database.busy_timeout(std::time::Duration::from_secs(5))?;
        database.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS setups(id TEXT PRIMARY KEY, artifact TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS configuration(singleton INTEGER PRIMARY KEY CHECK(singleton=1), value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS captures(digest TEXT PRIMARY KEY, untracked TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY, input TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 0, owner TEXT, error TEXT, created_ms INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS applications(subject TEXT PRIMARY KEY, run_id TEXT NOT NULL, value TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'prepared', receipt TEXT, created_ms INTEGER NOT NULL);
            CREATE UNIQUE INDEX IF NOT EXISTS one_active_run ON runs(active) WHERE active=1;")?;
        Ok(database)
    }

    /// Open the project's content-addressed durable ledger.
    pub fn ledger(&self) -> Result<Ledger> {
        Ledger::open(&self.state)
    }

    /// Observe the selected repository without modifying its checkout or index.
    pub fn integration(&self) -> Result<GitIntegration> {
        check(
            self.root.canonicalize()? == self.root,
            "The execution workspace changed identity; inspect it again.",
        )?;
        match &self.baseline_reference {
            Some(reference) => {
                GitIntegration::for_reference(self.repository_id.clone(), &self.root, reference)
            }
            None => GitIntegration::new(self.repository_id.clone(), &self.root),
        }
    }

    fn configuration(&self) -> Result<Option<Configuration>> {
        let raw: Option<String> = self
            .database()?
            .query_row(
                "SELECT value FROM configuration WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|raw| wire::decode(raw.as_bytes())).transpose()
    }

    fn load_setup(&self, id: &Digest) -> Result<TaskSetup> {
        let digest: String = self
            .database()?
            .query_row(
                "SELECT artifact FROM setups WHERE id=?1",
                [id.as_str()],
                |r| r.get(0),
            )
            .map_err(|_| {
                Error::new(
                    "The selected prepared setup is unavailable. Import or prepare it again."
                        .into(),
                )
            })?;
        let setup: TaskSetup = wire::decode(&self.ledger()?.read(&Digest::parse(&digest)?)?)?;
        check(
            Digest::of_json(&setup)? == *id && setup.repository_id == self.repository_id,
            "The prepared setup no longer matches this project.",
        )?;
        Ok(setup)
    }

    /// Register validated prepared assets; imported approval records are never adopted.
    pub fn register_setup(&self, setup: TaskSetup) -> Result<Value> {
        let ledger = self.ledger()?;
        check(
            setup.repository_id == self.repository_id,
            "Prepared setup belongs to another project.",
        )?;
        setup.validate(&ledger)?;
        let id = Digest::of_json(&setup)?;
        let artifact = ledger.store_json("operation", &setup, runner::now_ms()?)?;
        self.database()?.execute(
            "INSERT OR IGNORE INTO setups(id,artifact) VALUES(?1,?2)",
            params![id.as_str(), artifact.digest.as_str()],
        )?;
        encode(&ide_setup::summarize(&setup)?)
    }

    fn tracked_inputs(&self) -> Result<TrackedInputs> {
        if let Some(paths) = &self.tracked_paths {
            return Ok((paths.clone(), None));
        }
        let repository =
            git2::Repository::open(&self.root).map_err(|error| Error::new(error.to_string()))?;
        let mut index = repository
            .index()
            .map_err(|error| Error::new(error.to_string()))?;
        let path = index
            .path()
            .ok_or_else(|| Error::new("The project has no persistent Git index.".into()))?
            .to_path_buf();
        let digest = Digest::of(&std::fs::read(&path)?);
        index
            .read(true)
            .map_err(|error| Error::new(error.to_string()))?;
        check(
            !index.has_conflicts(),
            "Resolve Git index conflicts before verification.",
        )?;
        let mut paths = BTreeSet::new();
        for entry in index.iter() {
            let name = String::from_utf8(entry.path)
                .map_err(|_| Error::new("Tracked paths must be valid UTF-8.".into()))?;
            rovik_harness::scope::validate_path(&name)?;
            paths.insert(name);
        }
        Ok((paths, Some((path, digest))))
    }

    // Hash every included byte on each observation. This deliberately has no
    // mtime cache: timestamps are only race fences, never freshness evidence.
    // Publishing is reserved for explicit inspection/verification input capture.
    fn observe_snapshot(
        &self,
        untracked: &BTreeSet<String>,
        publish: Option<&Ledger>,
    ) -> Result<Snapshot> {
        use rovik_harness::{controller::Integration, snapshot::FileEntry};

        let integration = self.integration()?;
        let revision = integration.current_revision(&self.repository_id)?;
        let repository =
            git2::Repository::open(&self.root).map_err(|error| Error::new(error.to_string()))?;
        let head_before = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .map_err(|error| Error::new(error.to_string()))?
            .id();
        let commit = repository
            .find_commit(
                git2::Oid::from_str(&revision).map_err(|error| Error::new(error.to_string()))?,
            )
            .map_err(|error| Error::new(error.to_string()))?;
        let tree = commit
            .tree()
            .map_err(|error| Error::new(error.to_string()))?;
        let mut baseline_paths = BTreeSet::new();
        let mut invalid = None;
        let walked = tree.walk(git2::TreeWalkMode::PreOrder, |prefix, entry| {
            let result = (|| -> Result<()> {
                let name = entry.name().map_err(|_| Error::new("Tracked paths must be valid UTF-8.".into()))?;
                let path = format!("{prefix}{name}");
                rovik_harness::scope::validate_path(&path)?;
                check(!path.split('/').any(|part| part.eq_ignore_ascii_case(".git")),
                    "Git administrative files cannot enter a source snapshot.")?;
                if entry.kind() == Some(git2::ObjectType::Tree) {
                    return Ok(());
                }
                check([0o100644, 0o100755].contains(&entry.filemode()),
                    "Source snapshots require regular files; symlinks and submodules need an explicit adapter.")?;
                baseline_paths.insert(path);
                Ok(())
            })();
            match result {
                Ok(()) => git2::TreeWalkResult::Ok,
                Err(error) => {
                    invalid = Some(error);
                    git2::TreeWalkResult::Abort
                }
            }
        });
        if let Some(error) = invalid {
            return Err(error);
        }
        walked.map_err(|error| Error::new(error.to_string()))?;
        rovik_harness::scope::validate_tree_paths(baseline_paths.iter().map(String::as_str), true)?;
        let (tracked, index_before) = self.tracked_inputs()?;
        // A prior private capture is an index of reusable source references,
        // not freshness evidence. Every current byte is still hashed below;
        // the completed published snapshot validates every reused CAS blob.
        let previous: Option<Snapshot> = if let Some(ledger) = publish {
            let digest: Option<String> = self
                .database()?
                .query_row(
                    "SELECT digest FROM captures ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            digest
                .map(|digest| wire::decode(&ledger.read(&Digest::parse(&digest)?)?))
                .transpose()?
        } else {
            None
        };
        let known: BTreeMap<_, _> = previous
            .iter()
            .flat_map(|snapshot| snapshot.files.values())
            .map(|file| (file.content.digest.clone(), &file.content))
            .collect();
        let paths: BTreeSet<_> = tracked.union(untracked).cloned().collect();
        let mut files = BTreeMap::new();
        for path in paths {
            rovik_harness::scope::validate_path(&path)?;
            check(
                !path
                    .split('/')
                    .any(|part| part.eq_ignore_ascii_case(".git")),
                "Git administrative files cannot enter a source snapshot.",
            )?;
            let file = self.root.join(&path);
            let before = match std::fs::symlink_metadata(&file) {
                Ok(metadata) => metadata,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && (baseline_paths.contains(&path) || !untracked.contains(&path)) =>
                {
                    continue
                }
                Err(error) => return Err(error.into()),
            };
            check(
                before.is_file()
                    && !before.file_type().is_symlink()
                    && file.canonicalize()?.starts_with(&self.root),
                "Snapshot inputs must be regular files inside the project.",
            )?;
            let mut parent = file.parent();
            while let Some(directory) = parent.filter(|parent| *parent != self.root) {
                check(
                    !std::fs::symlink_metadata(directory)?
                        .file_type()
                        .is_symlink(),
                    "Snapshot inputs cannot traverse a symlink.",
                )?;
                parent = directory.parent();
            }
            super::source::require_hydrated(&file, &path, before.len())?;
            let content = super::source::observe(&file, &path)?;
            if let Some(ledger) = publish {
                if known.get(&content.digest).copied() != Some(&content) {
                    super::source::publish(ledger, &file, &path, &content)?;
                }
            }
            let after = std::fs::symlink_metadata(&file)?;
            check(
                after.is_file()
                    && !after.file_type().is_symlink()
                    && before.len() == content.byte_count
                    && before.len() == after.len()
                    && before.modified()? == after.modified()?
                    && before.permissions() == after.permissions(),
                "The working tree changed during capture. Inspect the project again.",
            )?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                if before.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                }
            };
            #[cfg(not(unix))]
            let mode = 0o100644;
            files.insert(path, FileEntry { mode, content });
        }
        rovik_harness::scope::validate_tree_paths(files.keys().map(String::as_str), true)?;
        if let Some((path, digest)) = index_before {
            check(
                Digest::of(&std::fs::read(path)?) == digest,
                "The Git index changed during capture. Inspect the project again.",
            )?;
        }
        check(
            integration.current_revision(&self.repository_id)? == revision
                && repository
                    .head()
                    .and_then(|head| head.peel_to_commit())
                    .map_err(|error| Error::new(error.to_string()))?
                    .id()
                    == head_before,
            "The Git baseline changed during capture. Inspect the project again.",
        )?;
        let deletions = baseline_paths
            .into_iter()
            .filter(|path| !files.contains_key(path))
            .collect();
        Ok(Snapshot {
            schema_version: 1,
            base_revision: revision,
            files,
            deletions,
            dependency_assets: vec![],
        })
    }

    fn capture(&self, untracked: &BTreeSet<String>) -> Result<Snapshot> {
        let ledger = self.ledger()?;
        let snapshot = self.observe_snapshot(untracked, Some(&ledger))?;
        snapshot.validate(&ledger, true)?;
        let digest = snapshot.digest()?;
        let database = self.database()?;
        let captured: bool = database.query_row(
            "SELECT EXISTS(SELECT 1 FROM captures WHERE digest=?1)",
            [digest.as_str()],
            |row| row.get(0),
        )?;
        if captured {
            let previous: Snapshot = wire::decode(&ledger.read(&digest)?)?;
            check(
                previous == snapshot,
                "Captured snapshot identity changed in private storage.",
            )?;
        } else {
            ledger.store_json("snapshot", &snapshot, runner::now_ms()?)?;
            database.execute(
                "INSERT OR IGNORE INTO captures(digest,untracked) VALUES(?1,?2)",
                params![
                    digest.as_str(),
                    serde_json::to_string(untracked).map_err(|e| Error::new(e.to_string()))?
                ],
            )?;
        }
        Ok(snapshot)
    }
    fn captured_selection(&self, digest: &Digest) -> Result<BTreeSet<String>> {
        let raw: String = self
            .database()?
            .query_row(
                "SELECT untracked FROM captures WHERE digest=?1",
                [digest.as_str()],
                |r| r.get(0),
            )
            .map_err(|_| {
                Error::new(
                    "Inspect the current project before selecting its verification setup.".into(),
                )
            })?;
        wire::decode(raw.as_bytes())
    }

    fn current_capture(&self, expected: &Digest, untracked: &BTreeSet<String>) -> Result<Snapshot> {
        let snapshot = self.capture(untracked)?;
        check(
            snapshot.digest()? == *expected,
            "The project changed after review. Save buffers and inspect the current changes again.",
        )?;
        Ok(snapshot)
    }

    fn backend(&self, engine: &Engine) -> Result<ContainerBackend> {
        match engine.kind {
            EngineKind::Docker => ContainerBackend::local(Path::new(&engine.executable)),
            EngineKind::Podman => {
                ContainerBackend::podman(Path::new(&engine.executable), &self.state.join("podman"))
            }
        }
    }

    fn controller(&self, engine: Option<&Engine>) -> Result<Controller> {
        Ok(Controller {
            ledger: self.ledger()?,
            backend: match engine {
                Some(engine) => self.backend(engine)?,
                None => ContainerBackend::management(),
            },
        })
    }

    fn dispatch(&self, request: Operation) -> Result<Value> {
        match request {
            Operation::ArchitectureFeedback { run_id } => self.architecture_feedback(&run_id),
            Operation::ApplicationState => self.application_guard(),
            Operation::ReleaseApplication {
                run_id,
                subject_digest,
                acknowledged,
            } => self.release_application(&run_id, &Digest::parse(&subject_digest)?, acknowledged),
            Operation::PrepareApplication { run_id } => self.prepare_application(&run_id),
            Operation::ApplyApplication {
                run_id,
                subject_digest,
                saved_buffers,
            } => self.apply_application(
                &run_id,
                &Digest::parse(&subject_digest)?,
                saved_buffers,
                false,
            ),
            Operation::RecoverApplication {
                run_id,
                subject_digest,
                saved_buffers,
            } => self.apply_application(
                &run_id,
                &Digest::parse(&subject_digest)?,
                saved_buffers,
                true,
            ),
            Operation::InspectPlan { plan_path } => encode(&self.read_plan(&plan_path)?),
            Operation::PrepareWorkspace { input } => {
                let ledger = self.ledger()?;
                let baseline = self.integration()?.snapshot(&ledger)?;
                let input = decode(input)?;
                let setup = super::ide_prepare::build(
                    &self.root,
                    &ledger,
                    &self.repository_id,
                    &baseline,
                    &input,
                )?;
                self.register_setup(setup)?;
                self.inspect(BTreeSet::new())
            }
            Operation::ImplementPlan {
                plan_path,
                expected_plan_digest,
                expected_snapshot_digest,
                untracked,
                saved_buffers,
                plan_criteria_reviewed,
                provider,
            } => {
                check(saved_buffers && plan_criteria_reviewed, "Save all included buffers and review that protected acceptance criteria cover the complete plan.")?;
                self.start_implementation(
                    &plan_path,
                    &Digest::parse(&expected_plan_digest)?,
                    &Digest::parse(&expected_snapshot_digest)?,
                    untracked.into_iter().collect(),
                    provider,
                )
            }
            Operation::ResumeImplementation { run_id, provider } => {
                self.resume_implementation(&run_id, provider, None)
            }
            Operation::DraftPlan {
                run_id,
                role,
                provider,
            } => self.resume_implementation(&run_id, provider, Some(role)),
            Operation::ReviewChanges { run_id } => self.review_changes(&run_id),
            Operation::ExportReview { run_id } => self.export_review(&run_id),
            Operation::PrepareDelivery {
                run_id,
                destination_reference,
                expected_head,
                message,
            } => self.prepare_delivery(&run_id, &destination_reference, &expected_head, &message),
            Operation::Inspect { untracked } => self.inspect(untracked.into_iter().collect()),
            Operation::ImportSetup { path } => {
                let path = Path::new(&path);
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.root.join(path)
                };
                let ledger = self.ledger()?;
                let baseline = self.integration()?.snapshot(&ledger)?;
                let setup = ide_setup::import_setup_with_baseline(
                    &ledger,
                    &path,
                    &self.repository_id,
                    &baseline,
                )?;
                self.register_setup(setup)?;
                self.inspect(BTreeSet::new())
            }
            Operation::Configure {
                setup_id,
                engine,
                expected_snapshot_digest,
            } => {
                let snapshot_digest = Digest::parse(&expected_snapshot_digest)?;
                let untracked = self.captured_selection(&snapshot_digest)?;
                self.current_capture(&snapshot_digest, &untracked)?;
                let setup_id = Digest::parse(&setup_id)?;
                let setup = self.load_setup(&setup_id)?;
                setup
                    .authority
                    .authorize(&Principal::current()?, "operate")?;
                let baseline = self.integration()?.snapshot(&self.ledger()?)?;
                check(baseline.digest()? == setup.base.digest()?, "The prepared baseline differs from this project's HEAD. Prepare a new setup for the current revision.")?;
                let backend = self.backend(&engine)?;
                backend.require_environment(&setup.environment)?;
                backend.validate_image(&setup.environment)?;
                let config = Configuration {
                    setup_id,
                    engine,
                    snapshot_digest,
                };
                self.database()?.execute("INSERT INTO configuration(singleton,value) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET value=excluded.value", [serde_json::to_string(&config).map_err(|e| Error::new(e.to_string()))?])?;
                self.inspect(untracked)
            }
            Operation::Verify {
                untracked,
                expected_snapshot_digest,
                saved_buffers,
            } => {
                check(
                    saved_buffers,
                    "Save all included editor buffers before verifying changes.",
                )?;
                self.start_verification(
                    untracked.into_iter().collect(),
                    Digest::parse(&expected_snapshot_digest)?,
                )
            }
            Operation::Status { run_id } | Operation::Approvals { run_id } => {
                self.require_run_context(&self.run_input(&run_id)?)?;
                self.status(&run_id)
            }
            Operation::Approve { run_id, request } => {
                self.require_run_context(&self.run_input(&run_id)?)?;
                let controller = self.controller(None)?;
                let setup = controller.setup(&run_id)?;
                let request: ApprovalRequest = decode(request)?;
                check(
                    request.run_id.as_deref() == Some(run_id.as_str())
                        && request.scope_digest == Digest::of_json(&setup.contract.scope)?
                        && request.policy_digest == Digest::of_json(&setup.policy)?,
                    "Approval must name this exact run, scope and policy.",
                )?;
                let mut pending = controller.pending_approvals(&run_id)?;
                pending.extend(self.application_requirements(&controller, &run_id)?);
                check(
                    pending.iter().any(|pending| {
                        pending.subject_digest == request.subject_digest
                            && pending.approval_type == request.approval_type
                    }),
                    "The requested approval is not a pending decision for this run.",
                )?;
                controller.ledger.grant(
                    &Principal::current()?,
                    &setup.authority,
                    request,
                    runner::now_ms()?,
                )?;
                self.status(&run_id)
            }
            Operation::Cancel { run_id } => {
                self.require_run_context(&self.run_input(&run_id)?)?;
                self.controller(None)?
                    .cancel(&Principal::current()?, &run_id)?;
                if let Some(cancel) = active_runs()
                    .lock()
                    .map_err(|_| Error::new("Verification worker registry is unavailable.".into()))?
                    .get(&(self.state.clone(), run_id.clone()))
                {
                    cancel.cancel();
                }
                self.status(&run_id)
            }
            Operation::Resume { run_id } => {
                let input = self.run_input(&run_id)?;
                check(
                    input.plan.is_none(),
                    "Resume this implementation with its selected provider credentials.",
                )?;
                self.require_run_context(&input)?;
                let target = self.execution_project(&input);
                target.current_capture(&input.snapshot_digest, &input.untracked)?;
                check(
                    self.configuration()?
                        .map(|config| Digest::of_json(&config))
                        .transpose()?
                        .as_ref()
                        == Some(&input.configuration_digest),
                    "The reviewed configuration changed. Start a new verification run.",
                )?;
                check(
                    !self.is_running(&run_id)?,
                    "This verification is already running.",
                )?;
                let controller = self.controller(Some(&input.configuration.engine))?;
                let setup = controller.setup(&run_id)?;
                setup
                    .authority
                    .authorize(&Principal::current()?, "operate")?;
                check(
                    controller.pending_approvals(&run_id)?.is_empty(),
                    "Review all pending decisions before resuming verification.",
                )?;
                check(
                    matches!(
                        controller.ledger.run(&run_id)?.state,
                        State::Blocked | State::Intake
                    ),
                    "Only a blocked verification or newly reviewed intake can resume.",
                )?;
                target.launch(&run_id, input)?;
                self.status(&run_id)
            }
            Operation::Recover { run_id } => {
                let ownership = ownership(&self.state)?;
                let _ownership = ownership
                    .lock()
                    .map_err(|_| Error::new("Verification ownership is unavailable.".into()))?;
                let input = self.run_input(&run_id)?;
                self.require_run_context(&input)?;
                check(
                    !self.is_running(&run_id)?,
                    "Cancel and wait for the active worker before recovering interrupted jobs.",
                )?;
                self.controller(Some(&input.configuration.engine))?
                    .recover(&Principal::current()?, &run_id)?;
                self.database()?
                    .execute("UPDATE runs SET active=0,owner=NULL WHERE id=?1", [&run_id])?;
                self.status(&run_id)
            }
        }
    }

    /// Inspect manifests, preparation choices, host capabilities and recent immutable runs.
    pub fn inspect(&self, selection: BTreeSet<String>) -> Result<Value> {
        let snapshot = self.capture(&selection)?;
        let ledger = self.ledger()?;
        let inventory = rovik_harness::onboarding::inspect(&ledger, &snapshot)?;
        let database = self.database()?;
        let mut query = database.prepare("SELECT id FROM setups ORDER BY rowid DESC")?;
        let ids = query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut setups = Vec::new();
        for id in ids {
            setups.push(ide_setup::summarize(
                &self.load_setup(&Digest::parse(&id)?)?,
            )?);
        }
        let configuration = self.configuration()?.map(|config| -> Result<Value> {
            let summary = ide_setup::summarize(&self.load_setup(&config.setup_id)?)?;
            Ok(json!({"digest":Digest::of_json(&config)?,"setupId":config.setup_id,"label":summary.label,"engine":config.engine}))
        }).transpose()?;
        let mut query =
            database.prepare("SELECT id FROM runs ORDER BY created_ms DESC, id DESC LIMIT 30")?;
        let ids = query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut runs = Vec::new();
        let mut observations = BTreeMap::from([(
            (self.root.clone(), selection.clone()),
            Ok(snapshot.digest()?),
        )]);
        for id in ids {
            let input = self.load_run_input(&id)?;
            if self.root != input.execution_root
                || self.baseline_reference != input.baseline_reference
            {
                continue;
            }
            let target = self.execution_project(&input);
            let observed = observations
                .entry((target.root.clone(), input.untracked.clone()))
                .or_insert_with(|| {
                    target
                        .observe_snapshot(&input.untracked, None)
                        .and_then(|snapshot| snapshot.digest())
                })
                .clone();
            runs.push(target.status_observed(&id, input, observed)?);
        }
        let repository =
            git2::Repository::open(&self.root).map_err(|e| Error::new(e.to_string()))?;
        let mut options = git2::StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false);
        let statuses = repository
            .statuses(Some(&mut options))
            .map_err(|e| Error::new(e.to_string()))?;
        let mut untracked: BTreeSet<_> = statuses
            .iter()
            .filter(|entry| entry.status().contains(git2::Status::WT_NEW))
            .map(|entry| {
                entry.path().map(str::to_owned).map_err(|error| {
                    Error::new(format!("Untracked path is not valid UTF-8: {error}"))
                })
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if let Some(tracked) = &self.tracked_paths {
            let head = repository
                .head()
                .and_then(|head| head.peel_to_tree())
                .map_err(|error| Error::new(error.to_string()))?;
            let mut invalid = false;
            let walked = head.walk(git2::TreeWalkMode::PreOrder, |prefix, entry| {
                let Ok(name) = entry.name() else {
                    invalid = true;
                    return git2::TreeWalkResult::Abort;
                };
                if entry.kind() == Some(git2::ObjectType::Tree) {
                    return git2::TreeWalkResult::Ok;
                }
                let path = format!("{prefix}{name}");
                if !tracked.contains(&path) {
                    untracked.insert(path);
                }
                git2::TreeWalkResult::Ok
            });
            check(
                !invalid,
                "Shadow snapshot contains a path that is not valid UTF-8.",
            )?;
            walked.map_err(|error| Error::new(error.to_string()))?;
        }
        let engines: Vec<_> = [
            (EngineKind::Docker, "/usr/bin/docker"),
            (EngineKind::Docker, "/usr/local/bin/docker"),
            (EngineKind::Podman, "/usr/bin/podman"),
            (EngineKind::Podman, "/usr/local/bin/podman"),
        ]
        .into_iter()
        .filter(|(_, path)| Path::new(path).is_file())
        .map(|(kind, path)| Engine {
            kind,
            executable: path.into(),
        })
        .collect();
        let supported = cfg!(target_os = "linux");
        let mut issues = inventory.unresolved.clone();
        if !supported {
            issues
                .push("Select a connected Linux execution host for isolated verification.".into());
        } else if engines.is_empty() {
            issues.push("Install Docker or rootless Podman on the selected execution host, then refresh its capabilities.".into());
        }
        if setups.is_empty() {
            issues.push("Import a reviewed setup with protected acceptance tests, pinned tools and dependencies.".into());
        }
        Ok(
            json!({"projectId":self.repository_id,"snapshotDigest":snapshot.digest()?,"inventory":inventory,
            "untracked":untracked,"selectedUntracked":selection,"host":{"supported":supported,"platform":std::env::consts::OS,"engines":engines,
            "reason":if supported {None} else {Some("Strict execution requires a Linux host.")}},
            "configuration":configuration,"setups":setups,"runs":runs,"issues":issues}),
        )
    }

    fn start_verification(&self, untracked: BTreeSet<String>, expected: Digest) -> Result<Value> {
        let configuration = self.configuration()?.ok_or_else(|| {
            Error::new("Select and review a prepared verification setup first.".into())
        })?;
        let candidate = self.current_capture(&expected, &untracked)?;
        let setup = self.verification_setup(&configuration, candidate)?;
        setup
            .authority
            .authorize(&Principal::current()?, "operate")?;
        let now = runner::now_ms()?;
        let mut controller = self.controller(Some(&configuration.engine))?;
        controller.create(&setup, now)?;
        let input = RunInput {
            plan: None,
            execution_root: self.root.clone(),
            baseline_reference: self.baseline_reference.clone(),
            tracked_paths: self.tracked_paths.clone(),
            configuration_digest: Digest::of_json(&configuration)?,
            configuration,
            snapshot_digest: expected,
            untracked,
        };
        self.database()?.execute(
            "INSERT INTO runs(id,input,created_ms) VALUES(?1,?2,?3)",
            params![
                setup.run_id,
                serde_json::to_string(&input).map_err(|e| Error::new(e.to_string()))?,
                i64::try_from(now).map_err(|_| Error::new("Run timestamp overflow.".into()))?
            ],
        )?;
        // First return concrete approval subjects. The UI resumes the run only after
        // the operator has reviewed those exact setup/oracle/baseline identities.
        if controller.pending_approvals(&setup.run_id)?.is_empty() {
            self.launch(&setup.run_id, input)?;
        }
        self.status(&setup.run_id)
    }

    fn verification_setup(
        &self,
        configuration: &Configuration,
        candidate: Snapshot,
    ) -> Result<TaskSetup> {
        let mut setup = self.load_setup(&configuration.setup_id)?;
        check(
            self.integration()?.snapshot(&self.ledger()?)?.digest()? == setup.base.digest()?,
            "The destination revision changed. Prepare and approve a setup for the new baseline.",
        )?;
        let now = runner::now_ms()?;
        check(
            setup.deadline_ms > now,
            "The prepared task deadline expired. Review a new execution budget and deadline.",
        )?;
        setup.run_id = format!("ide-{}", uuid::Uuid::new_v4());
        setup.plan.run_id = setup.run_id.clone();
        setup.initial_candidate = Some(candidate);
        setup.validate(&self.ledger()?)?;
        Ok(setup)
    }

    fn run_input(&self, run_id: &str) -> Result<RunInput> {
        let input = self.load_run_input(run_id)?;
        self.require_run_context(&input)?;
        Ok(input)
    }

    fn load_run_input(&self, run_id: &str) -> Result<RunInput> {
        let raw: String = self
            .database()?
            .query_row("SELECT input FROM runs WHERE id=?1", [run_id], |r| r.get(0))
            .map_err(|_| Error::new("Verification run does not belong to this project.".into()))?;
        wire::decode(raw.as_bytes())
    }

    fn require_run_context(&self, input: &RunInput) -> Result<()> {
        check(self.root == input.execution_root && self.baseline_reference == input.baseline_reference,
            "This verification belongs to another execution workspace. Open its owning chat to manage it.")
    }

    fn execution_project(&self, input: &RunInput) -> Self {
        Self {
            root: input.execution_root.clone(),
            state: self.state.clone(),
            repository_id: self.repository_id.clone(),
            baseline_reference: input.baseline_reference.clone(),
            tracked_paths: self.tracked_paths.clone(),
        }
    }

    fn is_running(&self, run_id: &str) -> Result<bool> {
        Ok(active_runs()
            .lock()
            .map_err(|_| Error::new("Verification worker registry is unavailable.".into()))?
            .contains_key(&(self.state.clone(), run_id.into())))
    }

    fn launch(&self, run_id: &str, input: RunInput) -> Result<()> {
        self.launch_with_provider(run_id, input, None, None)
    }

    fn launch_with_provider(
        &self,
        run_id: &str,
        input: RunInput,
        provider: Option<rovik_protocol::harness::ImplementationProvider>,
        role: Option<rovik_protocol::harness::DraftRole>,
    ) -> Result<()> {
        let ownership = ownership(&self.state)?;
        let _ownership = ownership
            .lock()
            .map_err(|_| Error::new("Verification ownership is unavailable.".into()))?;
        let mut active = active_runs()
            .lock()
            .map_err(|_| Error::new("Verification worker registry is unavailable.".into()))?;
        check(
            !active.contains_key(&(self.state.clone(), run_id.into())),
            "This verification is already running.",
        )?;
        let owner = uuid::Uuid::new_v4().to_string();
        let claimed = self.database()?.execute("UPDATE runs SET active=1,owner=?2,error=NULL WHERE id=?1 AND active=0", params![run_id, owner])
            .map_err(|_| Error::new("Another verification is active or requires recovery. Cancel or recover it before starting more work.".into()))?;
        check(
            claimed == 1,
            "This verification is owned by another controller; recover it before resuming.",
        )?;
        let cancel = Cancellation::default();
        active.insert((self.state.clone(), run_id.into()), cancel.clone());
        let project = self.clone();
        let run = run_id.to_string();
        let _worker = crate::rt::spawn_blocking(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Value> {
                    let mut controller = project.controller(Some(&input.configuration.engine))?;
                    let integration = project.integration()?;
                    if controller.ledger.run(&run)?.state == State::Blocked {
                        controller.resume(&Principal::current()?, &run)?;
                    }
                    match provider {
                        Some(selection) => {
                            let mut provider = plan::provider(&selection)?;
                            if let Some(role) = role {
                                plan::draft(&mut controller, &run, role, &mut provider, &cancel)
                            } else {
                                encode(&controller.execute(
                                    &run,
                                    &mut provider,
                                    &integration,
                                    &cancel,
                                )?)
                            }
                        }
                        None => encode(&controller.execute(
                            &run,
                            &mut OfflineProvider,
                            &integration,
                            &cancel,
                        )?),
                    }
                }))
                .unwrap_or_else(|_| {
                    Err(Error::new(
                    "Verification controller interrupted unexpectedly; recover before resuming."
                        .into(),
                ))
                });
            let error = match &result {
                Err(error) => Some(error.message.clone()),
                Ok(report) => report
                    .get("reasons")
                    .and_then(Value::as_array)
                    .filter(|reasons| !reasons.is_empty())
                    .map(|reasons| {
                        reasons
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    }),
            };
            if let Err(failure) = project.database().and_then(|database| {
                database
                    .execute(
                        "UPDATE runs SET active=0,owner=NULL,error=?2 WHERE id=?1 AND owner=?3",
                        params![run, error, owner],
                    )
                    .map(|_| ())
                    .map_err(Error::from)
            }) {
                log::error!(
                    "Cannot persist verification worker completion: {}",
                    failure.message
                );
            }
            match active_runs().lock() {
                Ok(mut active) => {
                    active.remove(&(project.state, run));
                }
                Err(_) => log::error!("Cannot remove completed verification worker from registry"),
            }
        });
        Ok(())
    }

    /// Recompute status from the trusted ledger and the actual current project tree.
    pub fn status(&self, run_id: &str) -> Result<Value> {
        let input = self.run_input(run_id)?;
        let target = self.execution_project(&input);
        let observed = target
            .observe_snapshot(&input.untracked, None)
            .and_then(|snapshot| snapshot.digest());
        target.status_observed(run_id, input, observed)
    }

    fn status_observed(
        &self,
        run_id: &str,
        input: RunInput,
        current: Result<Digest>,
    ) -> Result<Value> {
        let controller = self.controller(None)?;
        let run = controller.ledger.run(run_id)?;
        let setup = controller.setup(run_id)?;
        let running = self.is_running(run_id)?;
        let (active, error): (bool, Option<String>) = self.database()?.query_row(
            "SELECT active,error FROM runs WHERE id=?1",
            [run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let orphaned = active && !running;
        let candidate = controller.current_candidate(run_id)?;
        let configuration_current = self
            .configuration()?
            .map(|config| Digest::of_json(&config))
            .transpose()?
            .as_ref()
            == Some(&input.configuration_digest);
        let current_digest = match &current {
            Ok(digest) => Some(digest.clone()),
            Err(_) => None,
        };
        let plan_current = input.plan.as_ref().is_none_or(|binding| {
            self.read_plan(&binding.path)
                .is_ok_and(|plan| plan.digest == binding.digest)
        });
        let (application, application_receipt, application_current) =
            self.application_fields(run_id)?;
        let stale = !configuration_current
            || !plan_current
            || (!application_current && current_digest.as_ref() != Some(&input.snapshot_digest));
        let worker_error = error.is_some();
        let mut reasons: Vec<String> = error.into_iter().collect();
        if !application_current {
            if let Err(error) = current {
                reasons.push(error.message);
            }
        }
        let gate = if candidate.is_some() {
            match self
                .integration()
                .and_then(|integration| controller.verify(run_id, &integration))
            {
                Ok(gate) => Some(gate),
                Err(error) => {
                    reasons.push(error.message);
                    None
                }
            }
        } else {
            None
        };
        if let Some(gate) = &gate {
            reasons.extend(gate.reasons.iter().cloned());
        }
        if orphaned {
            reasons.push("This run is owned by another controller or was interrupted. Recovery interrupts its registered jobs; unknown usage must be reconciled before resuming.".into());
        }
        if stale {
            reasons.push("Project inputs or reviewed configuration changed. Verify the current changes in a new run.".into());
        }
        let policy_digest = Digest::of_json(&setup.policy)?;
        let scope_digest = Digest::of_json(&setup.contract.scope)?;
        let mut pending = Vec::new();
        let mut requirements = controller.pending_approvals(run_id)?;
        requirements.extend(self.application_requirements(&controller, run_id)?);
        for requirement in requirements {
            let request = ApprovalRequest {
                run_id: Some(run_id.into()),
                subject_digest: requirement.subject_digest.clone(),
                approval_type: requirement.approval_type.clone(),
                policy_digest: policy_digest.clone(),
                scope_digest: scope_digest.clone(),
                expires_ms: None,
            };
            let reference = controller.ledger.reference(&requirement.subject_digest)?;
            let subject: Value =
                wire::decode(&controller.ledger.read(&requirement.subject_digest)?)?;
            let mut assets = Vec::new();
            if reference.kind == "snapshot" {
                let snapshot: Snapshot = decode(subject.clone())?;
                let mut remaining = 1024 * 1024;
                for (path, file) in snapshot.files {
                    let mut preview = json!({"path":path,"digest":file.content.digest});
                    if file.content.byte_count <= 128 * 1024 && file.content.byte_count <= remaining
                    {
                        let bytes = controller.ledger.read(&file.content.digest)?;
                        remaining -= bytes.len() as u64;
                        match String::from_utf8(bytes) {
                            Ok(content) => preview["content"] = Value::String(content),
                            Err(_) => preview["omitted"] = json!("Binary asset; inspect the original prepared bundle before approval."),
                        }
                    } else {
                        preview["omitted"] = json!("Asset exceeds preview bounds; inspect the original prepared bundle before approval.");
                    }
                    assets.push(preview);
                }
            }
            pending.push(json!({"approvalType":requirement.approval_type,"subjectDigest":requirement.subject_digest,"scopeDigest":scope_digest,"policyDigest":policy_digest,
                "summary":format!("Review the exact {} decision for this verification run.",requirement.approval_type),"request":request,"subject":subject,"subjectKind":reference.kind,"assets":assets}));
        }
        let status = if application_current
            && run.state == State::Accepted
            && gate
                .as_ref()
                .is_some_and(|gate| gate.verdict == Verdict::Ready)
        {
            "applied"
        } else if stale {
            "stale"
        } else if running {
            "running"
        } else if run.state == State::Cancelled {
            "cancelled"
        } else if run.state == State::Accepted
            && gate
                .as_ref()
                .is_some_and(|gate| gate.verdict == Verdict::Ready)
        {
            "verified"
        } else if matches!(run.state, State::Rejected | State::Stopped)
            || gate
                .as_ref()
                .is_some_and(|gate| gate.verdict == Verdict::Failed)
        {
            "failed"
        } else {
            "blocked"
        };
        let operations = controller.ledger.operations(run_id)?;
        let mut checks = Vec::new();
        for operation in &operations {
            if operation.reservation.contains_key("checks") {
                if let Some(digest) = &operation.output_digest {
                    let result: rovik_harness::evidence::CheckResult =
                        wire::decode(&controller.ledger.read(digest)?)?;
                    result.validate()?;
                    checks.push(result);
                }
            }
        }
        let budgets = controller.ledger.budgets(run_id)?;
        let unsettled = operations
            .iter()
            .any(|operation| matches!(operation.state.as_str(), "running" | "abandoned"))
            || budgets
                .values()
                .any(|account| account.reserved != 0 || account.spent > account.limit);
        if unsettled && !running {
            reasons.push("Interrupted execution or unknown usage needs recovery and accounting reconciliation before resuming.".into());
        }
        Ok(
            json!({"runId":run_id,"kind":if input.plan.is_some(){"implementation"}else{"verification"},"planDigest":input.plan.as_ref().map(|p| &p.digest),"providerBinding":input.plan.as_ref().map(|p| &p.provider),"drafts":plan::drafts(&controller, run_id)?,"workspace":self.root,"state":run.state,"status":status,"snapshotDigest":input.snapshot_digest,
            "application":application,"applicationReceipt":application_receipt,
            "candidateDigest":candidate.map(|candidate|candidate.digest()).transpose()?,"configurationDigest":input.configuration_digest,
            "reason":reasons.first(),"reasons":reasons,"pendingApprovals":pending,"checks":checks,"gate":gate,
            "budgets":budgets,"operations":operations,
            "canCancel":!run.state.terminal(),"canRecover":!running && !run.state.terminal() && (orphaned || worker_error || operations.iter().any(|operation|matches!(operation.state.as_str(),"running"|"abandoned"))),
            "canResume":!running && !orphaned && !stale && !unsettled && pending.is_empty() && matches!(run.state,State::Intake|State::Blocked)}),
        )
    }
}

struct OfflineProvider;
impl rovik_harness::provider::Provider for OfflineProvider {
    fn supports_role(&self, _: rovik_harness::provider::Role) -> bool {
        false
    }
    fn generate(
        &mut self,
        _: &rovik_harness::provider::ModelRequest,
        _: &Cancellation,
    ) -> Result<rovik_harness::provider::ModelResponse> {
        Err(Error::classified(rovik_harness::FailureCategory::Capability,
            "This is model-free verification. Use verified implementation when code repair or model review is required."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_harness::fixtures;

    pub(super) fn project() -> (tempfile::TempDir, IdeProject) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        std::fs::create_dir_all(root.join("implementations/quota")).unwrap();
        std::fs::write(
            root.join("implementations/quota/lib.rs"),
            fixtures::QUOTA_RUST
                .replace("requested > self.remaining", "requested >= self.remaining"),
        )
        .unwrap();
        let repository = git2::Repository::init(&root).unwrap();
        let mut index = repository.index().unwrap();
        index
            .add_path(Path::new("implementations/quota/lib.rs"))
            .unwrap();
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Verification tests", "tests@rovik.invalid").unwrap();
        repository
            .commit(Some("HEAD"), &signature, &signature, "baseline", &tree, &[])
            .unwrap();
        let project = IdeProject::open(&directory.path().join("private"), &root).unwrap();
        (directory, project)
    }

    pub(super) fn prepared(project: &IdeProject, image: Digest, executable: &str) -> Configuration {
        let ledger = project.ledger().unwrap();
        let baseline = project.integration().unwrap().snapshot(&ledger).unwrap();
        let mut setup =
            fixtures::quota_rust(&ledger, "prepared-task", image, &baseline.base_revision).unwrap();
        setup.repository_id = project.repository_id.clone();
        assert_eq!(setup.base.digest().unwrap(), baseline.digest().unwrap());
        let summary = project.register_setup(setup).unwrap();
        Configuration {
            setup_id: Digest::parse(summary["id"].as_str().unwrap()).unwrap(),
            engine: Engine {
                kind: EngineKind::Docker,
                executable: executable.into(),
            },
            snapshot_digest: project.capture(&BTreeSet::new()).unwrap().digest().unwrap(),
        }
    }

    #[test]
    fn inspection_keeps_large_images_and_hydrated_lfs_assets_in_exact_snapshots() {
        use std::io::{Seek, SeekFrom, Write};
        let (_directory, project) = project();
        let repository = git2::Repository::open(&project.root).unwrap();
        let mut index = repository.index().unwrap();
        std::fs::create_dir_all(project.root.join("assets")).unwrap();
        let mut assets = BTreeMap::new();
        for (path, size, lfs) in [
            ("assets/image.png", 65 * 1024 * 1024, false),
            ("assets/model.bin", 160 * 1024 * 1024, true),
        ] {
            let file = project.root.join(path);
            let mut output = std::fs::File::create(&file).unwrap();
            output.write_all(b"\x89PNG\r\n\x1a\n").unwrap();
            output.set_len(size).unwrap();
            drop(output);
            let content = super::super::source::observe(&file, path).unwrap();
            let (id, git_size) = if lfs {
                let hex = content.digest.hex();
                let cached = repository
                    .path()
                    .join("lfs/objects")
                    .join(&hex[..2])
                    .join(&hex[2..4])
                    .join(hex);
                std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
                std::fs::copy(&file, cached).unwrap();
                let pointer = format!(
                    "version https://git-lfs.github.com/spec/v1\noid {}\nsize {}\n",
                    content.digest, size
                );
                (
                    repository.blob(pointer.as_bytes()).unwrap(),
                    pointer.len() as u32,
                )
            } else {
                let mut writer = repository.blob_writer(None).unwrap();
                std::io::copy(&mut std::fs::File::open(&file).unwrap(), &mut writer).unwrap();
                (writer.commit().unwrap(), size as u32)
            };
            index
                .add(&git2::IndexEntry {
                    ctime: git2::IndexTime::new(0, 0),
                    mtime: git2::IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    mode: 0o100644,
                    uid: 0,
                    gid: 0,
                    file_size: git_size,
                    id,
                    flags: 0,
                    flags_extended: 0,
                    path: path.as_bytes().to_vec(),
                })
                .unwrap();
            assets.insert(path, content);
        }
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repository.head().unwrap().peel_to_commit().unwrap();
        let signature = git2::Signature::now("Asset tests", "tests@rovik.invalid").unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "large assets",
                &tree,
                &[&parent],
            )
            .unwrap();
        let index_before = std::fs::read(repository.path().join("index")).unwrap();
        let inspected = project.inspect(BTreeSet::new()).unwrap();
        let ledger = project.ledger().unwrap();
        let digest = Digest::parse(inspected["snapshotDigest"].as_str().unwrap()).unwrap();
        let captured: Snapshot = wire::decode(&ledger.read(&digest).unwrap()).unwrap();
        let baseline = project.integration().unwrap().snapshot(&ledger).unwrap();
        assert_eq!(
            captured, baseline,
            "LFS pointers must resolve to the same bytes as the checkout"
        );
        for (path, content) in &assets {
            assert_eq!(&captured.files[*path].content, content);
        }
        ledger.verify_artifacts().unwrap();
        let materialized = project.state.join("large-assets-materialized");
        captured.materialize(&ledger, &materialized).unwrap();
        for (path, content) in &assets {
            assert_eq!(
                super::super::source::observe(&materialized.join(path), path).unwrap(),
                *content
            );
        }
        assert_eq!(
            project.observe_snapshot(&BTreeSet::new(), None).unwrap(),
            captured
        );
        for path in assets.keys() {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .open(project.root.join(path))
                .unwrap();
            output.seek(SeekFrom::End(-1)).unwrap();
            output.write_all(b"x").unwrap();
        }
        let changed = project.observe_snapshot(&BTreeSet::new(), None).unwrap();
        for (path, content) in &assets {
            assert_ne!(
                changed.files[*path].content.digest, content.digest,
                "same-size asset edits must invalidate verification"
            );
        }
        assert_eq!(
            std::fs::read(repository.path().join("index")).unwrap(),
            index_before
        );
    }

    #[test]
    fn inspection_preserves_dirty_bytes_and_requires_explicit_untracked_selection() {
        let (_directory, project) = project();
        let index_before = std::fs::read(project.root.join(".git/index")).unwrap();
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            fixtures::QUOTA_RUST,
        )
        .unwrap();
        std::fs::write(
            project.root.join("new.py"),
            "print('selected explicitly')\n",
        )
        .unwrap();
        let ordinary = project.inspect(BTreeSet::new()).unwrap();
        assert!(ordinary["untracked"]
            .as_array()
            .unwrap()
            .contains(&json!("new.py")));
        let selected = project.inspect(["new.py".into()].into()).unwrap();
        assert_ne!(ordinary["snapshotDigest"], selected["snapshotDigest"]);
        let digest = Digest::parse(selected["snapshotDigest"].as_str().unwrap()).unwrap();
        assert_eq!(
            project.captured_selection(&digest).unwrap(),
            ["new.py".into()].into()
        );
        assert_eq!(
            std::fs::read(project.root.join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(
            std::fs::read_to_string(project.root.join("implementations/quota/lib.rs")).unwrap(),
            fixtures::QUOTA_RUST
        );
    }

    #[test]
    fn unsaved_buffers_and_changed_reviewed_snapshots_cannot_start_verification() {
        let (_directory, project) = project();
        let snapshot = project.capture(&BTreeSet::new()).unwrap().digest().unwrap();
        let error = project
            .dispatch(Operation::Verify {
                untracked: vec![],
                expected_snapshot_digest: snapshot.to_string(),
                saved_buffers: false,
            })
            .unwrap_err();
        assert!(error.message.contains("Save all included"));
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            "changed after review",
        )
        .unwrap();
        assert!(project
            .current_capture(&snapshot, &BTreeSet::new())
            .unwrap_err()
            .message
            .contains("changed after review"));
        assert!(project
            .status("forged-accepted-run")
            .unwrap_err()
            .message
            .contains("does not belong"));
    }

    #[test]
    fn every_verification_rebinds_its_plan_and_requires_new_run_approvals() {
        let (_directory, project) = project();
        let configuration = prepared(
            &project,
            Digest::of(b"unavailable image"),
            "/missing/docker",
        );
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            fixtures::QUOTA_RUST,
        )
        .unwrap();
        let candidate = project.capture(&BTreeSet::new()).unwrap();
        let first = project
            .verification_setup(&configuration, candidate.clone())
            .unwrap();
        let second = project
            .verification_setup(&configuration, candidate)
            .unwrap();
        assert_ne!(first.run_id, "prepared-task");
        assert_ne!(first.run_id, second.run_id);
        assert_eq!(first.plan.run_id, first.run_id);
        assert_eq!(second.plan.run_id, second.run_id);
        first.validate(&project.ledger().unwrap()).unwrap();
        second.validate(&project.ledger().unwrap()).unwrap();
        assert_eq!(first.budgets, second.budgets);
        assert_eq!(first.deadline_ms, second.deadline_ms);
        assert_eq!(
            first.oracle.digest().unwrap(),
            second.oracle.digest().unwrap()
        );
    }

    #[test]
    fn private_controller_storage_cannot_be_placed_in_the_candidate_repository() {
        let (_directory, project) = project();
        let error = IdeProject::open(&project.root.join("state"), &project.root)
            .err()
            .unwrap();
        assert!(error.message.contains("outside the project"));
        assert!(!project.root.join("state").exists());
    }

    #[test]
    fn nested_projects_are_directed_to_the_repository_root() {
        let (directory, project) = project();
        let nested = project.root.join("implementations/quota");
        let error = IdeProject::open_for_project(
            &directory.path().join("nested-state"),
            &nested,
            "nested-project",
        )
        .err()
        .unwrap();
        assert!(
            error.message.contains("Open the Git repository root"),
            "{error:?}"
        );
        assert!(!directory.path().join("nested-state").exists());
        IdeProject::open_for_project(
            &directory.path().join("root-state"),
            &project.root,
            "root-project",
        )
        .unwrap();
    }

    #[test]
    fn staged_additions_are_captured_and_later_edits_change_the_reviewed_snapshot() {
        let (_directory, project) = project();
        std::fs::write(project.root.join("new.rs"), "pub fn staged() {}\n").unwrap();
        let repository = git2::Repository::open(&project.root).unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("new.rs")).unwrap();
        index.write().unwrap();
        let snapshot = project.capture(&BTreeSet::new()).unwrap();
        assert!(snapshot.files.contains_key("new.rs"));
        std::fs::write(project.root.join("new.rs"), "pub fn changed() {}\n").unwrap();
        assert_ne!(
            snapshot.digest().unwrap(),
            project.capture(&BTreeSet::new()).unwrap().digest().unwrap()
        );
    }

    #[test]
    fn freshness_hashes_current_bytes_without_publishing_or_trusting_timestamps() {
        let (_directory, project) = project();
        let original = project.capture(&BTreeSet::new()).unwrap();
        let ledger = project.ledger().unwrap();
        #[cfg(unix)]
        let artifacts = project.state.join("artifacts");
        #[cfg(unix)]
        let timestamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        #[cfg(unix)]
        std::fs::File::open(&artifacts)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(timestamp))
            .unwrap();
        let database = Connection::open_with_flags(
            project.state.join("ledger.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let version = || -> i64 {
            database
                .query_row("PRAGMA data_version", [], |row| row.get(0))
                .unwrap()
        };
        let before = version();
        assert_eq!(
            original,
            project.observe_snapshot(&BTreeSet::new(), None).unwrap()
        );
        assert_eq!(original, project.capture(&BTreeSet::new()).unwrap());
        assert_eq!(version(), before);
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&artifacts).unwrap().modified().unwrap(),
            timestamp
        );

        let path = project.root.join("implementations/quota/lib.rs");
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let previous = std::fs::read_to_string(&path).unwrap();
        let changed =
            previous.replace("requested >= self.remaining", "requested <= self.remaining");
        assert_eq!(changed.len(), previous.len());
        std::fs::write(&path, changed).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let observed = project.observe_snapshot(&BTreeSet::new(), None).unwrap();
        assert_ne!(original.digest().unwrap(), observed.digest().unwrap());
        let content = &observed.files["implementations/quota/lib.rs"].content;
        assert!(ledger.reference(&content.digest).is_err());
        assert_eq!(version(), before);
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&artifacts).unwrap().modified().unwrap(),
            timestamp
        );
        assert_eq!(observed, project.capture(&BTreeSet::new()).unwrap());
        ledger.verify_reference(content).unwrap();
    }

    #[test]
    fn observed_snapshots_preserve_the_controller_snapshot_identity() {
        let (_directory, project) = project();
        let ledger = project.ledger().unwrap();
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            fixtures::QUOTA_RUST,
        )
        .unwrap();
        std::fs::write(project.root.join("selected.py"), "print('explicit')\n").unwrap();
        let selection = ["selected.py".into()].into();
        let expected = project
            .integration()
            .unwrap()
            .snapshot_worktree(&ledger, &selection)
            .unwrap();
        assert_eq!(
            expected,
            project.observe_snapshot(&selection, None).unwrap()
        );
        assert_eq!(expected, project.capture(&selection).unwrap());
        std::fs::remove_file(project.root.join("implementations/quota/lib.rs")).unwrap();
        let expected = project
            .integration()
            .unwrap()
            .snapshot_worktree(&ledger, &selection)
            .unwrap();
        assert_eq!(
            expected,
            project.observe_snapshot(&selection, None).unwrap()
        );
        assert_eq!(expected, project.capture(&selection).unwrap());
    }

    #[test]
    fn files_removed_from_the_index_need_explicit_untracked_selection() {
        let (_directory, project) = project();
        let path = "implementations/quota/lib.rs";
        let repository = git2::Repository::open(&project.root).unwrap();
        let mut index = repository.index().unwrap();
        index.remove_path(Path::new(path)).unwrap();
        index.write().unwrap();
        assert!(project.root.join(path).exists());
        let excluded = project.capture(&BTreeSet::new()).unwrap();
        assert!(!excluded.files.contains_key(path));
        assert!(excluded.deletions.contains(&path.to_string()));
        let included = project.capture(&[path.into()].into()).unwrap();
        assert!(included.files.contains_key(path));
        assert!(!included.deletions.contains(&path.to_string()));
    }

    #[test]
    fn synchronized_untracked_files_need_selection_while_desktop_index_additions_are_included() {
        let (_directory, mut project) = project();
        let repository = git2::Repository::open(&project.root).unwrap();
        let base = repository.head().unwrap().peel_to_commit().unwrap();
        repository.branch("desktop-baseline", &base, false).unwrap();
        std::fs::write(project.root.join("staged.rs"), "pub fn staged() {}\n").unwrap();
        std::fs::write(project.root.join("untracked.rs"), "pub fn untracked() {}\n").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("staged.rs")).unwrap();
        index.add_path(Path::new("untracked.rs")).unwrap();
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Verification tests", "tests@rovik.invalid").unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "cloud shadow",
                &tree,
                &[&base],
            )
            .unwrap();
        project.baseline_reference = Some("refs/heads/desktop-baseline".into());
        project.tracked_paths =
            Some(["implementations/quota/lib.rs".into(), "staged.rs".into()].into());
        let snapshot = project.capture(&BTreeSet::new()).unwrap();
        assert_eq!(snapshot.base_revision, base.id().to_string());
        assert!(snapshot.files.contains_key("staged.rs"));
        assert!(!snapshot.files.contains_key("untracked.rs"));
        let inspection = project.inspect(BTreeSet::new()).unwrap();
        assert!(inspection["untracked"]
            .as_array()
            .unwrap()
            .contains(&json!("untracked.rs")));
        assert!(project
            .capture(&["untracked.rs".into()].into())
            .unwrap()
            .files
            .contains_key("untracked.rs"));
        std::fs::write(project.root.join("later-staged.rs"), "pub fn later() {}\n").unwrap();
        project
            .tracked_paths
            .as_mut()
            .unwrap()
            .insert("later-staged.rs".into());
        assert_ne!(
            snapshot.digest().unwrap(),
            project.capture(&BTreeSet::new()).unwrap().digest().unwrap()
        );
    }

    #[test]
    fn a_registered_project_reuses_its_catalog_across_distinct_execution_worktrees() {
        let (directory, original) = project();
        let first = IdeProject::open_for_project(
            &directory.path().join("shared"),
            &original.root,
            "registered-project",
        )
        .unwrap();
        let configuration = prepared(&first, Digest::of(b"image"), "/missing/docker");
        let repository = git2::Repository::open(&first.root).unwrap();
        let commit = repository.head().unwrap().peel_to_commit().unwrap();
        let branch = repository.branch("worker", &commit, false).unwrap();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(branch.get()));
        let second_root = directory.path().join("chat-worktree");
        repository
            .worktree("chat", &second_root, Some(&options))
            .unwrap();
        let second = IdeProject::open_for_project(
            &directory.path().join("shared"),
            &second_root,
            "registered-project",
        )
        .unwrap();
        assert_eq!(first.state, second.state);
        assert_eq!(first.repository_id, second.repository_id);
        assert_ne!(first.root, second.root);
        assert_eq!(
            second
                .load_setup(&configuration.setup_id)
                .unwrap()
                .repository_id,
            first.repository_id
        );
        std::fs::write(
            second.root.join("implementations/quota/lib.rs"),
            fixtures::QUOTA_RUST,
        )
        .unwrap();
        assert_ne!(
            first.capture(&BTreeSet::new()).unwrap().digest().unwrap(),
            second.capture(&BTreeSet::new()).unwrap().digest().unwrap()
        );
    }

    #[test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned native Rust toolchain image"]
    fn native_ide_verification_runs_without_model_calls_and_marks_later_edits_stale() {
        assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
        let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
        let executable =
            std::env::var("ROVIK_HARNESS_DOCKER_BIN").unwrap_or_else(|_| "/usr/bin/docker".into());
        let (_directory, project) = project();
        let config = prepared(&project, image, &executable);
        project
            .dispatch(Operation::Configure {
                setup_id: config.setup_id.to_string(),
                engine: config.engine,
                expected_snapshot_digest: config.snapshot_digest.to_string(),
            })
            .unwrap();
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            fixtures::QUOTA_RUST,
        )
        .unwrap();
        let index_before = std::fs::read(project.root.join(".git/index")).unwrap();
        let expected = project.capture(&BTreeSet::new()).unwrap().digest().unwrap();
        let status = project
            .dispatch(Operation::Verify {
                untracked: vec![],
                expected_snapshot_digest: expected.to_string(),
                saved_buffers: true,
            })
            .unwrap();
        assert_eq!(status["status"], "blocked");
        let run_id = status["runId"].as_str().unwrap().to_string();
        for approval in status["pendingApprovals"].as_array().unwrap() {
            assert!(!approval["subject"].is_null());
            project
                .dispatch(Operation::Approve {
                    run_id: run_id.clone(),
                    request: approval["request"].clone(),
                })
                .unwrap();
        }
        project
            .dispatch(Operation::Resume {
                run_id: run_id.clone(),
            })
            .unwrap();
        let started = std::time::Instant::now();
        while project.is_running(&run_id).unwrap() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(180),
                "Native verification timed out: run={:?}; operations={:#?}; private_state={}",
                project.ledger().unwrap().run(&run_id),
                project.ledger().unwrap().operations(&run_id),
                project.state.display(),
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let status = project.status(&run_id).unwrap();
        assert_eq!(status["status"], "verified", "{status:#}");
        assert_eq!(status["budgets"]["model_calls"]["spent"], 0);
        assert_eq!(
            std::fs::read(project.root.join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(
            std::fs::read_to_string(project.root.join("implementations/quota/lib.rs")).unwrap(),
            fixtures::QUOTA_RUST
        );
        std::fs::write(
            project.root.join("implementations/quota/lib.rs"),
            "changed after acceptance",
        )
        .unwrap();
        assert_eq!(project.status(&run_id).unwrap()["status"], "stale");
    }
}
