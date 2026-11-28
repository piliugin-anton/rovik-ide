//! Authenticated intake promotion and bounded replanning preserve all earlier evidence.

use crate::{
    approvals::Principal,
    controller::{Controller, TaskSetup},
    identity::Digest,
    ledger::RunRecord,
    plan::ApprovalRequirement,
    require,
    roles::Preparation,
    runner,
    state::State,
    wire, Error, Result,
};

fn trial_feedback(
    controller: &Controller,
    setup: &TaskSetup,
    result: &crate::evidence::CheckResult,
) -> Result<serde_json::Value> {
    let mut value = crate::context::check_feedback(&setup.policy, result)?;
    if setup.policy.deny_read.is_empty() {
        // Adapter output includes real discovery even when a proposed case ID is wrong.
        let mut remaining = 16 * 1024;
        let mut output = Vec::new();
        for digest in &result.evidence {
            if remaining == 0 {
                break;
            }
            let bytes = controller.ledger.read(digest)?;
            let count = bytes.len().min(remaining);
            output.push(String::from_utf8_lossy(&bytes[..count]).into_owned());
            remaining -= count;
        }
        value["output"] = serde_json::json!(output);
    }
    Ok(value)
}

impl Controller {
    /// Exercise a proposed oracle before review without granting approvals or accepting code.
    /// Every attempt consumes the preparation's durable check budget; promotion keeps it.
    pub fn trial_preparation(
        &mut self,
        principal: &Principal,
        setup: &TaskSetup,
        cancel: &runner::Cancellation,
    ) -> Result<serde_json::Value> {
        let current = self.ledger.run(&setup.run_id)?;
        require(
            current.state == State::Intake,
            "preparation is not at intake",
        )?;
        let preparation: Preparation = wire::decode(&self.ledger.read(&current.input_digest)?)?;
        preparation.authority.authorize(principal, "operate")?;
        require(
            setup.repository_id == preparation.repository_id
                && setup.base == preparation.base
                && setup.budgets == preparation.budgets
                && setup.deadline_ms <= preparation.deadline_ms
                && Digest::of_json(&setup.authority)? == Digest::of_json(&preparation.authority)?,
            "trial changed the preparation's immutable authority or input",
        )?;
        setup.validate(&self.ledger)?;
        self.register_setup(setup, runner::now_ms()?)?;
        let baseline_key = self.select_binding(setup, &setup.base)?;
        let mut observations = Vec::new();
        let mut ready = true;
        for spec in setup.plan.required_final() {
            require(!cancel.is_cancelled(), "preparation trial cancelled")?;
            let result = self.check(
                setup,
                &setup.base,
                &baseline_key,
                spec,
                "preparation_baseline",
                cancel,
            )?;
            let expected = setup.baseline.expected_failures.iter().find(|expected| {
                expected.check_id == spec.check_id
                    && expected.configuration_digest == spec.configuration_digest
            });
            let matches = match expected {
                Some(expected) => expected.matches(&result)?,
                None => result.status == crate::evidence::Status::Pass,
            };
            ready &= matches;
            observations
                .push(serde_json::json!({"stage":"baseline", "matches":matches, "result":trial_feedback(self, setup, &result)?}));
        }
        let spec = setup
            .plan
            .required_final()
            .find(|spec| {
                setup
                    .registry
                    .get(&spec.command_id)
                    .is_ok_and(|command| command.kind == crate::plan::CheckKind::Behavioral)
                    && setup
                        .negative_control_cases
                        .iter()
                        .all(|case| spec.expected_case_ids.contains(case))
            })
            .ok_or_else(|| Error::new("no behavioral check covers the negative control".into()))?;
        require(!cancel.is_cancelled(), "preparation trial cancelled")?;
        let control_key = self.select_binding(setup, &setup.negative_control)?;
        let result = self.check(
            setup,
            &setup.negative_control,
            &control_key,
            spec,
            "preparation_negative_control",
            cancel,
        )?;
        let matches = result.status == crate::evidence::Status::Fail
            && setup
                .negative_control_cases
                .iter()
                .all(|case| result.failed_case_ids.contains(case));
        ready &= matches;
        observations.push(
            serde_json::json!({"stage":"negative_control", "matches":matches, "result":trial_feedback(self, setup, &result)?}),
        );
        let report = serde_json::json!({"ready":ready,"observations":observations});
        self.ledger
            .store_json("report", &report, runner::now_ms()?)?;
        Ok(report)
    }

    /// Promote a preparation intake into an executable setup without resetting its budget.
    /// Contract/design/oracle approvals are still checked at BASELINE before execution.
    pub fn promote_preparation(
        &mut self,
        principal: &Principal,
        setup: &TaskSetup,
    ) -> Result<RunRecord> {
        let current = self.ledger.run(&setup.run_id)?;
        require(
            current.state == State::Intake,
            "preparation is not at intake",
        )?;
        let preparation: Preparation = wire::decode(&self.ledger.read(&current.input_digest)?)?;
        setup.authority.authorize(principal, "operate")?;
        preparation.authority.authorize(principal, "operate")?;
        require(
            Digest::of_json(&setup.authority)? == Digest::of_json(&preparation.authority)?,
            "promotion cannot change the preparation's approval authority",
        )?;
        require(
            setup.run_id == preparation.run_id
                && setup.repository_id == preparation.repository_id
                && setup.base == preparation.base
                && setup.budgets == preparation.budgets
                && setup.deadline_ms <= preparation.deadline_ms,
            "promotion changed approved repository, input, budget or deadline",
        )?;
        let input = self.register_setup(setup, runner::now_ms()?)?;
        self.replace_input(
            principal,
            &current,
            &input,
            "preparation_promoted",
            false,
            None,
        )
    }

    /// Register a concrete proposed intake and return the exact approval it needs.
    /// Registration cannot activate it or grant any approval.
    pub fn stage_replan(
        &self,
        principal: &Principal,
        setup: &TaskSetup,
    ) -> Result<ApprovalRequirement> {
        let previous = self.setup(&setup.run_id)?;
        previous.authority.authorize(principal, "operate")?;
        self.validate_replan(&previous, setup)?;
        let digest = self.register_setup(setup, runner::now_ms()?)?;
        Ok(ApprovalRequirement {
            subject_digest: digest,
            approval_type: "replan".into(),
            obligation_ids: vec![],
        })
    }

    fn validate_replan(&self, previous: &TaskSetup, next: &TaskSetup) -> Result<()> {
        require(previous.run_id == next.run_id && previous.repository_id == next.repository_id
            && previous.base == next.base, "replan must preserve the authorized base; use merge-candidate preparation for another base")?;
        require(
            previous.budgets == next.budgets
                && next.deadline_ms <= previous.deadline_ms
                && Digest::of_json(&previous.authority)? == Digest::of_json(&next.authority)?,
            "replan cannot increase budgets, extend deadline or change approval authority",
        )?;
        require(
            previous
                .budgets
                .get("replans")
                .is_some_and(|limit| *limit > 0),
            "no replan budget was authorized",
        )
    }

    /// Activate an exact reviewed replacement, spending one replan atomically with invalidation.
    /// Every baseline, oracle and final check runs under a fresh input binding.
    pub fn activate_replan(
        &mut self,
        principal: &Principal,
        run_id: &str,
        proposed: &Digest,
    ) -> Result<RunRecord> {
        let previous = self.setup(run_id)?;
        previous.authority.authorize(principal, "operate")?;
        let setup: TaskSetup = wire::decode(&self.ledger.read(proposed)?)?;
        self.validate_replan(&previous, &setup)?;
        setup.validate(&self.ledger)?;
        let current = self.ledger.run(run_id)?;
        require(
            matches!(
                current.state,
                State::Blocked | State::Replan | State::Review | State::DesignReady
            ),
            "run is not awaiting a semantic replan",
        )?;
        self.replace_input(
            principal,
            &current,
            proposed,
            "approved_replan_activated",
            true,
            Some(&setup),
        )
    }

    fn replace_input(
        &mut self,
        principal: &Principal,
        current: &RunRecord,
        input: &Digest,
        kind: &str,
        charge_replan: bool,
        setup: Option<&TaskSetup>,
    ) -> Result<RunRecord> {
        let now = runner::now_ms()?;
        let revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| Error::new("run revision overflow".into()))?;
        let event = self.ledger.store_json("event", &serde_json::json!({"kind":kind,"previous_input":current.input_digest,
            "input":input,"actor":principal.actor_id(),"invalidated_binding":current.binding_digest,"replans_spent":u8::from(charge_replan)}), now)?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.ledger.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Some(setup) = setup {
            require(
                self.approvals_current(setup, None, now)?,
                "replacement contract/design/oracle/plan approvals are missing or stale",
            )?;
            require(
                self.ledger.approval_current(
                    &current.run_id,
                    &ApprovalRequirement {
                        subject_digest: input.clone(),
                        approval_type: "replan".into(),
                        obligation_ids: vec![],
                    },
                    &Digest::of_json(&setup.contract.scope)?,
                    &Digest::of_json(&setup.policy)?,
                    &setup.authority,
                    now,
                )?,
                "exact replacement intake needs authenticated replan approval",
            )?;
        }
        let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM operations WHERE run_id=?1 AND state NOT IN ('completed','failed'))",
            [&current.run_id], |row| row.get(0))?;
        require(
            !active
                && self
                    .ledger
                    .budgets(&current.run_id)?
                    .values()
                    .all(|b| b.reserved == 0 && b.spent <= b.limit),
            "pending operations or accounting must be reconciled before replacing intake",
        )?;
        if charge_replan {
            let charged = tx.execute("UPDATE budget_accounts SET spent_value=spent_value+1 WHERE run_id=?1 AND unit='replans' AND reserved_value=0 AND spent_value<limit_value", [&current.run_id])?;
            require(charged == 1, "replan budget exhausted")?;
        }
        let before =
            serde_json::to_string(&current.state).map_err(|e| Error::new(e.to_string()))?;
        let after =
            serde_json::to_string(&State::Baseline).map_err(|e| Error::new(e.to_string()))?;
        let changed = tx.execute("UPDATE runs SET state=?1,revision=?2,input_digest=?3,binding_digest=NULL,restart_state=NULL,updated_ms=?4 WHERE run_id=?5 AND revision=?6 AND input_digest=?7 AND state=?8",
            rusqlite::params![after, revision as i64, input.as_str(), now as i64, current.run_id, current.revision as i64, current.input_digest.as_str(), before])?;
        require(
            changed == 1,
            "concurrent intake replacement; reload before retrying",
        )?;
        tx.execute(
            "INSERT INTO events VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                current.run_id,
                revision as i64,
                before,
                after,
                event.digest.as_str(),
                now as i64
            ],
        )?;
        tx.commit()?;
        self.ledger.run(&current.run_id)
    }
}
