//! Budgeted drafting and independent review, with no model approval authority.

use crate::{
    broker::FileEdit,
    context::{self, ContextItem, Trust},
    contract::{ContractRegistry, DesignRecord, TaskContract},
    controller::WorkerLimits,
    identity::Digest,
    ledger::{Ledger, RunRecord},
    provider::{self, ModelRequest, Provider, Role},
    require,
    runner::{self, Cancellation},
    snapshot::Snapshot,
    wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// An unapproved intake can start drafting without a complete TaskSetup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    /// Exactly 1.
    pub schema_version: u32,
    /// Durable preparation/run identity.
    pub run_id: String,
    /// Registered repository identity.
    pub repository_id: String,
    /// User's requested outcome; untrusted with respect to controller capabilities.
    pub request: String,
    /// Immutable explicitly selected input, including any approved dirty files.
    pub base: Snapshot,
    /// Trusted profile, suite, constraint and risk vocabulary.
    pub registry: ContractRegistry,
    /// Registered local authority for cancellation, recovery and accounting before promotion.
    pub authority: crate::approvals::AuthorityPolicy,
    /// Bounded model/context settings, shared by all preparation roles.
    pub worker: WorkerLimits,
    /// Total preparation budget; promotion retains this accounting.
    pub budgets: BTreeMap<String, u64>,
    /// Absolute deadline for drafting.
    pub deadline_ms: u64,
}
impl Preparation {
    /// Persist an intake before any charged model work.
    pub fn create(&self, ledger: &mut Ledger) -> Result<RunRecord> {
        require(
            self.schema_version == 1 && !self.request.trim().is_empty(),
            "invalid preparation intake",
        )?;
        crate::id(&self.run_id)?;
        crate::id(&self.repository_id)?;
        self.base.validate(ledger, true)?;
        self.worker.validate()?;
        require(
            self.deadline_ms > runner::now_ms()?,
            "preparation deadline exhausted",
        )?;
        for unit in ["model_calls", "tokens", "cost_microunits", "checks"] {
            require(
                self.budgets.contains_key(unit),
                format!("preparation lacks budget: {unit}"),
            )?;
        }
        let input = ledger.store_json("preparation", self, runner::now_ms()?)?;
        ledger.create_run(
            &self.run_id,
            &input.digest,
            &self.budgets,
            runner::now_ms()?,
        )
    }
}

/// A finding is evidence for human review, not a native pass/fail or approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFinding {
    /// Stable finding ID.
    pub id: String,
    /// Whether this finding prevents a ready recommendation.
    pub blocking: bool,
    /// Concrete issue and affected behavior.
    pub message: String,
    /// Exact artifacts inspected by the reviewer.
    pub evidence: Vec<Digest>,
}
/// Advisory model conclusion, independent of authenticated approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    /// No blocking issue observed within the supplied evidence.
    Ready,
    /// Implementation changes are needed.
    ChangesRequired,
    /// Domain, design or scope needs an authorized decision.
    NeedsDecision,
}
/// Closed output types prevent a drafting role from issuing edits or approvals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DraftArtifact {
    /// Domain analyst output.
    Contract {
        /// Proposed behavioral contract, possibly with blocking unknowns.
        contract: TaskContract,
    },
    /// Architect output.
    Design {
        /// Proposed responsibility record.
        design: DesignRecord,
    },
    /// Independent oracle-author output; files are never installed automatically.
    Oracle {
        /// Protected acceptance assets proposed for review.
        files: Vec<FileEdit>,
        /// Exact suite/case inventory.
        suites: BTreeMap<String, BTreeSet<String>>,
        /// Cases which must reject a relevant independently selected control.
        negative_control_cases: BTreeSet<String>,
    },
    /// Independent reviewer output.
    Review {
        /// Exact immutable candidate reviewed.
        candidate_digest: Digest,
        /// Attributed semantic findings.
        findings: Vec<ReviewFinding>,
        /// Unresolved business/design questions.
        questions: Vec<String>,
        /// Advisory disposition.
        recommendation: Recommendation,
    },
}
/// One correlated role response, retained alongside provider accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftResponse {
    /// Exactly 1.
    pub schema_version: u32,
    /// Exact active provider request.
    pub request_id: String,
    /// Role-specific result, never a command capability.
    pub artifact: DraftArtifact,
}

/// Inputs and provenance for one role invocation.
pub struct RoleInput<'a> {
    /// Durable run which pays for every attempt.
    pub run_id: &'a str,
    /// Strict output role.
    pub role: Role,
    /// Source context bound into the output.
    pub snapshot: &'a Snapshot,
    /// Trusted semantic vocabulary.
    pub registry: &'a ContractRegistry,
    /// Label to exact input artifacts (request, contract, design, native evidence).
    pub artifacts: &'a BTreeMap<String, Digest>,
    /// Bounded generation settings.
    pub limits: &'a WorkerLimits,
    /// Absolute deadline.
    pub deadline_ms: u64,
}

fn contract(input: &RoleInput<'_>, ledger: &Ledger) -> Result<TaskContract> {
    let digest = input
        .artifacts
        .get("contract")
        .ok_or_else(|| Error::new("role requires a supplied contract artifact".into()))?;
    wire::decode(&ledger.read(digest)?)
}
fn validate(
    output: &DraftResponse,
    request: &ModelRequest,
    input: &RoleInput<'_>,
    ledger: &Ledger,
) -> Result<()> {
    require(
        output.schema_version == 1 && output.request_id == request.request_id,
        "role response correlation mismatch",
    )?;
    match (&output.artifact, input.role) {
        (DraftArtifact::Contract { contract }, Role::DomainAnalyst) => {
            contract.validate(input.registry)
        }
        (DraftArtifact::Design { design }, Role::Architect) => {
            let mut shape = design.clone();
            // A draft preserves its unknowns; promotion validates the original record.
            for unknown in &mut shape.unknowns {
                unknown.blocking = false;
            }
            shape.validate(&contract(input, ledger)?)?;
            crate::design::validate(ledger, input.snapshot, design)
        }
        (
            DraftArtifact::Oracle {
                files,
                suites,
                negative_control_cases,
            },
            Role::OracleAuthor,
        ) => {
            require(
                !files.is_empty() && files.len() <= 128,
                "oracle draft has no assets or exceeds its limit",
            )?;
            crate::unique(files.iter().map(|f| f.path.as_str()))?;
            for file in files {
                crate::scope::validate_path(&file.path)?;
                require(
                    file.content.as_ref().is_some_and(|v| !v.is_empty())
                        && [0o100644, 0o100755].contains(&file.mode),
                    "invalid oracle asset",
                )?;
            }
            let contract = contract(input, ledger)?;
            let mut registry = input.registry.clone();
            registry.suites = suites.clone();
            contract.validate(&registry)?;
            let cases: BTreeSet<_> = contract
                .invariants
                .iter()
                .flat_map(|i| i.case_ids.iter().cloned())
                .collect();
            require(
                !negative_control_cases.is_empty() && negative_control_cases.is_subset(&cases),
                "oracle needs a relevant negative-control inventory",
            )
        }
        (
            DraftArtifact::Review {
                candidate_digest,
                findings,
                questions,
                recommendation,
            },
            Role::Reviewer,
        ) => {
            require(
                candidate_digest == &input.snapshot.digest()?,
                "review names a different candidate",
            )?;
            require(
                findings.len() <= 128 && questions.len() <= 32,
                "review exceeds finding/question limits",
            )?;
            crate::unique(findings.iter().map(|f| f.id.as_str()))?;
            for finding in findings {
                require(
                    !finding.message.trim().is_empty()
                        && finding.message.len() <= 8192
                        && !finding.evidence.is_empty(),
                    "review finding needs a bounded message and evidence",
                )?;
                for digest in &finding.evidence {
                    ledger.read(digest)?;
                }
            }
            require(
                questions
                    .iter()
                    .all(|q| !q.trim().is_empty() && q.len() <= 4096),
                "invalid review question",
            )?;
            require(
                *recommendation != Recommendation::Ready
                    || (!findings.iter().any(|f| f.blocking) && questions.is_empty()),
                "ready review contains unresolved blocking findings",
            )
        }
        _ => Err(Error::new(
            "role attempted an unauthorized response type".into(),
        )),
    }
}

/// Generate one validated draft, with at most the configured charged format repair.
/// No role receives write, shell, approval or policy-registration capabilities.
pub fn generate(
    ledger: &mut Ledger,
    input: &RoleInput<'_>,
    provider: &mut dyn Provider,
    cancel: &Cancellation,
) -> Result<DraftResponse> {
    require(
        input.role != Role::Implementer,
        "implementer must use the scoped broker",
    )?;
    input.limits.validate()?;
    input.snapshot.validate(ledger, true)?;
    let policy = input
        .artifacts
        .get("policy")
        .map(|digest| wire::decode::<crate::policy::EffectivePolicy>(&ledger.read(digest)?))
        .transpose()?;
    let deny_read = policy
        .as_ref()
        .map_or(&[][..], |policy| policy.deny_read.as_slice());
    crate::policy::validate_read_patterns(deny_read)?;
    let (active_cancel, _monitor) = crate::controller::ExecutionMonitor::start(
        ledger.root(),
        input.run_id,
        cancel.clone(),
        input.deadline_ms,
    )?;
    let cancel = &active_cancel;
    let revision = input.snapshot.digest()?;
    let now = runner::now_ms()?;
    let control = ledger.store(
        "context",
        "text/plain",
        input.role.control().as_bytes(),
        now,
    )?;
    let mut items = vec![ContextItem {
        item_id: "role-control".into(),
        artifact_digest: control.digest,
        trust: Trust::AuthoritativePolicy,
        source_revision: revision.clone(),
        mandatory: true,
        text: input.role.control().into(),
    }];
    let registry = ledger.store_json("context", input.registry, now)?;
    let mut sources = input.artifacts.clone();
    require(
        !sources.contains_key("role-registry"),
        "reserved role context label",
    )?;
    sources.insert("role-registry".into(), registry.digest);
    for (label, digest) in &sources {
        let text = String::from_utf8(ledger.read(digest)?)
            .map_err(|_| Error::new("role input is not UTF-8".into()))?;
        items.push(ContextItem {
            item_id: label.clone(),
            artifact_digest: digest.clone(),
            trust: Trust::UntrustedText,
            source_revision: revision.clone(),
            mandatory: true,
            text,
        });
    }
    let query = items
        .iter()
        .filter(|item| item.item_id != "role-control")
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    items.extend(context::ranked_sources(
        ledger,
        input.snapshot,
        &query,
        &[],
        None,
        deny_read,
    )?);
    let context = context::build(
        ledger,
        items,
        input.limits.context_bytes.saturating_sub(4096),
    )?;
    let mut feedback = vec![];
    for attempt in 0..=input.limits.format_repairs {
        require(
            runner::now_ms()? < input.deadline_ms,
            "role deadline exhausted",
        )?;
        let request = ModelRequest {
            role: input.role,
            request_id: uuid::Uuid::new_v4().to_string(),
            provider_model_id: input.limits.model_id.clone(),
            context: context.clone(),
            snapshot_digest: revision.clone(),
            maximum_output_tokens: input.limits.output_tokens,
            timeout_ms: input
                .limits
                .timeout_ms
                .min(input.deadline_ms.saturating_sub(runner::now_ms()?)),
            feedback,
        };
        require(
            wire::canonical(&request)?.len() <= input.limits.context_bytes,
            "complete role request exceeds context budget",
        )?;
        let response = provider::accounted_call(
            ledger,
            input.run_id,
            &request,
            provider::Reservation {
                tokens: input.limits.token_reservation,
                cost_microunits: input.limits.cost_reservation,
            },
            provider,
            cancel,
        )?;
        let draft = wire::decode::<DraftResponse>(response.output.as_bytes()).and_then(|output| {
            validate(&output, &request, input, ledger)?;
            Ok(output)
        });
        match draft {
            Ok(output) => {
                ledger.store_json("role-draft", &output, runner::now_ms()?)?;
                match &output.artifact {
                    DraftArtifact::Contract { contract } => {
                        ledger.store_json("contract", contract, runner::now_ms()?)?;
                    }
                    DraftArtifact::Design { design } => {
                        ledger.store_json("design", design, runner::now_ms()?)?;
                    }
                    _ => {}
                }
                return Ok(output);
            }
            Err(error) if attempt < input.limits.format_repairs => {
                feedback = vec![
                    serde_json::json!({"format_error":error.message,"instruction":"Return only the required role schema; do not invent missing semantic facts."}),
                ];
            }
            Err(error) => return Err(error),
        }
    }
    Err(Error::new("role exhausted format repair attempts".into()))
}

/// Reuse a completed validated review/draft only for the same inputs, role and model.
/// Failed attempts and accounting remain visible; this never synthesizes approval.
pub fn completed(ledger: &Ledger, input: &RoleInput<'_>) -> Result<Option<DraftResponse>> {
    for operation in ledger
        .operations(input.run_id)?
        .into_iter()
        .rev()
        .filter(|o| o.kind == "model" && o.state == "completed")
    {
        let request: ModelRequest = wire::decode(&ledger.read(&operation.input_digest)?)?;
        if request.role != input.role
            || request.snapshot_digest != input.snapshot.digest()?
            || request.provider_model_id != input.limits.model_id
            || request.maximum_output_tokens != input.limits.output_tokens
            || !input.artifacts.iter().all(|(label, digest)| {
                request
                    .context
                    .items
                    .iter()
                    .any(|i| i.item_id == *label && i.artifact_digest == *digest)
            })
            || !request
                .context
                .items
                .iter()
                .any(|i| i.item_id == "role-control" && i.text == input.role.control())
        {
            continue;
        }
        let Some(digest) = operation.output_digest else {
            continue;
        };
        let response: provider::ModelResponse = wire::decode(&ledger.read(&digest)?)?;
        if let Ok(output) = wire::decode::<DraftResponse>(response.output.as_bytes()) {
            if validate(&output, &request, input, ledger).is_ok() {
                return Ok(Some(output));
            }
        }
    }
    Ok(None)
}
