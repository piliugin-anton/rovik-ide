//! Live-provider comparisons with equal per-stage budgets and withheld native judgments.
//! Benchmark decisions never authorize production acceptance, delivery or deployment.
use crate::{
    approvals::Principal,
    context::{self, ContextBundle, ContextItem, Trust},
    controller::{Controller, TaskSetup, WorkerLimits},
    evidence::{CheckResult, Status},
    identity::Digest,
    profile::Language,
    provider::Provider,
    require,
    runner::{self, Cancellation},
    snapshot::Snapshot,
    verification::run_matrix as checks,
    wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
/// Comparable generation/verification arms, with the same provider and resource caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    /// Public request, existing repository instructions and the registered existing CI subset.
    ExistingCi,
    /// Adds approved contract, design and profiles; retains the existing CI subset.
    Guided,
    /// Same guidance, with every required native check in the complete approved plan.
    Enforced,
}
const ARMS: [Arm; 3] = [Arm::ExistingCi, Arm::Guided, Arm::Enforced];
/// A stage binds public generation inputs separately from independent held-out judgment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    /// Reviewed TaskSetup artifact containing source, profiles and the enforced plan.
    pub setup: Digest,
    /// Independent TaskSetup with the same base/profiles but different protected tests.
    /// Its contract, tests, cases and diagnostics never enter generation context.
    pub heldout: Digest,
    /// Public task text as an immutable UTF-8 artifact.
    pub request: Digest,
    /// Existing repository instructions, treated as non-authoritative text.
    pub existing_instructions: Digest,
    /// Existing CI check IDs selected from the full registered final matrix.
    pub existing_checks: BTreeSet<String>,
}
/// Ordered follow-on stages carry the actual preceding proposal, never a reference answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyCase {
    /// Stable case identity, independent of any demonstrated quota task.
    pub case_id: String,
    /// Primary language; mixed-language profiles remain part of the exact setup.
    pub language: Language,
    /// Workload stratum such as repair, refactor, parsing, interop or extension.
    pub workload: String,
    /// Registered dataset risk group, reported separately.
    pub risk_group: String,
    /// A reviewed negative example where introducing a new abstraction is unnecessary.
    pub no_new_abstraction: bool,
    /// One or more ordered tasks, including preserved-behavior checks in later stages.
    pub stages: Vec<Stage>,
}
/// Versioned, operator-selected repositories and independent test assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Study {
    /// Exactly 1.
    pub schema_version: u32,
    /// New identity for an intentionally new experiment; reruns cannot reset existing accounts.
    pub study_id: String,
    /// Dataset version/source description. Development smoke fixtures must say so.
    pub dataset_version: String,
    /// At least two repeated trials per case and arm.
    pub repetitions: u32,
    /// Identical model, context, output and per-call pricing reservations across all arms.
    pub worker: WorkerLimits,
    /// Equal per-stage caps including all failed model and check attempts.
    pub budgets: BTreeMap<String, u64>,
    /// Equal maximum proposal/repair iterations per stage.
    pub maximum_proposals: u32,
    /// Equal wall-clock allowance per stage, including generation and native execution.
    pub maximum_duration_ms: u64,
    /// Explicit upper planned model spend for the entire study, in priced microunits.
    pub maximum_total_cost: u64,
    /// Versioned cases; all six supported language families can use this same protocol.
    pub cases: Vec<StudyCase>,
}
/// Measured outcome of one arm/stage. Unknown outcomes are never recoded as passing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trial {
    /// Case identity.
    pub case_id: String,
    /// Zero-based repeated trial.
    pub repetition: u32,
    /// Selected comparison arm.
    pub arm: Arm,
    /// Zero-based sequential stage.
    pub stage: usize,
    /// Durable model and native execution run.
    pub run_id: String,
    /// Selected proposal, including failed proposals when available.
    pub candidate: Option<Digest>,
    /// Actual preceding stage proposal used as input.
    pub predecessor: Option<Digest>,
    /// All selected arm checks passed; this is a benchmark decision, never merge authority.
    pub accepted_by_arm: bool,
    /// Independent post-generation judgment; None means missing, errored or unsupported.
    pub heldout_correct: Option<bool>,
    /// Extra implementation proposals beyond the first.
    pub repairs: u32,
    /// Blocked semantic decisions requiring an operator, not fabricated human review time.
    pub decision_requests: u32,
    /// Actual changed files in this stage relative to its incoming proposal.
    pub changed_files: u64,
    /// Billed model calls, tokens, monetary microunits and native checks, including failures.
    pub spent: BTreeMap<String, u64>,
    /// Unknown usage remains visible and prevents continuing the study.
    pub reserved: BTreeMap<String, u64>,
    /// Complete stage wall time.
    pub duration_ms: u64,
    /// Authenticated final native results used for the arm decision.
    pub results: Vec<String>,
    /// Authenticated results of the separate withheld verification run.
    pub heldout_results: Vec<String>,
    /// Covered and required native observations, as integer counts.
    pub evidence_completeness: [u64; 2],
    /// Any blocked prerequisite, incomplete accounting or skipped dependent stage.
    pub reason: Option<String>,
}
/// Persisted comparison report; statistics include failed and blocked trials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyReport {
    /// Exactly 1.
    pub schema_version: u32,
    /// Exact pinned dataset/settings identity.
    pub study: Digest,
    /// Explicitly distinguishes actual provider calls from the deterministic verifier bank.
    pub methodology: String,
    /// Every completed stage; incomplete studies retain partial observations.
    pub trials: Vec<Trial>,
    /// False when cancellation, unknown accounting or a prerequisite stopped execution.
    pub complete: bool,
    /// Why an incomplete study stopped.
    pub stopped_reason: Option<String>,
}
fn setup(controller: &Controller, digest: &Digest) -> Result<TaskSetup> {
    wire::decode(&controller.ledger.read(digest)?)
}
impl Study {
    /// Validate capabilities and calculate the maximum planned cost before any provider call.
    pub fn preflight(&self, controller: &Controller, principal: &Principal) -> Result<u64> {
        require(self.schema_version == 1, "unsupported study version")?;
        crate::id(&self.study_id)?;
        require(
            !self.dataset_version.trim().is_empty()
                && (2..=1000).contains(&self.repetitions)
                && (1..=20).contains(&self.maximum_proposals)
                && (1..=86_400_000).contains(&self.maximum_duration_ms)
                && !self.cases.is_empty()
                && self.cases.len() <= 500,
            "study needs versioned cases, bounded proposals/duration and repeated trials",
        )?;
        self.worker.validate()?;
        for unit in ["model_calls", "tokens", "cost_microunits", "checks"] {
            require(
                self.budgets.contains_key(unit),
                format!("study is missing {unit} budget"),
            )?;
        }
        require(
            self.budgets["model_calls"] > 0
                && self.budgets["checks"] > 0
                && self.budgets["tokens"] >= self.worker.token_reservation
                && self.budgets["cost_microunits"] >= self.worker.cost_reservation,
            "study budgets cannot fund a single bounded attempt",
        )?;
        crate::unique(self.cases.iter().map(|case| case.case_id.as_str()))?;
        let mut stages = 0_u64;
        for case in &self.cases {
            require(
                !case.workload.trim().is_empty()
                    && !case.risk_group.trim().is_empty()
                    && !case.stages.is_empty()
                    && case.stages.len() <= 20,
                "study case needs workload, risk and stages",
            )?;
            for stage in &case.stages {
                let public = setup(controller, &stage.setup)?;
                let hidden = setup(controller, &stage.heldout)?;
                for config in [&public, &hidden] {
                    config.validate(&controller.ledger)?;
                    controller
                        .backend
                        .require_environment(&config.environment)?;
                    config.authority.authorize(principal, "operate")?;
                    require(config.reviewer.is_none() && config.plan.review_requirements.is_empty()
                        && config.plan.approval_requirements.is_empty() && config.exceptions.is_empty(),
                        "automated native studies cannot replace required human/model review or exception decisions; use reviewed cases without those pending requirements")?;
                }
                require(public.profiles.iter().any(|profile|profile.language==case.language)
                    && public.base==hidden.base && Digest::of_json(&public.profiles)?==Digest::of_json(&hidden.profiles)?
                    && public.environment.image_digest==hidden.environment.image_digest && public.dependencies==hidden.dependencies,
                    "held-out judgment must use the same source, language configurations, dependency assets and pinned image")?;
                require(
                    public.oracle.digest()? != hidden.oracle.digest()?,
                    "held-out test assets must be independent of generation/arm tests",
                )?;
                let checks: BTreeSet<_> = public
                    .plan
                    .required_final()
                    .map(|check| check.check_id.clone())
                    .collect();
                require(
                    !stage.existing_checks.is_empty() && stage.existing_checks.is_subset(&checks),
                    "existing CI subset names missing final checks",
                )?;
                for check in public
                    .plan
                    .required_final()
                    .filter(|check| stage.existing_checks.contains(&check.check_id))
                {
                    require(
                        check.dependencies.iter().all(|dependency| {
                            stage.existing_checks.contains(dependency)
                                || !public.plan.required_final().any(|prior| {
                                    prior.check_id == *dependency
                                        && prior.configuration_digest == check.configuration_digest
                                })
                        }),
                        "existing CI subset omitted a final dependency",
                    )?;
                }
                for artifact in [&stage.request, &stage.existing_instructions] {
                    require(
                        std::str::from_utf8(&controller.ledger.read(artifact)?).is_ok(),
                        "public study text must be UTF-8",
                    )?;
                }
                stages = stages
                    .checked_add(1)
                    .ok_or_else(|| Error::new("study size overflow".into()))?;
            }
        }
        let upper = stages
            .checked_mul(u64::from(self.repetitions))
            .and_then(|n| n.checked_mul(3))
            .and_then(|n| n.checked_mul(self.budgets["cost_microunits"]))
            .ok_or_else(|| Error::new("study spend overflow".into()))?;
        require(
            upper <= self.maximum_total_cost,
            "total planned model spend exceeds the approved study cap",
        )?;
        Ok(upper)
    }
}
fn context(
    controller: &Controller,
    config: &TaskSetup,
    candidate: &Snapshot,
    stage: &Stage,
    arm: Arm,
) -> Result<ContextBundle> {
    let revision = candidate.digest()?;
    let mut items = if arm == Arm::ExistingCi {
        vec![]
    } else {
        controller.context(config, candidate)?.items
    };
    for (label, digest) in [
        ("public-request", &stage.request),
        ("existing-instructions", &stage.existing_instructions),
    ] {
        items.push(ContextItem {
            item_id: label.into(),
            artifact_digest: digest.clone(),
            source_revision: revision.clone(),
            trust: Trust::UntrustedText,
            mandatory: true,
            text: String::from_utf8(controller.ledger.read(digest)?)
                .map_err(|e| Error::new(e.to_string()))?,
        });
    }
    if arm == Arm::ExistingCi {
        // Every arm retains the same non-negotiable host and protected-asset boundary.
        // Baseline receives mapping IDs and scope, without contract/design guidance.
        let capability = serde_json::json!({"scope":config.contract.scope,"required_mapping_ids":config.contract.invariants.iter().map(|i|&i.id).collect::<Vec<_>>()});
        let artifact = controller
            .ledger
            .store_json("context", &capability, runner::now_ms()?)?;
        items.push(ContextItem {
            item_id: "capabilities".into(),
            artifact_digest: artifact.digest,
            source_revision: revision.clone(),
            trust: Trust::AuthoritativePolicy,
            mandatory: true,
            text: capability.to_string(),
        });
        items.extend(context::ranked_sources(
            &controller.ledger,
            candidate,
            &config.contract.intent,
            &[],
            Some(&config.contract.scope),
            &config.policy.deny_read,
        )?);
    }
    context::build(
        &controller.ledger,
        items,
        config.worker.context_bytes.saturating_sub(4096),
    )
}
fn configure(mut config: TaskSetup, study: &Study, run_id: &str) -> Result<TaskSetup> {
    config.run_id = run_id.into();
    config.plan.run_id = run_id.into();
    config.worker = study.worker.clone();
    config.budgets = study.budgets.clone();
    config.deadline_ms = runner::now_ms()?
        .checked_add(study.maximum_duration_ms)
        .ok_or_else(|| Error::new("study deadline overflow".into()))?;
    config.initial_candidate = None;
    Ok(config)
}
fn carry(base: &Snapshot, previous: Option<&(Snapshot, Snapshot)>) -> Result<Snapshot> {
    let mut incoming = base.clone();
    if let Some((old, proposal)) = previous {
        let paths: BTreeSet<_> = old
            .files
            .keys()
            .chain(proposal.files.keys())
            .cloned()
            .collect();
        for path in paths {
            if old.files.get(&path) == proposal.files.get(&path) {
                continue;
            }
            require(
                base.files.get(&path) == old.files.get(&path)
                    || base.files.get(&path) == proposal.files.get(&path),
                format!("sequential input conflicts with a changed dependency/source at {path}"),
            )?;
            match proposal.files.get(&path) {
                Some(file) => {
                    incoming.files.insert(path.clone(), file.clone());
                    incoming.deletions.retain(|deleted| deleted != &path);
                }
                None => {
                    incoming.files.remove(&path);
                    if base.files.contains_key(&path) {
                        incoming.deletions.push(path);
                    }
                }
            }
        }
    }
    Ok(incoming)
}

fn truth(results: &[CheckResult]) -> Option<bool> {
    if results.iter().any(|result| result.status == Status::Fail) {
        Some(false)
    } else if !results.is_empty() && results.iter().all(|result| result.status == Status::Pass) {
        Some(true)
    } else {
        None
    }
}
impl Controller {
    /// Run a live-provider study after preflight. No arm can edit verifier assets,
    /// bypass host isolation, grant production approval, deliver or deploy code.
    /// Existing study run identities prevent accidental duplicate spend on rerun.
    pub fn evaluate_study(
        &mut self,
        principal: &Principal,
        study: &Study,
        provider: &mut dyn Provider,
        cancel: &Cancellation,
    ) -> Result<StudyReport> {
        study.preflight(self, principal)?;
        let study_digest = self
            .ledger
            .store_json("evaluation", study, runner::now_ms()?)?
            .digest;
        let mut report=StudyReport{schema_version:1,study:study_digest.clone(),methodology:"live-provider three-arm native verification study; benchmark decisions are not production acceptance".into(),trials:vec![],complete:false,stopped_reason:None};
        // Rotate arm order by repetition to reduce consistent provider/time ordering effects.
        'study: for repetition in 0..study.repetitions {
            for case in &study.cases {
                for offset in 0..3 {
                    let arm = ARMS[(offset + repetition as usize) % 3];
                    let mut previous: Option<(Snapshot, Snapshot)> = None;
                    let mut dependency_blocked = false;
                    for (stage_number, stage) in case.stages.iter().enumerate() {
                        if cancel.is_cancelled() {
                            report.stopped_reason = Some("study cancelled".into());
                            break 'study;
                        }
                        let run_id = format!(
                            "study-{}-{}-{arm:?}-{repetition}-{stage_number}",
                            study.study_id, case.case_id
                        );
                        let config = configure(setup(self, &stage.setup)?, study, &run_id)?;
                        let start = Instant::now();
                        // create rejects an existing run even if a previous attempt crashed before reporting.
                        self.create(&config, runner::now_ms()?)?;
                        let mut trial = Trial {
                            case_id: case.case_id.clone(),
                            repetition,
                            arm,
                            stage: stage_number,
                            run_id: run_id.clone(),
                            candidate: None,
                            predecessor: previous
                                .as_ref()
                                .map(|(_, candidate)| candidate.digest())
                                .transpose()?,
                            accepted_by_arm: false,
                            heldout_correct: None,
                            repairs: 0,
                            decision_requests: 0,
                            changed_files: 0,
                            spent: BTreeMap::new(),
                            reserved: BTreeMap::new(),
                            duration_ms: 0,
                            results: vec![],
                            heldout_results: vec![],
                            evidence_completeness: [0, 0],
                            reason: None,
                        };
                        let (active_cancel, _monitor) = crate::controller::ExecutionMonitor::start(
                            self.ledger.root(),
                            &run_id,
                            cancel.clone(),
                            config.deadline_ms,
                        )?;
                        let mut candidate = config.base.clone();
                        let mut incoming = candidate.clone();
                        let selected: Vec<_> = config
                            .plan
                            .required_final()
                            .filter(|check| {
                                arm == Arm::Enforced
                                    || stage.existing_checks.contains(&check.check_id)
                            })
                            .cloned()
                            .collect();
                        let outcome = (|| -> Result<()> {
                            require(!dependency_blocked,"previous stage did not produce an accepted proposal; dependent stage skipped")?;
                            candidate = carry(&config.base, previous.as_ref())?;
                            incoming = candidate.clone();
                            crate::snapshot::admit(
                                &self.ledger,
                                &config.base,
                                &candidate,
                                &config.policy,
                                &config.admission,
                            )?;
                            let mut feedback = vec![];
                            for attempt in 0..study.maximum_proposals {
                                require(
                                    runner::now_ms()? < config.deadline_ms,
                                    "study stage deadline exhausted",
                                )?;
                                self.select_binding(&config, &candidate)?;
                                let context = context(self, &config, &candidate, stage, arm)?;
                                let proposal = self.implement_with_context(
                                    &config,
                                    provider,
                                    &active_cancel,
                                    candidate.clone(),
                                    feedback,
                                    context,
                                )?;
                                candidate = proposal;
                                trial.candidate = Some(candidate.digest()?);
                                trial.repairs = attempt;
                                trial.results.clear();
                                trial.evidence_completeness = [0, selected.len() as u64];
                                self.select_binding(&config, &candidate)?;
                                if arm == Arm::Enforced {
                                    config.contract.require_resolved().map_err(|error| {
                                        error.with_category(crate::FailureCategory::Contract)
                                    })?;
                                    let risk = crate::risk::assess_for_languages(
                                        &self.ledger,
                                        &config.base,
                                        &candidate,
                                        &config.contract.risk_tags,
                                        &config
                                            .profiles
                                            .iter()
                                            .map(|profile| profile.language)
                                            .collect::<Vec<_>>(),
                                    )?;
                                    config.risk_policy.validate(
                                        &risk,
                                        &config.plan,
                                        &config.registry,
                                    )?;
                                }
                                let observed =
                                    checks(self, &config, &candidate, &selected, &active_cancel)?;
                                trial.results = observed
                                    .iter()
                                    .map(|result| result.result_id.clone())
                                    .collect();
                                trial.evidence_completeness = [
                                    observed
                                        .iter()
                                        .filter(|result| {
                                            matches!(result.status, Status::Pass | Status::Fail)
                                        })
                                        .count() as u64,
                                    selected.len() as u64,
                                ];
                                if truth(&observed) == Some(true) {
                                    trial.accepted_by_arm = true;
                                    break;
                                }
                                if observed.iter().any(|result| {
                                    result.status != Status::Pass && !result.repairable()
                                }) {
                                    return Err(Error::new("native infrastructure/capability failure requires operator review".into()));
                                }
                                feedback = observed
                                    .into_iter()
                                    .map(|result| context::check_feedback(&config.policy, &result))
                                    .collect::<Result<_>>()?;
                            }
                            Ok(())
                        })();
                        if let Err(error) = outcome {
                            trial.decision_requests = u32::from(matches!(
                                error.category,
                                crate::FailureCategory::Contract | crate::FailureCategory::Design
                            ));
                            trial.reason = Some(error.to_string());
                        }
                        if trial.candidate.is_some()
                            && !active_cancel.is_cancelled()
                            && runner::now_ms()? < config.deadline_ms
                        {
                            let hidden_id = format!("{run_id}-heldout");
                            let mut hidden =
                                configure(setup(self, &stage.heldout)?, study, &hidden_id)?;
                            hidden.deadline_ms = config.deadline_ms;
                            self.create(&hidden, runner::now_ms()?)?;
                            let required: Vec<_> = hidden.plan.required_final().cloned().collect();
                            match checks(self, &hidden, &candidate, &required, &active_cancel) {
                                Ok(observed) => {
                                    trial.heldout_correct = truth(&observed);
                                    trial.heldout_results = observed
                                        .into_iter()
                                        .map(|result| result.result_id)
                                        .collect();
                                }
                                Err(error) => {
                                    trial.reason = Some(format!(
                                        "{}held-out judgment: {error}",
                                        trial
                                            .reason
                                            .as_ref()
                                            .map(|reason| format!("{reason}; "))
                                            .unwrap_or_default()
                                    ));
                                }
                            }
                            for (unit, account) in self.ledger.budgets(&hidden_id)? {
                                trial.spent.insert(unit.clone(), account.spent);
                                trial.reserved.insert(unit, account.reserved);
                            }
                        }
                        for (unit, account) in self.ledger.budgets(&run_id)? {
                            *trial.spent.entry(unit.clone()).or_default() += account.spent;
                            *trial.reserved.entry(unit).or_default() += account.reserved;
                        }
                        trial.changed_files = incoming
                            .files
                            .keys()
                            .chain(candidate.files.keys())
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .filter(|path| incoming.files.get(*path) != candidate.files.get(*path))
                            .count() as u64;
                        trial.duration_ms =
                            start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                        dependency_blocked = !trial.accepted_by_arm;
                        if !dependency_blocked {
                            previous = Some((config.base.clone(), candidate));
                        }
                        let unknown = trial.reserved.values().any(|value| *value != 0);
                        let overrun = self
                            .ledger
                            .budgets(&run_id)?
                            .values()
                            .any(|account| account.spent > account.limit);
                        let artifact =
                            self.ledger
                                .store_json("evaluation", &trial, runner::now_ms()?)?;
                        self.ledger.finish_benchmark(&run_id, &artifact.digest)?;
                        let hidden_id = format!("{run_id}-heldout");
                        let hidden_exists: bool = self.ledger.connection.query_row(
                            "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1)",
                            [&hidden_id],
                            |row| row.get(0),
                        )?;
                        if hidden_exists {
                            self.ledger.finish_benchmark(&hidden_id, &artifact.digest)?;
                        }
                        report.trials.push(trial);
                        self.ledger
                            .store_json("evaluation", &report, runner::now_ms()?)?;
                        if unknown || overrun {
                            report.stopped_reason=Some("unknown or exceeded usage requires reconciliation; no further model calls were issued".into());
                            break 'study;
                        }
                    }
                }
            }
        }
        if cancel.is_cancelled() && report.stopped_reason.is_none() {
            report.stopped_reason = Some("study cancelled".into());
        }
        report.complete = report.stopped_reason.is_none();
        self.ledger
            .store_json("evaluation", &report, runner::now_ms()?)?;
        Ok(report)
    }
}
/// A binomial estimate with a 95% Wilson interval; no estimate exists with zero trials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rate {
    /// Observed numerator.
    pub count: u64,
    /// Eligible denominator, excluding unknown outcomes.
    pub total: u64,
    /// Proportion and confidence interval as decimal strings; absent for zero denominator.
    pub estimate: Option<[String; 3]>,
}
impl Rate {
    fn new(count: u64, total: u64) -> Self {
        let estimate = if total == 0 {
            None
        } else {
            let n = total as f64;
            let p = count as f64 / n;
            let z = 1.959963984540054;
            let denominator = 1.0 + z * z / n;
            let center = (p + z * z / (2.0 * n)) / denominator;
            let radius = z * ((p * (1.0 - p) + z * z / (4.0 * n)) / n).sqrt() / denominator;
            Some([
                format!("{p:.8}"),
                format!("{:.8}", (center - radius).max(0.0)),
                format!("{:.8}", (center + radius).min(1.0)),
            ])
        };
        Self {
            count,
            total,
            estimate,
        }
    }
}
/// Results remain stratified by language, workload, risk and comparison arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyMetrics {
    /// Primary language.
    pub language: Language,
    /// Workload group.
    pub workload: String,
    /// Risk group.
    pub risk_group: String,
    /// Comparison arm.
    pub arm: Arm,
    /// Complete case success means every sequential stage was accepted and passed held-out tests.
    pub success: Rate,
    /// Accepted stage proposals rejected by independent tests.
    pub escaped_defects: Rate,
    /// Correct-by-heldout stage proposals blocked by the arm's controls.
    pub heldout_false_blocks: Rate,
    /// Unknown independent judgments, kept outside known-correct/incorrect denominators.
    pub unknown_judgments: u64,
    /// Observed extra proposals, including failed trials.
    pub repairs: u64,
    /// Explicit requests for a domain/design decision.
    pub decision_requests: u64,
    /// Actual total priced model cost, including failures.
    pub cost_microunits: u64,
    /// Cost per complete successful case; absent when there were no successes.
    pub cost_per_success: Option<String>,
    /// Sum of all observed stage durations, including failures and held-out verification.
    pub duration_ms: u64,
    /// End-to-end latency spent per successful case, including failed trials.
    pub latency_per_success_ms: Option<String>,
}
impl StudyReport {
    /// Reject incomplete/competing identities before reporting full-study estimates.
    /// Wilson intervals describe this dataset; correlated extensions are grouped as one case.
    pub fn summarize(&self, study: &Study) -> Result<Vec<StudyMetrics>> {
        require(
            self.schema_version == 1 && self.study == Digest::of_json(study)? && self.complete,
            "cannot summarize a stale or incomplete study as complete",
        )?;
        let mut seen = BTreeSet::new();
        let mut groups: BTreeMap<(Language, String, String, Arm), Vec<&Trial>> = BTreeMap::new();
        for trial in &self.trials {
            let case = study
                .cases
                .iter()
                .find(|case| case.case_id == trial.case_id)
                .ok_or_else(|| Error::new("unregistered study case".into()))?;
            require(
                trial.repetition < study.repetitions
                    && trial.stage < case.stages.len()
                    && seen.insert((
                        trial.case_id.clone(),
                        trial.repetition,
                        trial.arm,
                        trial.stage,
                    )),
                "invalid or duplicate study trial",
            )?;
            require(
                trial.reserved.values().all(|n| *n == 0),
                "study still has unknown accounting",
            )?;
            groups
                .entry((
                    case.language,
                    case.workload.clone(),
                    case.risk_group.clone(),
                    trial.arm,
                ))
                .or_default()
                .push(trial);
        }
        for case in &study.cases {
            for repetition in 0..study.repetitions {
                for arm in ARMS {
                    for stage in 0..case.stages.len() {
                        require(
                            seen.contains(&(case.case_id.clone(), repetition, arm, stage)),
                            "study omitted a required arm/stage/repetition",
                        )?;
                    }
                }
            }
        }
        let mut summary = vec![];
        for ((language, workload, risk_group, arm), trials) in groups {
            let mut cases: BTreeMap<(&str, u32), bool> = BTreeMap::new();
            for trial in &trials {
                *cases
                    .entry((&trial.case_id, trial.repetition))
                    .or_insert(true) &=
                    trial.accepted_by_arm && trial.heldout_correct == Some(true);
            }
            let mut escaped = BTreeMap::<(&str, u32), (bool, bool)>::new();
            let mut blocked = BTreeMap::<(&str, u32), (bool, bool)>::new();
            for trial in &trials {
                let escape = escaped
                    .entry((&trial.case_id, trial.repetition))
                    .or_default();
                escape.0 |= trial.accepted_by_arm && trial.heldout_correct == Some(false);
                escape.1 |= trial.accepted_by_arm && trial.heldout_correct.is_some();
                let block = blocked
                    .entry((&trial.case_id, trial.repetition))
                    .or_default();
                block.0 |= !trial.accepted_by_arm && trial.heldout_correct == Some(true);
                block.1 |= trial.heldout_correct == Some(true);
            }
            let successes = cases.values().filter(|passed| **passed).count() as u64;
            let cost_microunits = trials
                .iter()
                .map(|trial| trial.spent.get("cost_microunits").copied().unwrap_or(0))
                .try_fold(0_u64, |sum, value| {
                    sum.checked_add(value)
                        .ok_or_else(|| Error::new("study total overflow".into()))
                })?;
            let duration_ms =
                trials
                    .iter()
                    .map(|trial| trial.duration_ms)
                    .try_fold(0_u64, |sum, value| {
                        sum.checked_add(value)
                            .ok_or_else(|| Error::new("study duration overflow".into()))
                    })?;
            summary.push(StudyMetrics {
                language,
                workload,
                risk_group,
                arm,
                success: Rate::new(successes, cases.len() as u64),
                escaped_defects: Rate::new(
                    escaped.values().filter(|(failed, _)| *failed).count() as u64,
                    escaped.values().filter(|(_, known)| *known).count() as u64,
                ),
                heldout_false_blocks: Rate::new(
                    blocked.values().filter(|(failed, _)| *failed).count() as u64,
                    blocked.values().filter(|(_, known)| *known).count() as u64,
                ),
                unknown_judgments: trials
                    .iter()
                    .filter(|trial| trial.heldout_correct.is_none())
                    .count() as u64,
                repairs: trials.iter().map(|trial| u64::from(trial.repairs)).sum(),
                decision_requests: trials
                    .iter()
                    .map(|trial| u64::from(trial.decision_requests))
                    .sum(),
                cost_microunits,
                cost_per_success: (successes > 0)
                    .then(|| format!("{:.8}", cost_microunits as f64 / successes as f64)),
                duration_ms,
                latency_per_success_ms: (successes > 0)
                    .then(|| format!("{:.8}", duration_ms as f64 / successes as f64)),
            });
        }
        Ok(summary)
    }
}

impl Controller {
    /// Check report decisions and billed counts against durable native receipts and accounts.
    /// Supplied summaries cannot turn worker claims or edited JSON into measured evidence.
    pub fn summarize_study(
        &self,
        study: &Study,
        report: &StudyReport,
    ) -> Result<Vec<StudyMetrics>> {
        let summary = report.summarize(study)?;
        for trial in &report.trials {
            self.ledger
                .require_benchmark_report(&trial.run_id, &Digest::of_json(trial)?)?;
            let case = study
                .cases
                .iter()
                .find(|case| case.case_id == trial.case_id)
                .ok_or_else(|| Error::new("missing study case".into()))?;
            let stage = &case.stages[trial.stage];
            require(
                trial.run_id
                    == format!(
                        "study-{}-{}-{:?}-{}-{}",
                        study.study_id, case.case_id, trial.arm, trial.repetition, trial.stage
                    ),
                "study run identity changed",
            )?;
            let actual = self.setup(&trial.run_id)?;
            let expected = setup(self, &stage.setup)?;
            let normalize = |mut config: TaskSetup| {
                config.run_id.clear();
                config.plan.run_id.clear();
                config.deadline_ms = 0;
                config.worker = study.worker.clone();
                config.budgets = study.budgets.clone();
                config.initial_candidate = None;
                config
            };
            require(
                Digest::of_json(&normalize(actual.clone()))?
                    == Digest::of_json(&normalize(expected))?
                    && Digest::of_json(&actual.worker)? == Digest::of_json(&study.worker)?
                    && actual.budgets == study.budgets
                    && actual.initial_candidate.is_none(),
                "study run has different approved inputs or model limits",
            )?;
            if let Some(candidate) = &trial.candidate {
                require(
                    self.current_candidate(&trial.run_id)?
                        .is_some_and(|snapshot| {
                            snapshot.digest().is_ok_and(|digest| digest == *candidate)
                        }),
                    "study proposal differs from the durable selected candidate",
                )?;
            }
            let mut known = vec![];
            for id in &trial.results {
                let result = self.ledger.result(&trial.run_id, id)?;
                require(
                    self.ledger
                        .completed_results(&trial.run_id, &Digest::of_json(&result.evidence_key)?)?
                        .contains(&result)
                        && trial.candidate.as_ref() == Some(&result.evidence_key.candidate_digest),
                    "study result lacks an authenticated receipt for this proposal",
                )?;
                known.push(result);
            }
            let required: BTreeSet<_> = actual
                .plan
                .required_final()
                .filter(|check| {
                    trial.arm == Arm::Enforced || stage.existing_checks.contains(&check.check_id)
                })
                .map(|check| (check.check_id.clone(), check.configuration_digest.clone()))
                .collect();
            let observed: BTreeSet<_> = known
                .iter()
                .map(|result| (result.check_id.clone(), result.configuration_digest.clone()))
                .collect();
            require(
                observed.is_subset(&required)
                    && observed.len() == known.len()
                    && trial.accepted_by_arm
                        == (observed == required && truth(&known) == Some(true)),
                "study arm decision does not match the required native observations",
            )?;
            let mut hidden_results = vec![];
            let hidden_id = format!("{}-heldout", trial.run_id);
            for id in &trial.heldout_results {
                let result = self.ledger.result(&hidden_id, id)?;
                require(
                    self.ledger
                        .completed_results(&hidden_id, &Digest::of_json(&result.evidence_key)?)?
                        .contains(&result)
                        && trial.candidate.as_ref() == Some(&result.evidence_key.candidate_digest),
                    "held-out judgment lacks authenticated evidence for the same proposal",
                )?;
                hidden_results.push(result);
            }
            if !hidden_results.is_empty() {
                let hidden = self.setup(&hidden_id)?;
                require(
                    Digest::of_json(&normalize(hidden.clone()))?
                        == Digest::of_json(&normalize(setup(self, &stage.heldout)?))?,
                    "held-out verification inputs changed",
                )?;
                require(
                    hidden
                        .plan
                        .required_final()
                        .map(|check| (check.check_id.clone(), check.configuration_digest.clone()))
                        .collect::<BTreeSet<_>>()
                        == hidden_results
                            .iter()
                            .map(|result| {
                                (result.check_id.clone(), result.configuration_digest.clone())
                            })
                            .collect::<BTreeSet<_>>(),
                    "held-out matrix is incomplete",
                )?;
            }
            require(
                trial.heldout_correct == truth(&hidden_results),
                "held-out truth was inferred without matching native outcomes",
            )?;
            let mut spent = BTreeMap::new();
            let mut reserved = BTreeMap::new();
            for id in [&trial.run_id, &hidden_id] {
                if id == &hidden_id {
                    let exists: bool = self.ledger.connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1)",
                        [id],
                        |row| row.get(0),
                    )?;
                    if !exists {
                        continue;
                    }
                }
                for (unit, account) in self.ledger.budgets(id)? {
                    *spent.entry(unit.clone()).or_insert(0_u64) += account.spent;
                    *reserved.entry(unit).or_insert(0_u64) += account.reserved;
                }
            }
            require(
                spent == trial.spent && reserved == trial.reserved,
                "study billing differs from the durable accounts",
            )?;
        }
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequential_tasks_keep_real_prior_edits_and_block_conflicting_dependency_changes() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = crate::ledger::Ledger::open(&dir.path().join("state")).unwrap();
        let base = crate::fixtures::snapshot(
            &ledger,
            "first",
            &[
                ("domain.rs", "first implementation"),
                ("dependency.rs", "v1"),
            ],
        )
        .unwrap();
        let prior = crate::fixtures::snapshot(
            &ledger,
            "first",
            &[
                ("domain.rs", "actual generated implementation"),
                ("dependency.rs", "v1"),
            ],
        )
        .unwrap();
        let next = crate::fixtures::snapshot(
            &ledger,
            "next",
            &[
                ("domain.rs", "first implementation"),
                ("dependency.rs", "v2"),
            ],
        )
        .unwrap();
        let incoming = carry(&next, Some(&(base.clone(), prior.clone()))).unwrap();
        assert_eq!(incoming.files["domain.rs"], prior.files["domain.rs"]);
        assert_eq!(incoming.files["dependency.rs"], next.files["dependency.rs"]);
        assert_eq!(incoming.base_revision, "next");
        let conflict = crate::fixtures::snapshot(
            &ledger,
            "next",
            &[
                ("domain.rs", "new dependency changes this API"),
                ("dependency.rs", "v2"),
            ],
        )
        .unwrap();
        assert!(carry(&conflict, Some(&(base, prior))).is_err());
    }
    #[test]
    fn uncertainty_keeps_zero_denominators_unknown_and_is_serialized_without_json_floats() {
        assert!(Rate::new(0, 0).estimate.is_none());
        let observed = Rate::new(5, 10);
        let [estimate, low, high] = observed.estimate.as_ref().unwrap();
        assert_eq!(estimate, "0.50000000");
        assert!(low.parse::<f64>().unwrap() < 0.5);
        assert!(high.parse::<f64>().unwrap() > 0.5);
        let bytes = wire::canonical(&observed).unwrap();
        let _: Rate = wire::decode(&bytes).unwrap();
    }
}
