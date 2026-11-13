//! Explicit fresh verification consumes new native attempts, never cached verdicts.
use crate::{
    approvals::Principal,
    controller::{Controller, Integration, TaskSetup},
    evidence::{CheckResult, GateDecision, Status},
    plan::CheckSpec,
    require,
    runner::{self, Cancellation},
    snapshot::Snapshot,
    state::State,
    Error, Result,
};
use std::collections::BTreeSet;
impl Controller {
    /// Consume exact delivery approval while serializing approval revocation and
    /// acceptance changes around an adapter's conditional destination update.
    pub fn deliver_authorized<T>(
        &self,
        principal: &Principal,
        run_id: &str,
        subject: &crate::identity::Digest,
        integration: &dyn Integration,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let setup = self.setup(run_id)?;
        setup.authority.authorize(principal, "delivery")?;
        self.ledger.read(subject)?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.ledger.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        require(
            self.ledger.run(run_id)?.state == State::Accepted
                && self.verify(run_id, integration)?.verdict == crate::evidence::Verdict::Ready,
            "delivery requires a current accepted integration candidate",
        )?;
        require(
            self.ledger.approval_current(
                run_id,
                &crate::plan::ApprovalRequirement {
                    subject_digest: subject.clone(),
                    approval_type: "delivery".into(),
                    obligation_ids: vec![],
                },
                &crate::identity::Digest::of_json(&setup.contract.scope)?,
                &crate::identity::Digest::of_json(&setup.policy)?,
                &setup.authority,
                runner::now_ms()?,
            )?,
            "exact delivery request lacks current authenticated approval",
        )?;
        let output = action()?;
        tx.commit()?;
        Ok(output)
    }
    /// Execute the complete final matrix again for an active selected candidate.
    /// Terminal runs remain immutable; create a verification run with an initial
    /// candidate to recheck an accepted result. Model reviews may become stale.
    pub fn verify_fresh(
        &mut self,
        principal: &Principal,
        run_id: &str,
        integration: &dyn Integration,
        cancel: &Cancellation,
    ) -> Result<GateDecision> {
        let setup = self.setup(run_id)?;
        setup.authority.authorize(principal, "operate")?;
        setup.validate(&self.ledger)?;
        let current = self.ledger.run(run_id)?;
        let state = if current.state == State::Blocked {
            current.restart_state.unwrap_or(State::Intake)
        } else {
            current.state
        };
        require(matches!(state,State::FastChecks|State::RiskChecks|State::Review|State::CleanCi|State::ReadyForApproval),
            "fresh verification needs an active implementation candidate; use an initial-candidate run to recheck a terminal result")?;
        require(
            runner::now_ms()? < setup.deadline_ms,
            "verification deadline exhausted",
        )?;
        require(
            self.approvals_current(&setup, None, runner::now_ms()?)?,
            "verification plan approvals are missing or stale",
        )?;
        require(
            self.ledger
                .operations(run_id)?
                .iter()
                .all(|op| op.state == "completed" || op.state == "failed")
                && self
                    .ledger
                    .budgets(run_id)?
                    .values()
                    .all(|account| account.reserved == 0 && account.spent <= account.limit),
            "pending execution or unknown accounting must be reconciled before fresh verification",
        )?;
        let candidate = self
            .current_candidate(run_id)?
            .ok_or_else(|| Error::new("no candidate selected".into()))?;
        require(
            integration.current_revision(&setup.repository_id)? == candidate.base_revision,
            "destination moved; reconstruct the merge candidate before verification",
        )?;
        if current.state == State::Blocked {
            self.resume(principal, run_id)?;
        }
        let mut key = self.key(&setup, &candidate)?;
        key.verification_nonce = Some(crate::identity::Digest::of(uuid::Uuid::new_v4().as_bytes()));
        let binding = self
            .ledger
            .store_json("operation", &key, runner::now_ms()?)?;
        self.ledger.bind(
            &self.ledger.run(run_id)?,
            &binding.digest,
            runner::now_ms()?,
        )?;
        let (cancel, _monitor) = crate::controller::ExecutionMonitor::start(
            self.ledger.root(),
            run_id,
            cancel.clone(),
            setup.deadline_ms,
        )?;
        let mut pending: Vec<_> = setup.plan.required_final().collect();
        let mut done = BTreeSet::new();
        while !pending.is_empty() {
            let at = pending
                .iter()
                .position(|check| {
                    check.dependencies.iter().all(|dependency| {
                        done.contains(&(dependency.clone(), check.configuration_digest.clone()))
                            || setup.plan.checks.iter().any(|prior| {
                                prior.check_id == *dependency
                                    && prior.phase != crate::plan::Phase::Final
                            })
                    })
                })
                .ok_or_else(|| {
                    Error::new("final verification dependencies cannot be scheduled".into())
                })?;
            let check = pending.remove(at);
            let result = self.check(&setup, &candidate, &key, check, "check", &cancel)?;
            if result.status != Status::Pass || cancel.is_cancelled() {
                break;
            }
            done.insert((check.check_id.clone(), check.configuration_digest.clone()));
        }
        self.verify(run_id, integration)
    }
}

pub(crate) fn run_matrix(
    controller: &mut Controller,
    config: &TaskSetup,
    candidate: &Snapshot,
    selected: &[CheckSpec],
    cancel: &Cancellation,
) -> Result<Vec<CheckResult>> {
    let key = controller.select_binding(config, candidate)?;
    let mut pending: Vec<_> = selected.iter().collect();
    let mut done = BTreeSet::new();
    let mut results = vec![];
    while !pending.is_empty() {
        let at = pending
            .iter()
            .position(|check| {
                check.dependencies.iter().all(|dependency| {
                    done.contains(&(dependency.clone(), check.configuration_digest.clone()))
                        || !selected.iter().any(|prior| {
                            prior.check_id == *dependency
                                && prior.configuration_digest == check.configuration_digest
                        })
                })
            })
            .ok_or_else(|| Error::new("study check dependencies cannot be scheduled".into()))?;
        require(
            !cancel.is_cancelled() && runner::now_ms()? < config.deadline_ms,
            "study cancelled or deadline exhausted",
        )?;
        let check = pending.remove(at);
        let result = controller.check(config, candidate, &key, check, "matrix", cancel)?;
        done.insert((check.check_id.clone(), check.configuration_digest.clone()));
        results.push(result);
    }
    Ok(results)
}
