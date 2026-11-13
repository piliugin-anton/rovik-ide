//! Exact reviewed checkout application; accepted candidates never write through ordinary agent landing.

use super::*;
use crate::overlay::{
    fs::{DeletedBase, OverlayChange},
    land,
};
use rovik_harness::{
    controller::Integration, plan::ApprovalRequirement, review_bundle::SnapshotPatch,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Application {
    run_id: String,
    repository_id: String,
    root: PathBuf,
    baseline_reference: Option<String>,
    head: String,
    checkout_head: String,
    checkout_reference: String,
    index_digest: Digest,
    tracked_paths: BTreeSet<String>,
    candidate_digest: Digest,
    source_digest: Digest,
    configuration_digest: Digest,
    plan_digest: Option<Digest>,
    location: String,
    paths: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    subject_digest: Digest,
    candidate_digest: Digest,
    root: PathBuf,
    head: String,
    location: String,
    applied: bool,
    delivered_locally: bool,
    journal_id: Option<String>,
    paths: Vec<String>,
    applied_at_ms: u64,
    #[serde(default)]
    observed_snapshot_digest: Option<Digest>,
}

/// Observe private application state without synchronizing or changing a worktree.
/// An applied or interrupted host workspace must not accept another desktop merge.
pub fn application_guard(
    state_root: PathBuf,
    root: PathBuf,
    project_id: String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send>> {
    Box::pin(async move {
        crate::rt::spawn_blocking(move || {
            let database = state_root.join(Digest::of(project_id.as_bytes()).hex()).join("ide.sqlite3");
            if !database.exists() { return Ok(json!({"blocked":false,"runs":[]})); }
            let metadata = std::fs::symlink_metadata(&database)?;
            check(metadata.is_file() && !metadata.file_type().is_symlink(), "The private application index is not a regular file.")?;
            let database = Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let present: bool = database.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='applications')", [], |row|row.get(0))?;
            if !present { return Ok(json!({"blocked":false,"runs":[]})); }
            guard_database(&database, &root.canonicalize()?)
        })
        .await
        .map_err(|error| Error::new(error.to_string()))?
    })
}

impl IdeProject {
    pub(super) fn application_guard(&self) -> Result<Value> {
        guard_database(&self.database()?, &self.root)
    }

    fn application(&self, run_id: &str, digest: &Digest) -> Result<Application> {
        let value: String = self
            .database()?
            .query_row(
                "SELECT value FROM applications WHERE subject=?1 AND run_id=?2",
                params![digest.as_str(), run_id],
                |row| row.get(0),
            )
            .map_err(|_| {
                Error::new("Review the prepared application for this exact run first.".into())
            })?;
        let application: Application = wire::decode(value.as_bytes())?;
        check(
            Digest::of_json(&application)? == *digest
                && application.run_id == run_id
                && application.repository_id == self.repository_id
                && application.root == self.root
                && application.baseline_reference == self.baseline_reference,
            "Application belongs to another candidate or execution workspace.",
        )?;
        self.require_run_context(&self.run_input(run_id)?)?;
        Ok(application)
    }

    fn application_git(&self) -> Result<(String, String, Digest)> {
        let repository =
            git2::Repository::open(&self.root).map_err(|error| Error::new(error.to_string()))?;
        let head = repository
            .head()
            .map_err(|error| Error::new(error.to_string()))?;
        let revision = head
            .peel_to_commit()
            .map_err(|error| Error::new(error.to_string()))?
            .id()
            .to_string();
        let reference = head
            .name()
            .map_err(|error| {
                Error::new(format!("The destination HEAD has no valid name: {error}"))
            })?
            .to_string();
        let index = repository
            .index()
            .map_err(|error| Error::new(error.to_string()))?;
        check(
            !index.has_conflicts(),
            "Resolve Git index conflicts before applying changes.",
        )?;
        let index_path = index
            .path()
            .ok_or_else(|| Error::new("The destination has no Git index.".into()))?;
        Ok((revision, reference, Digest::of(&std::fs::read(index_path)?)))
    }

    fn application_context(&self, application: &Application, input: &RunInput) -> Result<()> {
        check(
            self.root.canonicalize()? == application.root,
            "The application destination changed identity.",
        )?;
        check(self.integration()?.current_revision(&self.repository_id)? == application.head,
            "The destination revision changed. Review and verify a new candidate before application.")?;
        let (head, reference, index) = self.application_git()?;
        check(head == application.checkout_head && reference == application.checkout_reference && index == application.index_digest,
            "The destination branch, HEAD or Git index changed after review. Prepare a new application.")?;
        check(
            self.tracked_inputs()?.0 == application.tracked_paths,
            "The selected tracked paths changed after application review.",
        )?;
        check(
            self.configuration()?
                .map(|value| Digest::of_json(&value))
                .transpose()?
                .as_ref()
                == Some(&application.configuration_digest),
            "The reviewed verification configuration changed.",
        )?;
        if let Some(plan) = &input.plan {
            check(
                Some(&plan.digest) == application.plan_digest.as_ref()
                    && self.read_plan(&plan.path)?.digest == plan.digest,
                "The reviewed implementation plan changed.",
            )?;
        }
        Ok(())
    }

    /// Prepare a concrete application decision without granting approval or writing project files.
    pub fn prepare_application(&self, run_id: &str) -> Result<Value> {
        if self.application_receipt(run_id)?.is_some() {
            return self.status(run_id);
        }
        let input = self.run_input(run_id)?;
        check(input.plan.is_some(), "This model-free verification already describes current checkout files; prepare an implementation candidate before applying changes.")?;
        check(
            self.status(run_id)?["status"] == "verified",
            "Only a current accepted candidate can prepare application.",
        )?;
        let controller = self.controller(None)?;
        let setup = controller.setup(run_id)?;
        setup
            .authority
            .authorize(&Principal::current()?, "delivery")?;
        let candidate = controller
            .current_candidate(run_id)?
            .ok_or_else(|| Error::new("This run has no candidate.".into()))?;
        let patch = SnapshotPatch::between(&setup.base, &candidate)?;
        let (checkout_head, checkout_reference, index_digest) = self.application_git()?;
        let application = Application {
            run_id: run_id.into(),
            repository_id: self.repository_id.clone(),
            root: self.root.clone(),
            baseline_reference: self.baseline_reference.clone(),
            head: self.integration()?.current_revision(&self.repository_id)?,
            checkout_head,
            checkout_reference,
            index_digest,
            tracked_paths: self.tracked_inputs()?.0,
            candidate_digest: candidate.digest()?,
            source_digest: input.snapshot_digest.clone(),
            configuration_digest: input.configuration_digest.clone(),
            plan_digest: input.plan.as_ref().map(|plan| plan.digest.clone()),
            location: if self.baseline_reference.is_some() {
                "host"
            } else {
                "local"
            }
            .into(),
            paths: patch.changes.keys().cloned().collect(),
        };
        self.application_context(&application, &input)?;
        self.current_capture(&application.source_digest, &input.untracked)?;
        exact_preimages(
            &self.root,
            &controller.ledger,
            &setup.base,
            &candidate,
            false,
        )?;
        let artifact = controller
            .ledger
            .store_json("delivery", &application, runner::now_ms()?)?;
        let created_ms = i64::try_from(runner::now_ms()?)
            .map_err(|_| Error::new("Application timestamp overflow.".into()))?;
        self.database()?.execute("UPDATE applications SET state='superseded' WHERE run_id=?1 AND subject!=?2 AND state='prepared'",params![run_id,artifact.digest.as_str()])?;
        self.database()?.execute("INSERT OR IGNORE INTO applications(subject,run_id,value,state,created_ms) VALUES(?1,?2,?3,'prepared',?4)",
            params![artifact.digest.as_str(),run_id,serde_json::to_string(&application).map_err(|error| Error::new(error.to_string()))?,created_ms])?;
        self.status(run_id)
    }

    pub(super) fn application_requirements(
        &self,
        controller: &Controller,
        run_id: &str,
    ) -> Result<Vec<ApprovalRequirement>> {
        let setup = controller.setup(run_id)?;
        let database = self.database()?;
        let mut statement = database.prepare("SELECT subject FROM applications WHERE run_id=?1 AND state IN ('prepared','applying') ORDER BY created_ms")?;
        let mut pending = Vec::new();
        for subject in statement.query_map([run_id], |row| row.get::<_, String>(0))? {
            let requirement = ApprovalRequirement {
                subject_digest: Digest::parse(&subject?)?,
                approval_type: "delivery".into(),
                obligation_ids: vec![],
            };
            self.application(run_id, &requirement.subject_digest)?;
            if !controller.ledger.approval_current(
                run_id,
                &requirement,
                &Digest::of_json(&setup.contract.scope)?,
                &Digest::of_json(&setup.policy)?,
                &setup.authority,
                runner::now_ms()?,
            )? {
                pending.push(requirement);
            }
        }
        Ok(pending)
    }

    /// Apply or explicitly recover the exact approved candidate, preserving index and branch state.
    pub fn apply_application(
        &self,
        run_id: &str,
        subject: &Digest,
        saved_buffers: bool,
        recover: bool,
    ) -> Result<Value> {
        check(
            saved_buffers,
            "Save all editor buffers before applying or recovering reviewed changes.",
        )?;
        let _application_lock = crate::os_lock::try_hold(&self.state.join("application.lock"))?
            .ok_or_else(|| {
                Error::new("Another application is in progress; retry after it finishes.".into())
            })?;
        let application = self.application(run_id, subject)?;
        let input = self.run_input(run_id)?;
        let controller = self.controller(None)?;
        let setup = controller.setup(run_id)?;
        let candidate = controller
            .current_candidate(run_id)?
            .ok_or_else(|| Error::new("This run has no accepted candidate.".into()))?;
        check(
            candidate.digest()? == application.candidate_digest,
            "The reviewed application names a different candidate.",
        )?;
        self.application_context(&application, &input)?;
        if let Some(receipt) = self.application_receipt(run_id)? {
            check(
                receipt.subject_digest == *subject && self.application_current(&receipt)?,
                "The applied destination changed. Review a new verified candidate.",
            )?;
            return self.status(run_id);
        }
        let state: String = self.database()?.query_row(
            "SELECT state FROM applications WHERE subject=?1",
            [subject.as_str()],
            |row| row.get(0),
        )?;
        check(
            matches!(state.as_str(), "prepared" | "applying"),
            "This application review was superseded. Review the current prepared decision.",
        )?;
        check(
            state != "applying" || recover,
            "Application was interrupted. Review its journal and choose Recover application.",
        )?;
        if state == "prepared" {
            self.current_capture(&application.source_digest, &input.untracked)?;
        }
        let source: Snapshot = wire::decode(&controller.ledger.read(&input.snapshot_digest)?)?;
        let private = self.state.join("applications").join(subject.hex());
        std::fs::create_dir_all(&private)?;
        let changes = materialize(&private, &controller.ledger, &setup.base, &candidate)?;
        let journal_key = "application";
        let integration = self.integration()?;
        let journal_id = controller.deliver_authorized(&Principal::current()?, run_id, subject, &integration, || {
            self.application_context(&application, &input)?;
            self.database()?.execute("UPDATE applications SET state='applying' WHERE subject=?1", [subject.as_str()])?;
            if recover && state == "applying" {
                land::recover_guarded(&private, journal_key, &self.root, &application.paths, || {
                    self.application_context(&application, &input).and_then(|_| application_preimages(&self.root, &controller.ledger, &application, &input, &source, &candidate, true)).map_err(|error| error.message)
                }).map_err(|error| Error::new(error.to_string()))?;
                if application_present(&self.root, &controller.ledger, &application, &input, &source, &candidate)? {
                    return Ok(land::latest(&private, journal_key).map(|(id,_,_)|id));
                }
            }
            let landed = land::land_guarded(&private, journal_key, &self.root, &changes, false, None, false, || {
                self.application_context(&application, &input)
                    .and_then(|_| application_preimages(&self.root, &controller.ledger, &application, &input, &source, &candidate, false))
                    .map_err(|error| error.message)
            }).map_err(|error| Error::new(error.to_string()))?;
            check(application_present(&self.root, &controller.ledger, &application, &input, &source, &candidate)?, "The destination changed during application; no success receipt was recorded. Recover the reviewed journal.")?;
            self.application_context(&application, &input)?;
            Ok(landed.map(|landed| landed.id))
        })?;
        // Observe all authenticated candidate paths, including added files, and
        // retain baseline-relative deletion tombstones separately from candidate
        // identity. Removed untracked inputs are no longer required to exist.
        let observation_selection = candidate.files.keys().cloned().collect();
        let observed_snapshot_digest = Some(self.capture(&observation_selection)?.digest()?);
        self.application_context(&application, &input)?;
        check(application_present(&self.root, &controller.ledger, &application, &input, &source, &candidate)?,
            "The destination changed while recording application. Recover the reviewed journal before recording success.")?;
        let receipt = Receipt {
            subject_digest: subject.clone(),
            candidate_digest: application.candidate_digest,
            root: self.root.clone(),
            head: application.head,
            location: application.location.clone(),
            applied: true,
            delivered_locally: application.location == "local",
            journal_id,
            paths: application.paths,
            applied_at_ms: runner::now_ms()?,
            observed_snapshot_digest,
        };
        let artifact =
            controller
                .ledger
                .store_json("delivery-receipt", &receipt, runner::now_ms()?)?;
        self.database()?.execute(
            "UPDATE applications SET state='applied',receipt=?2 WHERE subject=?1",
            params![subject.as_str(), artifact.digest.as_str()],
        )?;
        let paths: Vec<_> = receipt
            .paths
            .iter()
            .map(|path| self.root.join(path))
            .collect();
        crate::overlay::chats::project_changed(&self.root, &paths, false);
        self.status(run_id)
    }

    /// Release only the synchronization hold, after explicit preservation of host output.
    /// This records no desktop delivery and never modifies the original application receipt.
    pub fn release_application(
        &self,
        run_id: &str,
        subject: &Digest,
        acknowledged: bool,
    ) -> Result<Value> {
        check(acknowledged, "Confirm that the host changes were preserved or manually integrated before allowing synchronization.")?;
        let _lock =
            crate::os_lock::try_hold(&self.state.join("application.lock"))?.ok_or_else(|| {
                Error::new(
                    "An application is in progress; reconcile it before releasing synchronization."
                        .into(),
                )
            })?;
        let application = self.application(run_id, subject)?;
        check(
            application.location == "host",
            "Only host applications need a synchronization release.",
        )?;
        let receipt = self.application_receipt(run_id)?.ok_or_else(|| {
            Error::new(
                "Interrupted application must be recovered before releasing synchronization."
                    .into(),
            )
        })?;
        check(
            receipt.subject_digest == *subject,
            "A different application was acknowledged.",
        )?;
        let controller = self.controller(None)?;
        controller
            .setup(run_id)?
            .authority
            .authorize(&Principal::current()?, "operate")?;
        controller.ledger.store_json("delivery-receipt", &json!({"kind":"host-synchronization-release","subjectDigest":subject,"runId":run_id,"root":self.root,"releasedAtMs":runner::now_ms()?,"deliveredLocally":false}),runner::now_ms()?)?;
        self.database()?.execute("UPDATE applications SET state='released' WHERE subject=?1 AND state IN ('applied','released')",[subject.as_str()])?;
        self.status(run_id)
    }

    fn application_receipt(&self, run_id: &str) -> Result<Option<Receipt>> {
        let value: Option<String> = self.database()?.query_row("SELECT receipt FROM applications WHERE run_id=?1 AND state IN ('applied','released') ORDER BY created_ms DESC,rowid DESC LIMIT 1", [run_id], |row|row.get(0)).optional()?.flatten();
        value
            .map(|value| wire::decode(&self.ledger()?.read(&Digest::parse(&value)?)?))
            .transpose()
    }

    fn application_current(&self, receipt: &Receipt) -> Result<bool> {
        let application = self.application(
            &self.run_id_for_subject(&receipt.subject_digest)?,
            &receipt.subject_digest,
        )?;
        let input = self.run_input(&application.run_id)?;
        if self.application_context(&application, &input).is_err() {
            return Ok(false);
        }
        let controller = self.controller(None)?;
        let Some(candidate) = controller.current_candidate(&application.run_id)? else {
            return Ok(false);
        };
        let source: Snapshot = wire::decode(&controller.ledger.read(&input.snapshot_digest)?)?;
        Ok(candidate.digest()? == receipt.candidate_digest
            && application_present(
                &self.root,
                &controller.ledger,
                &application,
                &input,
                &source,
                &candidate,
            )?)
    }

    /// Return the receipt, candidate and saved observer identities only while
    /// the complete application and its exact approvals remain current.
    /// Editor feedback must also match its newly observed source digest.
    pub fn current_application_binding(
        &self,
        run_id: &str,
    ) -> Result<Option<(Digest, Digest, Digest)>> {
        self.require_run_context(&self.run_input(run_id)?)?;
        let Some(receipt) = self.application_receipt(run_id)? else {
            return Ok(None);
        };
        let Some(observed) = receipt.observed_snapshot_digest.clone() else {
            return Ok(None);
        };
        if !self.application_current(&receipt)? {
            return Ok(None);
        }
        let controller = self.controller(None)?;
        if controller.ledger.run(run_id)?.state != State::Accepted
            || controller.verify(run_id, &self.integration()?)?.verdict != Verdict::Ready
        {
            return Ok(None);
        }
        let setup = controller.setup(run_id)?;
        let requirement = ApprovalRequirement {
            subject_digest: receipt.subject_digest.clone(),
            approval_type: "delivery".into(),
            obligation_ids: vec![],
        };
        if !controller.ledger.approval_current(
            run_id,
            &requirement,
            &Digest::of_json(&setup.contract.scope)?,
            &Digest::of_json(&setup.policy)?,
            &setup.authority,
            runner::now_ms()?,
        )? {
            return Ok(None);
        }
        let digest: String = self.database()?.query_row(
            "SELECT receipt FROM applications WHERE run_id=?1 AND subject=?2 AND state IN ('applied','released')",
            params![run_id, receipt.subject_digest.as_str()],
            |row| row.get(0),
        )?;
        Ok(Some((
            Digest::parse(&digest)?,
            receipt.candidate_digest,
            observed,
        )))
    }

    fn run_id_for_subject(&self, subject: &Digest) -> Result<String> {
        Ok(self.database()?.query_row(
            "SELECT run_id FROM applications WHERE subject=?1",
            [subject.as_str()],
            |row| row.get(0),
        )?)
    }

    pub(super) fn application_fields(&self, run_id: &str) -> Result<(Value, Value, bool)> {
        let prepared: Option<String> = self
            .database()?
            .query_row(
                "SELECT subject FROM applications WHERE run_id=?1 ORDER BY created_ms DESC,rowid DESC LIMIT 1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let prepared = prepared
            .map(|digest| {
                let digest = Digest::parse(&digest)?;
                Ok::<_, Error>(
                    json!({"subjectDigest":digest,"subject":self.application(run_id, &digest)?}),
                )
            })
            .transpose()?
            .unwrap_or(Value::Null);
        let receipt = self.application_receipt(run_id)?;
        let current = receipt
            .as_ref()
            .map(|receipt| self.application_current(receipt))
            .transpose()?
            .unwrap_or(false);
        let mut value = receipt
            .map(|receipt| encode(&receipt))
            .transpose()?
            .unwrap_or(Value::Null);
        if !value.is_null() {
            value["current"] = json!(current);
            let released: bool = self.database()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM applications WHERE run_id=?1 AND state='released')",
                [run_id],
                |row| row.get(0),
            )?;
            value["synchronizationReleased"] = json!(released);
        }
        Ok((prepared, value, current))
    }
}

fn guard_database(database: &Connection, root: &Path) -> Result<Value> {
    let mut statement = database.prepare("SELECT run_id,value FROM applications WHERE state IN ('applying','applied') ORDER BY created_ms")?;
    let mut runs = Vec::new();
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, value) = row?;
        let application: Application = wire::decode(value.as_bytes())?;
        if application.root == root {
            runs.push(id);
        }
    }
    Ok(json!({"blocked":!runs.is_empty(),"runs":runs}))
}

fn file_matches(
    root: &Path,
    ledger: &Ledger,
    path: &str,
    entry: Option<&rovik_harness::snapshot::FileEntry>,
) -> Result<bool> {
    let mut parent = root.to_path_buf();
    let components: Vec<_> = Path::new(path).components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        parent.push(component.as_os_str());
        if std::fs::symlink_metadata(&parent)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Ok(false);
        }
    }
    let path = root.join(path);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entry.is_none()),
        Err(error) => return Err(error.into()),
    };
    let Some(entry) = entry else {
        return Ok(false);
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.nlink() != 1 {
            return Ok(false);
        }
        if (metadata.permissions().mode() & 0o111 != 0) != (entry.mode == 0o100755) {
            return Ok(false);
        }
    }
    Ok(std::fs::read(path)? == ledger.read(&entry.content.digest)?)
}

fn exact_preimages(
    root: &Path,
    ledger: &Ledger,
    base: &Snapshot,
    candidate: &Snapshot,
    partial: bool,
) -> Result<()> {
    let paths: BTreeSet<_> = base.files.keys().chain(candidate.files.keys()).collect();
    for path in paths {
        let before = base.files.get(path);
        let after = candidate.files.get(path);
        check(file_matches(root,ledger,path,before)? || (partial && before != after && file_matches(root,ledger,path,after)?),
            &format!("The destination changed at {path}; application cannot overwrite current user changes."))?;
    }
    Ok(())
}

fn candidate_present(
    root: &Path,
    ledger: &Ledger,
    base: &Snapshot,
    candidate: &Snapshot,
) -> Result<bool> {
    for path in base
        .files
        .keys()
        .chain(candidate.files.keys())
        .collect::<BTreeSet<_>>()
    {
        if !file_matches(root, ledger, path, candidate.files.get(path))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn application_preimages(
    root: &Path,
    ledger: &Ledger,
    application: &Application,
    input: &RunInput,
    source: &Snapshot,
    candidate: &Snapshot,
    partial: bool,
) -> Result<()> {
    exact_preimages(root, ledger, source, candidate, partial)?;
    for path in application
        .tracked_paths
        .iter()
        .chain(input.untracked.iter())
    {
        if !source.files.contains_key(path) && !candidate.files.contains_key(path) {
            check(
                file_matches(root, ledger, path, None)?,
                &format!(
                    "A selected deleted path reappeared: {path}. Reverify before application."
                ),
            )?;
        }
    }
    Ok(())
}

fn application_present(
    root: &Path,
    ledger: &Ledger,
    application: &Application,
    input: &RunInput,
    source: &Snapshot,
    candidate: &Snapshot,
) -> Result<bool> {
    if !candidate_present(root, ledger, source, candidate)? {
        return Ok(false);
    }
    for path in application
        .tracked_paths
        .iter()
        .chain(input.untracked.iter())
    {
        if !candidate.files.contains_key(path) && !file_matches(root, ledger, path, None)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn materialize(
    private: &Path,
    ledger: &Ledger,
    base: &Snapshot,
    candidate: &Snapshot,
) -> Result<Vec<OverlayChange>> {
    let patch = SnapshotPatch::between(base, candidate)?;
    let mut changes = Vec::new();
    for (index, (path, change)) in patch.changes.iter().enumerate() {
        rovik_harness::scope::validate_path(path)?;
        check(
            !path
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git")),
            "Application cannot modify Git control paths.",
        )?;
        let before = change
            .before
            .as_ref()
            .map(|entry| ledger.read(&entry.content.digest))
            .transpose()?;
        if let Some(after) = &change.after {
            let upper = private.join(format!("after-{index}"));
            crate::atomic_file::write_atomic_private(&upper, &ledger.read(&after.content.digest)?)?;
            let base_copy = before
                .as_ref()
                .map(|bytes| {
                    let file = private.join(format!("before-{index}"));
                    crate::atomic_file::write_atomic_private(&file, bytes)?;
                    Ok::<_, Error>(file)
                })
                .transpose()?;
            changes.push(OverlayChange::Write {
                path: path.clone(),
                kind: crate::vfs::store::Kind::File,
                origin: before.as_ref().map(|_| path.clone()),
                upper: Some(upper),
                base_copy,
                perm: if after.mode == 0o100755 { 0o755 } else { 0o644 },
                perm_changed: change
                    .before
                    .as_ref()
                    .is_none_or(|before| before.mode != after.mode),
                target: None,
                by_product: false,
            });
        } else if let Some(before) = before {
            changes.push(OverlayChange::Delete {
                path: path.clone(),
                by_product: false,
                base: Some(DeletedBase::file(&before)),
            });
        }
    }
    check(
        changes.iter().all(|change| !change.by_product()),
        "The accepted patch contains paths excluded by IDE landing; application is blocked.",
    )?;
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_harness::fixtures;

    fn trees() -> (tempfile::TempDir, PathBuf, Ledger, Snapshot, Snapshot) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let ledger = Ledger::open(&directory.path().join("ledger")).unwrap();
        let base = fixtures::snapshot(
            &ledger,
            "source-head",
            &[
                ("source.txt", "user's dirty starting bytes\n"),
                ("kept.txt", "unrelated source\n"),
            ],
        )
        .unwrap();
        let candidate = fixtures::snapshot(
            &ledger,
            "source-head",
            &[
                ("source.txt", "reviewed candidate\n"),
                ("kept.txt", "unrelated source\n"),
                ("new/nested.txt", "new candidate file\n"),
            ],
        )
        .unwrap();
        for (path, entry) in &base.files {
            std::fs::write(root.join(path), ledger.read(&entry.content.digest).unwrap()).unwrap();
        }
        std::fs::write(root.join("unselected.txt"), "user's unselected bytes").unwrap();
        (directory, root, ledger, base, candidate)
    }

    #[test]
    fn exact_patch_application_preserves_unrelated_dirty_inputs_and_never_touches_git_metadata() {
        let (directory, root, ledger, base, candidate) = trees();
        let repository = git2::Repository::init(&root).unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("source.txt")).unwrap();
        index.write().unwrap();
        let before = std::fs::read(repository.path().join("index")).unwrap();
        let private = directory.path().join("application");
        std::fs::create_dir_all(&private).unwrap();
        let changes = materialize(&private, &ledger, &base, &candidate).unwrap();
        let landed = land::land_guarded(
            &private,
            "application",
            &root,
            &changes,
            false,
            None,
            false,
            || {
                exact_preimages(&root, &ledger, &base, &candidate, false)
                    .map_err(|error| error.message)
            },
        )
        .unwrap()
        .unwrap();
        assert!(candidate_present(&root, &ledger, &base, &candidate).unwrap());
        assert_eq!(
            std::fs::read(repository.path().join("index")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read_to_string(root.join("unselected.txt")).unwrap(),
            "user's unselected bytes"
        );
        assert_eq!(landed.paths.len(), 2);
        assert!(land::latest(&private, "application").is_some());
    }

    #[test]
    fn index_removed_unselected_file_is_preserved_and_blocks_candidate_readdition() {
        let (directory, project) = super::super::tests::project();
        let excluded = "implementations/quota/local-notes.txt";
        let user_bytes = b"private edits retained after git rm --cached\n";
        std::fs::write(project.root.join(excluded), "previously committed notes\n").unwrap();
        let repository = git2::Repository::open(&project.root).unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new(excluded)).unwrap();
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repository.head().unwrap().peel_to_commit().unwrap();
        let signature = git2::Signature::now("Application tests", "tests@rovik.invalid").unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "tracked notes",
                &tree,
                &[&parent],
            )
            .unwrap();
        std::fs::write(project.root.join(excluded), user_bytes).unwrap();
        index.remove_path(Path::new(excluded)).unwrap();
        index.write().unwrap();
        let original_index = std::fs::read(repository.path().join("index")).unwrap();

        let ledger = project.ledger().unwrap();
        let baseline = project.integration().unwrap().snapshot(&ledger).unwrap();
        let mut prepared = fixtures::quota_rust(
            &ledger,
            "index-removal",
            Digest::of(b"unused-image"),
            &baseline.base_revision,
        )
        .unwrap();
        prepared.repository_id = project.repository_id.clone();
        // Implementation intake reserves the complete context and output limits.
        prepared.budgets.insert("tokens".into(), 256_000);
        prepared.base = baseline.clone();
        prepared.negative_control = baseline;
        for citation in &mut prepared.design.existing_code_considered {
            citation.source_revision = prepared.base.digest().unwrap();
        }
        prepared.plan.design_digest = Digest::of_json(&prepared.design).unwrap();
        let registered = project.register_setup(prepared).unwrap();
        let source = project.capture(&BTreeSet::new()).unwrap();
        assert!(!source.files.contains_key(excluded));
        assert!(source.deletions.contains(&excluded.to_string()));
        let configuration = Configuration {
            setup_id: Digest::parse(registered["id"].as_str().unwrap()).unwrap(),
            engine: Engine {
                kind: EngineKind::Docker,
                executable: "/unused/docker".into(),
            },
            snapshot_digest: source.digest().unwrap(),
        };
        std::fs::write(project.root.join("plan.md"), "Repair quota").unwrap();
        let provider = decode(json!({"profileId":"fixture","model":"fixture-scripted","endpoint":"http://localhost/v1/chat/completions","rates":{"input":0,"cachedInput":0,"output":0}})).unwrap();
        let setup = project
            .implementation_setup(
                &configuration,
                source,
                &project.read_plan("plan.md").unwrap(),
                &provider,
            )
            .unwrap();
        assert!(!setup.base.files.contains_key(excluded));
        assert!(setup.base.deletions.is_empty());
        let mut candidate = setup.base.clone();
        candidate
            .files
            .get_mut("implementations/quota/lib.rs")
            .unwrap()
            .content = ledger
            .store(
                "source",
                "application/octet-stream",
                fixtures::QUOTA_RUST.as_bytes(),
                runner::now_ms().unwrap(),
            )
            .unwrap();
        let patch = SnapshotPatch::between(&setup.base, &candidate).unwrap();
        assert!(!patch.changes.contains_key(excluded));

        // Even identical existing bytes do not authorize adopting an excluded
        // file as a candidate addition. No other candidate change may land.
        let mut collision = candidate.clone();
        collision.files.insert(
            excluded.into(),
            rovik_harness::snapshot::FileEntry {
                mode: 0o100644,
                content: ledger
                    .store(
                        "source",
                        "application/octet-stream",
                        user_bytes,
                        runner::now_ms().unwrap(),
                    )
                    .unwrap(),
            },
        );
        let error =
            exact_preimages(&project.root, &ledger, &setup.base, &collision, false).unwrap_err();
        assert!(error.message.contains(excluded));
        let private = directory.path().join("application");
        std::fs::create_dir_all(&private).unwrap();
        let changes = materialize(&private, &ledger, &setup.base, &collision).unwrap();
        assert!(land::land_guarded(
            &private,
            "application",
            &project.root,
            &changes,
            false,
            None,
            false,
            || exact_preimages(&project.root, &ledger, &setup.base, &collision, false)
                .map_err(|error| error.message)
        )
        .is_err());
        assert!(land::latest(&private, "application").is_none());
        assert_eq!(
            std::fs::read(project.root.join("implementations/quota/lib.rs")).unwrap(),
            ledger
                .read(
                    &setup.base.files["implementations/quota/lib.rs"]
                        .content
                        .digest
                )
                .unwrap()
        );
        let mut invalid_deletion = candidate.clone();
        invalid_deletion.deletions.push(excluded.into());
        assert!(materialize(&private, &ledger, &setup.base, &invalid_deletion).is_err());

        let changes = materialize(&private, &ledger, &setup.base, &candidate).unwrap();
        let landed = land::land_guarded(
            &private,
            "application",
            &project.root,
            &changes,
            false,
            None,
            false,
            || {
                exact_preimages(&project.root, &ledger, &setup.base, &candidate, false)
                    .map_err(|error| error.message)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            landed.paths,
            vec!["implementations/quota/lib.rs".to_string()]
        );
        assert!(candidate_present(&project.root, &ledger, &setup.base, &candidate).unwrap());
        assert_eq!(
            std::fs::read(project.root.join(excluded)).unwrap(),
            user_bytes
        );
        assert_eq!(
            std::fs::read(repository.path().join("index")).unwrap(),
            original_index
        );
        assert!(repository
            .index()
            .unwrap()
            .get_path(Path::new(excluded), 0)
            .is_none());
    }

    #[test]
    fn changed_source_and_failed_final_authorization_prevent_every_application_write() {
        let (directory, root, ledger, base, candidate) = trees();
        let private = directory.path().join("application");
        std::fs::create_dir_all(&private).unwrap();
        let changes = materialize(&private, &ledger, &base, &candidate).unwrap();
        let error = land::land_guarded(
            &private,
            "application",
            &root,
            &changes,
            false,
            None,
            false,
            || Err("approval revoked".into()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("approval revoked"));
        assert!(!root.join("new/nested.txt").exists());
        assert!(land::latest(&private, "application").is_none());
        std::fs::write(
            root.join("kept.txt"),
            "changed outside the patch after review",
        )
        .unwrap();
        assert!(land::land_guarded(
            &private,
            "application",
            &root,
            &changes,
            false,
            None,
            false,
            || exact_preimages(&root, &ledger, &base, &candidate, false)
                .map_err(|error| error.message)
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("source.txt")).unwrap(),
            "user's dirty starting bytes\n"
        );
        assert!(!root.join("new/nested.txt").exists());
    }

    fn interrupted(
        private: &Path,
        root: &Path,
        ledger: &Ledger,
        base: &Snapshot,
        candidate: &Snapshot,
    ) {
        let directory = private.join("landings/application/interrupted");
        std::fs::create_dir_all(directory.join("old")).unwrap();
        std::fs::create_dir_all(directory.join("new")).unwrap();
        std::fs::write(
            private.join("landings/application/project.txt"),
            root.to_str().unwrap(),
        )
        .unwrap();
        let old = directory.join("old/source");
        let new = directory.join("new/source");
        let added = directory.join("new/added");
        std::fs::write(
            &old,
            ledger
                .read(&base.files["source.txt"].content.digest)
                .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &new,
            ledger
                .read(&candidate.files["source.txt"].content.digest)
                .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &added,
            ledger
                .read(&candidate.files["new/nested.txt"].content.digest)
                .unwrap(),
        )
        .unwrap();
        let journal = json!({"version":1,"state":"applying","at":1,"entries":[
            {"path":"source.txt","old":{"kind":"file","file":old,"perm":420},"new":{"kind":"file","file":new,"perm":420}},
            {"path":"new/nested.txt","old":{"kind":"absent"},"new":{"kind":"file","file":added,"perm":420}}
        ]});
        std::fs::write(
            directory.join("journal.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("source.txt"),
            ledger
                .read(&candidate.files["source.txt"].content.digest)
                .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn interrupted_application_recovers_exact_bytes_and_refuses_later_user_changes() {
        let (directory, root, ledger, base, candidate) = trees();
        let private = directory.path().join("application");
        interrupted(&private, &root, &ledger, &base, &candidate);
        let paths = vec!["source.txt".into(), "new/nested.txt".into()];
        std::fs::write(root.join("kept.txt"), "new user's work").unwrap();
        assert!(
            land::recover_guarded(&private, "application", &root, &paths, || exact_preimages(
                &root, &ledger, &base, &candidate, true
            )
            .map_err(|error| error.message))
            .is_err()
        );
        assert!(!root.join("new/nested.txt").exists());
        std::fs::write(root.join("kept.txt"), "unrelated source\n").unwrap();
        land::recover_guarded(&private, "application", &root, &paths, || {
            exact_preimages(&root, &ledger, &base, &candidate, true).map_err(|error| error.message)
        })
        .unwrap();
        assert!(candidate_present(&root, &ledger, &base, &candidate).unwrap());
        assert_eq!(
            land::latest(&private, "application").unwrap().0,
            "interrupted"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("unselected.txt")).unwrap(),
            "user's unselected bytes"
        );
    }

    #[test]
    fn application_guard_is_scoped_to_the_bound_workspace_and_release_is_distinct_from_delivery() {
        let (_directory, project) = super::super::tests::project();
        let application = Application {
            run_id: "run".into(),
            repository_id: project.repository_id.clone(),
            root: project.root.clone(),
            baseline_reference: Some("refs/heads/source".into()),
            head: "head".into(),
            checkout_head: "shadow".into(),
            checkout_reference: "refs/heads/chat".into(),
            index_digest: Digest::of(b"index"),
            tracked_paths: BTreeSet::new(),
            candidate_digest: Digest::of(b"candidate"),
            source_digest: Digest::of(b"source"),
            configuration_digest: Digest::of(b"config"),
            plan_digest: None,
            location: "host".into(),
            paths: vec![],
        };
        let subject = Digest::of_json(&application).unwrap();
        project.database().unwrap().execute("INSERT INTO applications(subject,run_id,value,state,created_ms) VALUES(?1,'run',?2,'applying',1)",params![subject.as_str(),serde_json::to_string(&application).unwrap()]).unwrap();
        assert_eq!(project.application_guard().unwrap()["blocked"], true);
        let other = project.root.join("other");
        assert_eq!(
            guard_database(&project.database().unwrap(), &other).unwrap()["blocked"],
            false
        );
        project
            .database()
            .unwrap()
            .execute("UPDATE applications SET state='released'", [])
            .unwrap();
        assert_eq!(project.application_guard().unwrap()["blocked"], false);
        assert_eq!(application.location, "host");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned native Rust image"]
    fn native_accepted_candidate_requires_delivery_approval_and_applies_with_a_durable_receipt() {
        assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
        let image = Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap();
        let executable =
            std::env::var("ROVIK_HARNESS_DOCKER_BIN").unwrap_or_else(|_| "/usr/bin/docker".into());
        let (_directory, project) = super::super::tests::project();
        let mut configuration = super::super::tests::prepared(&project, image, &executable);
        let mut prepared = project.load_setup(&configuration.setup_id).unwrap();
        prepared.budgets.insert("tokens".into(), 256_000);
        let registered = project.register_setup(prepared).unwrap();
        configuration.setup_id = Digest::parse(registered["id"].as_str().unwrap()).unwrap();
        project
            .database()
            .unwrap()
            .execute(
                "INSERT INTO configuration(singleton,value) VALUES(1,?1)",
                [serde_json::to_string(&configuration).unwrap()],
            )
            .unwrap();
        let removed = "implementations/quota/obsolete.txt";
        let added = "implementations/quota/new.txt";
        std::fs::write(project.root.join(removed), "selected untracked source").unwrap();
        let untracked = BTreeSet::from([removed.to_string()]);
        let original = project.capture(&untracked).unwrap();
        std::fs::write(project.root.join("plan.md"), "Repair quota").unwrap();
        let plan:plan::PlanBinding=decode(json!({"path":"plan.md","digest":Digest::of(b"Repair quota"),"provider":{"profileId":"fixture","model":"fixture-scripted","endpoint":"http://localhost/v1/chat/completions","rates":{"input":0,"cachedInput":0,"output":0}}})).unwrap();
        let mut setup = project
            .implementation_setup(
                &configuration,
                original.clone(),
                &project.read_plan("plan.md").unwrap(),
                &plan.provider,
            )
            .unwrap();
        let mut candidate = original.clone();
        candidate
            .files
            .get_mut("implementations/quota/lib.rs")
            .unwrap()
            .content = project
            .ledger()
            .unwrap()
            .store(
                "source",
                "application/octet-stream",
                fixtures::QUOTA_RUST.as_bytes(),
                runner::now_ms().unwrap(),
            )
            .unwrap();
        candidate.files.remove(removed);
        candidate.deletions.push(removed.into());
        candidate.files.insert(
            added.into(),
            rovik_harness::snapshot::FileEntry {
                mode: 0o100644,
                content: project
                    .ledger()
                    .unwrap()
                    .store(
                        "source",
                        "application/octet-stream",
                        b"reviewed added file\n",
                        runner::now_ms().unwrap(),
                    )
                    .unwrap(),
            },
        );
        setup.initial_candidate = Some(candidate);
        setup
            .authority
            .actors
            .get_mut(Principal::current().unwrap().actor_id())
            .unwrap()
            .insert("delivery".into());
        let mut controller = project.controller(Some(&configuration.engine)).unwrap();
        controller
            .create(&setup, runner::now_ms().unwrap())
            .unwrap();
        let input = RunInput {
            plan: Some(plan),
            execution_root: project.root.clone(),
            baseline_reference: None,
            tracked_paths: None,
            configuration_digest: Digest::of_json(&configuration).unwrap(),
            configuration,
            snapshot_digest: original.digest().unwrap(),
            untracked,
        };
        project
            .database()
            .unwrap()
            .execute(
                "INSERT INTO runs(id,input,created_ms) VALUES(?1,?2,1)",
                params![setup.run_id, serde_json::to_string(&input).unwrap()],
            )
            .unwrap();
        for requirement in controller.pending_approvals(&setup.run_id).unwrap() {
            controller
                .ledger
                .grant(
                    &Principal::current().unwrap(),
                    &setup.authority,
                    ApprovalRequest {
                        run_id: Some(setup.run_id.clone()),
                        subject_digest: requirement.subject_digest,
                        approval_type: requirement.approval_type,
                        policy_digest: Digest::of_json(&setup.policy).unwrap(),
                        scope_digest: Digest::of_json(&setup.contract.scope).unwrap(),
                        expires_ms: None,
                    },
                    runner::now_ms().unwrap(),
                )
                .unwrap();
        }
        let result = controller
            .execute(
                &setup.run_id,
                &mut OfflineProvider,
                &project.integration().unwrap(),
                &Cancellation::default(),
            )
            .unwrap();
        assert_eq!(result.state, State::Accepted);
        let prepared = project.prepare_application(&setup.run_id).unwrap();
        let subject =
            Digest::parse(prepared["application"]["subjectDigest"].as_str().unwrap()).unwrap();
        assert!(project
            .apply_application(&setup.run_id, &subject, true, false)
            .is_err());
        let decision = prepared["pendingApprovals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pending| pending["approvalType"] == "delivery")
            .unwrap();
        project
            .dispatch(Operation::Approve {
                run_id: setup.run_id.clone(),
                request: decision["request"].clone(),
            })
            .unwrap();
        let approval = controller
            .ledger
            .approval_history(&setup.run_id)
            .unwrap()
            .into_iter()
            .find(|row| row["approval"]["approval_type"] == "delivery")
            .unwrap();
        controller
            .revoke_approval(
                &Principal::current().unwrap(),
                &setup.run_id,
                approval["approval"]["approval_id"].as_str().unwrap(),
                "The reviewed application decision was withdrawn",
            )
            .unwrap();
        assert!(project
            .apply_application(&setup.run_id, &subject, true, false)
            .is_err());
        project
            .dispatch(Operation::Approve {
                run_id: setup.run_id.clone(),
                request: decision["request"].clone(),
            })
            .unwrap();
        let index = std::fs::read(project.root.join(".git/index")).unwrap();
        let applied = project
            .apply_application(&setup.run_id, &subject, true, false)
            .unwrap();
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["applicationReceipt"]["deliveredLocally"], true);
        assert_eq!(applied["applicationReceipt"]["current"], true);
        let (_, candidate_digest, observed_digest) = project
            .current_application_binding(&setup.run_id)
            .unwrap()
            .unwrap();
        assert_ne!(candidate_digest, observed_digest, "Deleted selected untracked input is a candidate tombstone, not a Git baseline deletion");
        let feedback = project.architecture_feedback(&setup.run_id).unwrap();
        assert_eq!(feedback["freshness"], "unavailable", "This fixture has no architecture checks; applied tombstones must not make its source stale");
        assert_eq!(
            feedback["evidenceCandidateDigest"],
            candidate_digest.as_str()
        );
        assert_eq!(feedback["currentSnapshotDigest"], observed_digest.as_str());
        assert_eq!(
            feedback["applicationBinding"]["candidateDigest"],
            candidate_digest.as_str()
        );
        assert_eq!(
            feedback["applicationBinding"]["observedSnapshotDigest"],
            observed_digest.as_str()
        );
        assert!(feedback["applicationBinding"]["receiptDigest"]
            .as_str()
            .is_some_and(|digest| !digest.is_empty()));
        assert!(!project.root.join(removed).exists());
        assert_eq!(
            std::fs::read_to_string(project.root.join(added)).unwrap(),
            "reviewed added file\n"
        );
        assert_eq!(
            std::fs::read(project.root.join(".git/index")).unwrap(),
            index
        );
        assert_eq!(
            std::fs::read_to_string(project.root.join("implementations/quota/lib.rs")).unwrap(),
            fixtures::QUOTA_RUST
        );
        assert_eq!(
            project
                .apply_application(&setup.run_id, &subject, true, false)
                .unwrap()["applicationReceipt"],
            applied["applicationReceipt"]
        );
        std::fs::write(
            project.root.join(added),
            "later edit to applied untracked file",
        )
        .unwrap();
        let changed = project.status(&setup.run_id).unwrap();
        assert_eq!(changed["applicationReceipt"]["applied"], true);
        assert_eq!(changed["applicationReceipt"]["current"], false);
        assert_eq!(changed["status"], "stale");
        assert!(project
            .current_application_binding(&setup.run_id)
            .unwrap()
            .is_none());
        let edited_feedback = project.architecture_feedback(&setup.run_id).unwrap();
        assert_eq!(edited_feedback["freshness"], "stale");
        assert!(edited_feedback["applicationBinding"].is_null());
    }
}
