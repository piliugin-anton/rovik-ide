//! Operator recovery, explicit accounting and immutable failure dispositions.
use crate::{
    approvals::{ApprovalRequest, Principal},
    controller::Controller,
    evidence::{CheckResult, Status},
    identity::Digest,
    ledger::{Lease, Ledger},
    plan::ApprovalRequirement,
    require, runner,
    state::State,
    wire, Error, Result,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::new("accounting integer overflow".into()))
}
/// Durable operation status, including unresolved reservations.
#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    /// Controller-created operation identity.
    pub operation_id: String,
    /// Attempt identity, distinct from native check ID.
    pub operation_key: String,
    /// Model, verification or repair work.
    pub kind: String,
    /// Durable lifecycle state.
    pub state: String,
    /// Immutable request identity.
    pub input_digest: Digest,
    /// Authenticated completed output if any.
    pub output_digest: Option<Digest>,
    /// Reserved units retained until complete accounting.
    pub reservation: BTreeMap<String, u64>,
}
/// An authenticated operator supplies actual usage after an interrupted operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconciliation {
    /// Exact abandoned operation.
    pub operation_id: String,
    /// Actual units, not invented zeroes or an assumed reservation.
    pub usage: BTreeMap<String, u64>,
    /// Explanation and source of the accounting decision.
    pub reason: String,
}
/// Explicit resolution of a historical failure by exact replacement evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureResolution {
    /// Immutable historical failing/incomplete result.
    pub previous_result_id: String,
    /// Current successful result for the same obligation/configuration.
    pub replacement_result_id: String,
    /// Review rationale, including any nondeterminism or residual risk.
    pub reason: String,
}
impl Ledger {
    pub(crate) fn record_infrastructure_recovery(
        &self,
        previous: &CheckResult,
        replacement: &CheckResult,
        now: u64,
    ) -> Result<()> {
        require(
            previous.infrastructure_retryable()
                && replacement.status == Status::Pass
                && previous.result_id != replacement.result_id
                && previous.run_id == replacement.run_id
                && previous.identity() == replacement.identity()
                && previous.checker_digest == replacement.checker_digest
                && previous.evidence_key == replacement.evidence_key,
            "only an exact completed infrastructure retry can recover automatically",
        )?;
        let completed =
            self.completed_results(&previous.run_id, &Digest::of_json(&previous.evidence_key)?)?;
        require(
            completed.contains(previous) && completed.contains(replacement),
            "infrastructure recovery lacks authenticated receipts",
        )?;
        self.connection.execute(
            "INSERT INTO infrastructure_recoveries VALUES(?1,?2,?3)",
            params![previous.result_id, replacement.result_id, integer(now)?],
        )?;
        Ok(())
    }

    /// Explicit links from incomplete infrastructure observations to exact successful retries.
    /// These records never resolve a behavioral or compilation failure.
    pub fn infrastructure_recoveries(&self, run_id: &str) -> Result<Vec<FailureResolution>> {
        let mut query = self.connection.prepare("SELECT i.previous_result_id,i.replacement_result_id FROM infrastructure_recoveries i JOIN check_results c ON c.result_id=i.previous_result_id WHERE c.run_id=?1 ORDER BY i.created_ms,i.previous_result_id")?;
        let rows = query.query_map([run_id], |row| Ok(FailureResolution {
            previous_result_id: row.get(0)?, replacement_result_id: row.get(1)?,
            reason: "Bounded infrastructure retry completed for the same source, check and evidence binding.".into(),
        }))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }
    /// Private state root for trusted operators; never a worker capability.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Inspect the run's operations without changing their accounting.
    pub fn operations(&self, run_id: &str) -> Result<Vec<Operation>> {
        let mut statement=self.connection.prepare("SELECT operation_id,operation_key,kind,state,input_digest,output_digest,reservation_json FROM operations WHERE run_id=?1 ORDER BY rowid")?;
        let rows = statement.query_map([run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (operation_id, operation_key, kind, state, input, output, reservation) = row?;
            Ok(Operation {
                operation_id,
                operation_key,
                kind,
                state,
                input_digest: Digest::parse(&input)?,
                output_digest: output.map(|d| Digest::parse(&d)).transpose()?,
                reservation: wire::decode(reservation.as_bytes())?,
            })
        })
        .collect()
    }
    /// Persist the job identity before Docker can create a process.
    pub(crate) fn record_job(&self, lease: &Lease, job: &str, workspace: &Path) -> Result<()> {
        let tx =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        self.verify_lease(lease, runner::now_ms()?)?;
        require(
            workspace.starts_with(self.root.join("jobs")),
            "job workspace escaped private state",
        )?;
        tx.execute(
            "INSERT INTO jobs VALUES(?1,?2,?3,?4,NULL)",
            params![
                lease.operation_id,
                integer(lease.generation)?,
                job,
                workspace
                    .to_str()
                    .ok_or_else(|| Error::new("job workspace is not UTF-8".into()))?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn job_removed(&self, job: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE jobs SET removed_ms=?1 WHERE job_id=?2",
            params![integer(runner::now_ms()?)?, job],
        )?;
        Ok(())
    }
    /// A completed operation must authenticate exactly the stored result bytes.
    pub(crate) fn completed_check(
        &self,
        run_id: &str,
        key: &str,
        input: &Digest,
    ) -> Result<Option<CheckResult>> {
        let found:Option<(String,Option<String>,String)>=self.connection.query_row("SELECT state,output_digest,input_digest FROM operations WHERE run_id=?1 AND operation_key=?2",params![run_id,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((state, output, old_input)) = found else {
            return Ok(None);
        };
        require(
            old_input == input.as_str(),
            "attempt identity conflicts with immutable inputs",
        )?;
        if state != "completed" {
            return Ok(None);
        }
        let digest = Digest::parse(
            &output.ok_or_else(|| Error::new("completed operation has no output".into()))?,
        )?;
        let result: CheckResult = wire::decode(&self.read(&digest)?)?;
        let binding = Digest::of_json(&result.evidence_key)?;
        require(
            self.completed_results(run_id, &binding)?
                .iter()
                .any(|r| r.result_id == result.result_id),
            "completed operation has no authenticated receipt",
        )?;
        Ok(Some(result))
    }
    /// Read a result for explicit operator review, preserving non-completed history.
    pub fn result(&self, run_id: &str, result_id: &str) -> Result<CheckResult> {
        let digest: String = self.connection.query_row(
            "SELECT result_digest FROM check_results WHERE run_id=?1 AND result_id=?2",
            params![run_id, result_id],
            |r| r.get(0),
        )?;
        wire::decode(&self.read(&Digest::parse(&digest)?)?)
    }
}
impl Controller {
    pub(crate) fn candidate_results(
        &self,
        run_id: &str,
        binding: &Digest,
    ) -> Result<Vec<CheckResult>> {
        let historical: BTreeSet<_> = self
            .ledger
            .operations(run_id)?
            .into_iter()
            .filter(|operation| matches!(operation.kind.as_str(), "baseline" | "negative_control"))
            .map(|operation| operation.operation_id)
            .collect();
        Ok(self
            .ledger
            .completed_results(run_id, binding)?
            .into_iter()
            .filter(|result| !historical.contains(&result.operation_id))
            .collect())
    }
    fn management_registration(
        &self,
        run_id: &str,
    ) -> Result<(
        crate::approvals::AuthorityPolicy,
        Option<crate::runner::Environment>,
    )> {
        let current = self.ledger.run(run_id)?;
        let reference = self.ledger.reference(&current.input_digest)?;
        if reference.kind == "preparation" {
            let preparation: crate::roles::Preparation =
                wire::decode(&self.ledger.read(&current.input_digest)?)?;
            preparation.base.validate(&self.ledger, true)?;
            preparation.worker.validate()?;
            Ok((preparation.authority, None))
        } else {
            let setup = self.setup(run_id)?;
            Ok((setup.authority, Some(setup.environment)))
        }
    }
    /// Resume only the durable restart stage after prerequisites and accounting are repaired.
    pub fn resume(&mut self, principal: &Principal, run_id: &str) -> Result<()> {
        let (authority, environment) = self.management_registration(run_id)?;
        authority.authorize(principal, "operate")?;
        if environment.is_some() {
            self.setup(run_id)?.validate(&self.ledger)?;
        }
        self.ledger.verify_artifacts()?;
        let current = self.ledger.run(run_id)?;
        require(
            current.state == State::Blocked,
            "only a blocked run can resume",
        )?;
        require(
            !self
                .ledger
                .operations(run_id)?
                .iter()
                .any(|o| o.state == "running" || o.state == "abandoned"),
            "active or abandoned operations need recovery and accounting before resuming",
        )?;
        require(
            self.ledger
                .budgets(run_id)?
                .values()
                .all(|b| b.reserved == 0 && b.spent <= b.limit),
            "unresolved usage or exhausted budget blocks resume",
        )?;
        let next = current
            .restart_state
            .ok_or_else(|| Error::new("blocked run has no restart stage".into()))?;
        let event = self.ledger.store_json(
            "event",
            &serde_json::json!({"kind":"operator_resume","actor":principal.actor_id()}),
            runner::now_ms()?,
        )?;
        self.ledger
            .transition(&current, next, &event.digest, None, runner::now_ms()?)?;
        Ok(())
    }
    /// Cancel durably. Running adapters observe this state and terminate their jobs.
    pub fn cancel(&mut self, principal: &Principal, run_id: &str) -> Result<()> {
        self.management_registration(run_id)?
            .0
            .authorize(principal, "operate")?;
        let current = self.ledger.run(run_id)?;
        if current.state == State::Cancelled {
            return Ok(());
        }
        let event = self.ledger.store_json(
            "event",
            &serde_json::json!({"kind":"operator_cancel","actor":principal.actor_id()}),
            runner::now_ms()?,
        )?;
        self.ledger.transition(
            &current,
            State::Cancelled,
            &event.digest,
            None,
            runner::now_ms()?,
        )?;
        Ok(())
    }
    /// Fence interrupted work, kill only its registered jobs and retain unknown usage.
    /// This explicit management action also interrupts any still-running controller.
    pub fn recover(&mut self, principal: &Principal, run_id: &str) -> Result<Vec<String>> {
        let (authority, environment) = self.management_registration(run_id)?;
        authority.authorize(principal, "operate")?;
        if let Some(environment) = environment {
            self.backend.require_environment(&environment)?;
        }
        let current = self.ledger.run(run_id)?;
        require(
            current.state != State::Accepted,
            "accepted history cannot be recovered as new work",
        )?;
        let now = runner::now_ms()?;
        let event = self.ledger.store_json(
            "event",
            &serde_json::json!({"kind":"operator_recovery","actor":principal.actor_id()}),
            now,
        )?;
        // Block first; a late completion cannot then advance the workflow. Fencing
        // below invalidates every previously issued callback even across restarts.
        if !current.state.terminal() && current.state != State::Blocked {
            let restart = if current.state == State::ReadyForApproval {
                State::CleanCi
            } else {
                current.state
            };
            self.ledger
                .transition(&current, State::Blocked, &event.digest, Some(restart), now)?;
        }
        self.ledger.connection.execute("UPDATE operations SET state='abandoned',generation=generation+1 WHERE run_id=?1 AND state='running'",[run_id])?;
        let jobs = {
            // Recheck even previously removed identities: a daemon may finish a
            // create request after the controller lost its connection.
            let mut statement=self.ledger.connection.prepare("SELECT j.job_id,j.workspace FROM jobs j JOIN operations o ON o.operation_id=j.operation_id WHERE o.run_id=?1")?;
            let rows = statement
                .query_map([run_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let mut removed = vec![];
        for (job, workspace) in jobs {
            let existed = self.backend.remove_registered_job(&job)?;
            let workspace = PathBuf::from(workspace);
            require(
                workspace.starts_with(self.ledger.root.join("jobs")),
                "registered workspace escaped private state",
            )?;
            if workspace.exists() {
                require(
                    workspace
                        .canonicalize()?
                        .starts_with(self.ledger.root.join("jobs").canonicalize()?),
                    "job workspace changed identity",
                )?;
                std::fs::remove_dir_all(workspace)?;
            }
            self.ledger.job_removed(&job)?;
            if existed {
                removed.push(job);
            }
        }
        self.ledger.verify_artifacts()?;
        Ok(removed)
    }
    /// Settle an interrupted attempt with authenticated actual usage, never promote its output.
    pub fn reconcile(
        &mut self,
        principal: &Principal,
        run_id: &str,
        request: &Reconciliation,
    ) -> Result<()> {
        self.management_registration(run_id)?
            .0
            .authorize(principal, "accounting")?;
        require(
            !request.reason.trim().is_empty(),
            "accounting reconciliation needs a rationale",
        )?;
        let now = runner::now_ms()?;
        let record = self.ledger.store_json(
            "receipt",
            &serde_json::json!({"actor":principal.actor_id(),"request":request}),
            now,
        )?;
        let tx = self
            .ledger
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (state, reservation): (String, String) = tx.query_row(
            "SELECT state,reservation_json FROM operations WHERE run_id=?1 AND operation_id=?2",
            params![run_id, request.operation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        require(
            state == "abandoned",
            "only abandoned work can receive recovered accounting",
        )?;
        let reservation: BTreeMap<String, u64> = wire::decode(reservation.as_bytes())?;
        require(
            request.usage.keys().eq(reservation.keys()),
            "reconciliation must cover every reserved unit",
        )?;
        for (unit, reserved) in &reservation {
            let changed=tx.execute("UPDATE budget_accounts SET reserved_value=reserved_value-?1,spent_value=spent_value+?2 WHERE run_id=?3 AND unit=?4 AND reserved_value>=?1 AND spent_value<=9223372036854775807-?2",params![integer(*reserved)?,integer(request.usage[unit])?,run_id,unit])?;
            require(
                changed == 1,
                "inconsistent recovered usage or accounting overflow",
            )?;
        }
        tx.execute(
            "INSERT INTO reconciliations VALUES(?1,?2,?3,?4)",
            params![
                request.operation_id,
                principal.actor_id(),
                record.digest.as_str(),
                integer(now)?
            ],
        )?;
        tx.execute(
            "INSERT INTO usage_receipts VALUES(?1,?2,?3)",
            params![request.operation_id, record.digest.as_str(), integer(now)?],
        )?;
        tx.execute(
            "UPDATE operations SET state='failed' WHERE operation_id=?1",
            [&request.operation_id],
        )?;
        tx.commit()?;
        let current = self.ledger.run(run_id)?;
        if !current.state.terminal()
            && self
                .ledger
                .budgets(run_id)?
                .values()
                .any(|b| b.spent > b.limit)
        {
            let event=self.ledger.store_json("event",&serde_json::json!({"kind":"recovered_usage_exceeded_budget","operation":request.operation_id}),now)?;
            self.ledger
                .transition(&current, State::Stopped, &event.digest, None, now)?;
        }
        Ok(())
    }
    /// Review an exact historical failure and replacement; current failing checks remain failures.
    pub fn resolve_failure(
        &mut self,
        principal: &Principal,
        run_id: &str,
        resolution: &FailureResolution,
    ) -> Result<String> {
        let setup = self.setup(run_id)?;
        setup.authority.authorize(principal, "failure-resolution")?;
        require(
            !resolution.reason.trim().is_empty(),
            "failure resolution needs a rationale",
        )?;
        let previous = self.ledger.result(run_id, &resolution.previous_result_id)?;
        let previous_state: String = self.ledger.connection.query_row(
            "SELECT state FROM operations WHERE run_id=?1 AND operation_id=?2",
            params![run_id, previous.operation_id],
            |r| r.get(0),
        )?;
        require(
            previous.status != Status::Pass || previous_state != "completed",
            "a completed passing result is not a historical failure",
        )?;
        let replacement = self
            .ledger
            .result(run_id, &resolution.replacement_result_id)?;
        require(
            previous.result_id != replacement.result_id
                && previous.identity() == replacement.identity()
                && replacement.status == Status::Pass,
            "resolution needs a passing replacement of the same check/configuration",
        )?;
        let binding = self
            .ledger
            .run(run_id)?
            .binding_digest
            .ok_or_else(|| Error::new("run has no candidate".into()))?;
        require(
            Digest::of_json(&replacement.evidence_key)? == binding
                && self
                    .ledger
                    .completed_results(run_id, &binding)?
                    .iter()
                    .any(|r| r.result_id == replacement.result_id),
            "replacement is not current authenticated evidence",
        )?;
        let now = runner::now_ms()?;
        let subject = self.ledger.store_json("exception", resolution, now)?;
        let approval = self.ledger.grant(
            principal,
            &setup.authority,
            ApprovalRequest {
                run_id: Some(run_id.into()),
                subject_digest: subject.digest,
                approval_type: "failure-resolution".into(),
                policy_digest: Digest::of_json(&setup.policy)?,
                scope_digest: Digest::of_json(&setup.contract.scope)?,
                expires_ms: None,
            },
            now,
        )?;
        self.ledger.connection.execute(
            "INSERT INTO failure_resolutions VALUES(?1,?2,?3,?4)",
            params![
                uuid::Uuid::new_v4().to_string(),
                previous.result_id,
                approval.approval_id,
                integer(now)?
            ],
        )?;
        Ok(approval.approval_id)
    }
    pub(crate) fn resolved_failures(
        &self,
        setup: &crate::controller::TaskSetup,
        results: &[CheckResult],
    ) -> Result<BTreeSet<String>> {
        let mut statement=self.ledger.connection.prepare("SELECT f.result_id,a.subject_digest FROM failure_resolutions f JOIN approvals a ON a.approval_id=f.approval_id JOIN check_results c ON c.result_id=f.result_id WHERE c.run_id=?1 AND a.approval_type='failure-resolution'")?;
        let mut resolved = self.replanned_resolved(setup, results)?;
        for recovery in self.ledger.infrastructure_recoveries(&setup.run_id)? {
            let previous = self
                .ledger
                .result(&setup.run_id, &recovery.previous_result_id)?;
            if previous.infrastructure_retryable()
                && results.iter().any(|replacement| {
                    replacement.result_id == recovery.replacement_result_id
                        && replacement.status == Status::Pass
                        && replacement.identity() == previous.identity()
                        && replacement.checker_digest == previous.checker_digest
                        && replacement.evidence_key == previous.evidence_key
                })
            {
                resolved.insert(previous.result_id);
            }
        }
        for row in statement.query_map([&setup.run_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (result_id, digest) = row?;
            let subject = Digest::parse(&digest)?;
            let resolution: FailureResolution = wire::decode(&self.ledger.read(&subject)?)?;
            let previous = self.ledger.result(&setup.run_id, &result_id)?;
            if resolution.previous_result_id == result_id
                && results.iter().any(|r| {
                    r.result_id == resolution.replacement_result_id
                        && r.identity() == previous.identity()
                        && r.status == Status::Pass
                })
                && self.ledger.approval_current(
                    &setup.run_id,
                    &ApprovalRequirement {
                        subject_digest: subject,
                        approval_type: "failure-resolution".into(),
                        obligation_ids: vec![],
                    },
                    &Digest::of_json(&setup.contract.scope)?,
                    &Digest::of_json(&setup.policy)?,
                    &setup.authority,
                    runner::now_ms()?,
                )?
            {
                resolved.insert(result_id);
            }
        }
        Ok(resolved)
    }
}

impl Controller {
    /// Exact missing or expired approvals for the current candidate and immutable setup.
    pub fn pending_approvals(&self, run_id: &str) -> Result<Vec<ApprovalRequirement>> {
        let setup = self.setup(run_id)?;
        let candidate = self.current_candidate(run_id)?;
        let mut pending = vec![];
        for requirement in setup.approval_requirements(candidate.as_ref())? {
            if !self.ledger.approval_current(
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
    /// Revoke a CLI approval only in the run whose authority was authenticated.
    pub fn revoke_approval(
        &self,
        principal: &Principal,
        run_id: &str,
        approval_id: &str,
        reason: &str,
    ) -> Result<()> {
        let setup = self.setup(run_id)?;
        let owner: Option<String> = self.ledger.connection.query_row(
            "SELECT run_id FROM approvals WHERE approval_id=?1",
            [approval_id],
            |r| r.get(0),
        )?;
        require(
            owner.as_deref() == Some(run_id),
            "approval belongs to another run or is globally scoped",
        )?;
        self.ledger.revoke(
            principal,
            &setup.authority,
            approval_id,
            reason,
            runner::now_ms()?,
        )
    }
}
impl Ledger {
    /// Immutable approval history with current expiry and revocation information.
    pub fn approval_history(&self, run_id: &str) -> Result<Vec<serde_json::Value>> {
        let mut statement=self.connection.prepare("SELECT a.approval_digest,EXISTS(SELECT 1 FROM approval_revocations r WHERE r.approval_id=a.approval_id) FROM approvals a WHERE a.run_id=?1 ORDER BY a.created_ms")?;
        let mut approvals = vec![];
        for row in statement.query_map([run_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
        })? {
            let (digest, revoked) = row?;
            let approval: crate::approvals::Approval =
                wire::decode(&self.read(&Digest::parse(&digest)?)?)?;
            approvals.push(serde_json::json!({"expired":approval.expires_ms.is_some_and(|until|runner::now_ms().map_or(true,|now|now>=until)),"approval":approval,"revoked":revoked}));
        }
        Ok(approvals)
    }
}
