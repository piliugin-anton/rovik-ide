//! Strict Autopilot owns one durable controller run and never re-enters ordinary repair.
use super::{
    driver, rows,
    stage::{Recorded, Stage, StageRecord, Status},
    Settings,
};
use crate::{
    agent_run::{StartKind, StartRequest},
    core::Core,
    harness::ide,
};
use rovik_harness::identity::Digest;
use rovik_protocol::{
    autopilot::VerifiedDeliverySettings,
    harness::{ImplementationProvider, Operation},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Mutex, OnceLock},
};

/// Task-start authority and identities, independent of later global settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StrictTask {
    /// Persisted format version.
    pub schema_version: u32,
    /// Registered owning project, not a model-supplied repository name.
    pub project_id: String,
    /// Exact canonical workspace on the owning execution host.
    pub root: String,
    /// Explicitly reviewed setup, selection and prices.
    pub configuration: VerifiedDeliverySettings,
    /// Selected IDE model profile identity; credentials are never stored.
    pub profile_id: String,
    /// Frozen model identity.
    pub model: String,
    /// Validated non-secret chat-completions endpoint.
    pub endpoint: String,
    /// Host-authenticated original source reference, if remote.
    pub baseline_reference: Option<String>,
    /// Original desktop index selection, if remote.
    pub tracked_paths: Option<Vec<String>>,
    /// Explicit host-only destination policy, frozen at task start.
    #[serde(default)]
    pub keeps_on_host: bool,
    /// Whole-task limits frozen with the requirement; strict budgets cannot reset them.
    #[serde(default)]
    pub budget: rovik_protocol::autopilot::Budget,
}

/// Stable accounting/run linkage; cancellation cannot replace it with another budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StrictRun {
    /// Controller-issued durable implementation run.
    pub run_id: String,
    /// A user stop prevents unattended restarting after reopen.
    #[serde(default)]
    pub stopped: bool,
}

fn task(records: &[Recorded]) -> Option<&StrictTask> {
    records.first()?.record.strict_task.as_ref()
}
fn linked(records: &[Recorded]) -> Option<(&Recorded, &StrictRun)> {
    records.iter().find_map(|r| {
        (r.record.name == Stage::Implement)
            .then_some(r)
            .and_then(|r| r.record.strict_run.as_ref().map(|link| (r, link)))
    })
}

/// Only a completed application or explicit stop releases strict task ownership.
pub(super) fn released(records: &[Recorded]) -> bool {
    records.last().is_some_and(|r| {
        r.record.name == Stage::Deliver && r.record.status == Status::Done
            || r.record.status == Status::Failed && r.record.outcome.as_deref() == Some("stopped")
    }) || linked(records).is_some_and(|(_, link)| link.stopped)
}

/// Stage authority survives a user row flushed before AgentStart is validated.
pub(super) async fn owned_task(
    core: &Core,
    dialog_id: &str,
) -> Result<Option<(String, Vec<Recorded>)>, String> {
    let all = rows::load(core, dialog_id).await?;
    Ok(owned_in(&all))
}

/// Find persisted strict ownership without relying on the current transcript page.
pub(super) fn owned_in(all: &[Recorded]) -> Option<(String, Vec<Recorded>)> {
    let latest = all.last().map(|r| r.record.task.as_str());
    let mut seen = BTreeSet::new();
    for row in all
        .iter()
        .rev()
        .filter(|row| row.record.strict_task.is_some())
    {
        if !seen.insert(&row.record.task) {
            continue;
        }
        let records: Vec<_> = all
            .iter()
            .filter(|r| r.record.task == row.record.task)
            .cloned()
            .collect();
        if task(&records).is_none() {
            continue;
        }
        let finished = released(&records);
        if Some(row.record.task.as_str()) == latest
            || !finished && records.iter().any(|r| r.record.name == Stage::Implement)
        {
            return Some((row.record.task.clone(), records));
        }
    }
    None
}

/// Whether the current task has frozen strict ownership, even if settings changed.
pub async fn requires_verified(core: &Core, dialog_id: &str) -> Result<bool, String> {
    Ok(owned_task(core, dialog_id)
        .await?
        .is_some_and(|(_, rows)| task(&rows).is_some()))
}

fn validate_budget(budget: &rovik_protocol::autopilot::Budget) -> Result<(), String> {
    for (name, limit, scale) in [
        ("cost", budget.max_cost, 1_000_000.0),
        ("duration", budget.max_hours, 3_600_000.0),
    ] {
        if limit.is_some_and(|value| {
            !value.is_finite() || value < 0.0 || value * scale >= u64::MAX as f64
        }) {
            return Err(format!("Review a finite nonnegative whole-task {name} limit before starting verified Autopilot."));
        }
    }
    Ok(())
}

/// Capture reviewed nonsecret authority before the first planning request.
pub(super) async fn freeze(
    core: &Core,
    request: &StartRequest,
) -> Result<Option<StrictTask>, String> {
    if !matches!(&request.start, StartKind::Send{message} if driver::starts_task(message)) {
        return Ok(None);
    }
    let config = core.agents.config_for(&request.dialog_id).autopilot;
    if !config.require_verified_delivery {
        return Ok(None);
    }
    validate_budget(&config.budget)?;
    let configuration = config.verified_delivery.ok_or("Review a protected setup and explicit model prices for this Autopilot task before starting it.")?;
    Digest::parse(&configuration.setup_id).map_err(|e| e.to_string())?;
    Digest::parse(&configuration.configuration_digest).map_err(|e| e.to_string())?;
    let root = PathBuf::from(
        request
            .root
            .as_deref()
            .ok_or("Open a project workspace before starting verified Autopilot.")?,
    )
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let dialog = request.dialog_id.clone();
    let project_id = core
        .db
        .read(move |db| crate::chat_db::store::dialog_project(db, &dialog))
        .await?
        .ok_or("Verified Autopilot needs a project-backed chat.")?;
    let selection = ImplementationProvider {
        profile: request.profile.clone(),
        rates: configuration.rates.clone(),
    };
    ide::validate_implementation_provider(&selection).map_err(|e| e.to_string())?;
    let source =
        crate::harness::ide_source::resolve(&core.paths.data_dir, &request.dialog_id, &root)
            .map_err(|e| e.to_string())?;
    if core.agents.on_server() && source.is_none() {
        return Err("Synchronize authenticated project inputs before starting verified Autopilot on a host.".into());
    }
    let (baseline_reference, tracked_paths) = source.map_or((None, None), |(reference, paths)| {
        (Some(reference), Some(paths))
    });
    let frozen = StrictTask {
        schema_version: 1,
        project_id,
        root: root.to_string_lossy().into_owned(),
        configuration,
        profile_id: request.profile.id.clone(),
        model: request.profile.model.clone(),
        endpoint: format!(
            "{}/chat/completions",
            request.profile.base_url.trim_end_matches('/')
        ),
        baseline_reference,
        tracked_paths,
        keeps_on_host: core.agents.on_server()
            && core
                .agents
                .deliverer()
                .is_some_and(|d| d.keeps_on_host(&request.dialog_id)),
        budget: config.budget,
    };
    let inspection = execute(
        core,
        &frozen,
        Operation::Inspect {
            untracked: frozen.configuration.untracked.clone(),
        },
    )
    .await?;
    if inspection["configuration"]["setupId"].as_str() != Some(&frozen.configuration.setup_id)
        || inspection["configuration"]["digest"].as_str()
            != Some(frozen.configuration.configuration_digest.as_str())
    {
        return Err(
            "The reviewed verification setup changed; review this task's setup again.".into(),
        );
    }
    Ok(Some(frozen))
}

async fn execute(core: &Core, frozen: &StrictTask, operation: Operation) -> Result<Value, String> {
    ide::execute_bound(
        core.paths.data_dir.join("harness"),
        PathBuf::from(&frozen.root),
        frozen.project_id.clone(),
        frozen.baseline_reference.clone(),
        frozen.tracked_paths.clone(),
        operation,
    )
    .await
    .map_err(|e| e.to_string())
}

fn selection(frozen: &StrictTask, settings: &Settings) -> Result<ImplementationProvider, String> {
    if settings.profile.id != frozen.profile_id
        || settings.profile.model != frozen.model
        || format!(
            "{}/chat/completions",
            settings.profile.base_url.trim_end_matches('/')
        ) != frozen.endpoint
    {
        return Err("Resume with this task's original configured provider and model; its credentials may be refreshed.".into());
    }
    Ok(ImplementationProvider {
        profile: settings.profile.clone(),
        rates: frozen.configuration.rates.clone(),
    })
}

/// Derive a missing bill only for actual usage of the exact reviewed model route.
pub(in crate::agent_run) fn priced_usage(
    frozen: &StrictTask,
    profile: &rovik_protocol::agent::ModelProfile,
    mut usage: Value,
) -> Value {
    if usage.get("cost").is_some_and(|cost| !cost.is_null())
        || profile.id != frozen.profile_id
        || profile.model != frozen.model
        || format!(
            "{}/chat/completions",
            profile.base_url.trim_end_matches('/')
        ) != frozen.endpoint
        || profile.connector.is_some()
        || profile.connector_kind().is_some()
        || profile
            .api_route
            .as_deref()
            .is_some_and(|route| route != "chat_completions")
    {
        return usage;
    }
    let Some(object) = usage.as_object_mut() else {
        return usage;
    };
    let rates = &frozen.configuration.rates;
    let missing_cache = object
        .get("prompt_tokens_details")
        .and_then(|v| v.get("cached_tokens"))
        .is_none();
    let registered = crate::harness::Rates {
        input: if missing_cache {
            rates.input.max(rates.cached_input)
        } else {
            rates.input
        },
        cached_input: rates.cached_input,
        output: rates.output,
    };
    match registered.usage(Value::Object(object.clone())) {
        Ok(actual) => {
            if let Some(cost) = actual.cost_microunits {
                object.insert("cost".into(), serde_json::json!(cost as f64 / 1_000_000.0));
                object.insert("rovikPricing".into(),serde_json::json!({"kind":"reviewed-actual-counts","profileId":frozen.profile_id,"model":frozen.model,"rates":rates,"costMicrounits":cost,"unknownCacheSplit":missing_cache}));
            }
        }
        Err(error) => {
            object.insert(
                "rovikPricing".into(),
                serde_json::json!({"kind":"unresolved","reason":error.message}),
            );
        }
    }
    usage
}

async fn context(
    core: &Core,
    dialog_id: &str,
    frozen: &StrictTask,
    settings: &Settings,
    existing: bool,
) -> Result<(), String> {
    if frozen.schema_version != 1 {
        return Err("This verified task requires a newer Rovik version.".into());
    }
    let root = PathBuf::from(
        settings
            .root
            .as_deref()
            .ok_or("Reopen this task's original execution workspace.")?,
    )
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let id = dialog_id.to_string();
    let project = core
        .db
        .read(move |db| crate::chat_db::store::dialog_project(db, &id))
        .await?;
    if root.as_path() != std::path::Path::new(&frozen.root)
        || project.as_deref() != Some(&frozen.project_id)
    {
        return Err("This verified task is bound to its original project and execution workspace. Resume there or start a new reviewed task.".into());
    }
    let current = if existing {
        crate::harness::ide_source::resolve_existing(&core.paths.data_dir, dialog_id, &root)
    } else {
        crate::harness::ide_source::resolve(&core.paths.data_dir, dialog_id, &root)
    }
    .map_err(|e| e.to_string())?;
    let expected = frozen
        .baseline_reference
        .clone()
        .zip(frozen.tracked_paths.clone());
    if current != expected {
        return Err("The task's authenticated source selection changed; review a new task.".into());
    }
    Ok(())
}

/// Whether this task must bypass ordinary implementation and delivery carriers.
pub(super) fn owns(records: &[Recorded]) -> bool {
    task(records).is_some()
}

/// Retrieve the cap captured before the task's first planning request.
pub(super) fn frozen_budget(records: &[Recorded]) -> Option<&rovik_protocol::autopilot::Budget> {
    task(records).map(|task| &task.budget)
}

/// Validate immutable prepared assets, retaining a blocked stage when unavailable.
pub(super) async fn preparation(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    stage: Stage,
    round: u32,
    settings: &Settings,
    existing: Option<String>,
) -> Result<bool, String> {
    let frozen = task(records).ok_or("Verified task binding is missing.")?;
    context(core, dialog_id, frozen, settings, false).await?;
    let first = &records[0].record;
    let record = match existing {
        Some(id) => id,
        None => {
            rows::add_stage(
                core,
                dialog_id,
                &StageRecord::running(&first.task, stage, round, first.effort),
            )
            .await?
        }
    };
    let readiness = ide::validate_autopilot_assets(ide::AutopilotReadiness {
        state_root: core.paths.data_dir.join("harness"),
        root: PathBuf::from(&frozen.root),
        project_id: frozen.project_id.clone(),
        configuration: frozen.configuration.clone(),
        baseline_reference: frozen.baseline_reference.clone(),
        tracked_paths: frozen.tracked_paths.clone(),
        check_image: stage == Stage::Prepare,
    })
    .await;
    let done = readiness.is_ok();
    let summary = match readiness {
        Ok(value) => {
            if stage == Stage::Prepare {
                "Pinned local image and prepared dependency assets are available; no installer was run.".into()
            } else {
                format!("Protected acceptance assets selected ({} cases). Native baseline/control execution and exact plan approval remain required.",value["caseCount"])
            }
        }
        Err(error) => format!("Prepared assets need review: {error}"),
    };
    rows::end_stage(core, dialog_id, &record, |r| {
        r.summary = Some(summary);
        if done {
            r.status = Status::Done;
            r.outcome = Some("prepared-assets".into());
        }
    })
    .await?;
    Ok(done)
}

type RemainingBudget = (Option<u64>, Option<u64>, Option<u64>);

fn remaining(frozen: &StrictTask, records: &[Recorded]) -> Result<RemainingBudget, String> {
    validate_budget(&frozen.budget)?;
    for record in records.iter().map(|r| &r.record) {
        if record.name == Stage::Plan
            && record.status != Status::Skipped
            && record.spent_so_far().is_none()
        {
            return Err("A modeled planning stage has no durable usage accounting; reconcile it before verified implementation.".into());
        }
        if (record.strict_accounting_open || record.strict_interrupted)
            && (frozen.budget.max_cost.is_some()
                || frozen.budget.max_tokens.is_some()
                || frozen.budget.max_hours.is_some())
        {
            return Err("An interrupted planning request has unresolved usage; a bounded task cannot treat it as free.".into());
        }
        if [record.spent, record.spending]
            .into_iter()
            .flatten()
            .any(|s| !s.cost.is_finite() || s.cost < 0.0)
        {
            return Err("Earlier stage cost accounting must be finite and nonnegative.".into());
        }
    }
    let spent = super::stage::total_spent(records.iter().map(|r| &r.record));
    let bounded = |n: f64, unit: &str| -> Result<u64, String> {
        if !n.is_finite() || n < 0.0 || n >= u64::MAX as f64 {
            Err(format!(
                "Review a finite nonnegative whole-task {unit} limit."
            ))
        } else {
            Ok(n.floor() as u64)
        }
    };
    let tokens = frozen
        .budget
        .max_tokens
        .map(|max| max.saturating_sub(spent.tokens));
    let cost = match frozen.budget.max_cost {
        Some(max) => {
            if spent.uncosted > 0 {
                return Err("Earlier task stages have unresolved provider cost; reconcile them before authorizing verified implementation.".into());
            }
            let cap = bounded(max * 1_000_000.0, "cost")?;
            let actual = records
                .iter()
                .flat_map(|r| [r.record.spent, r.record.spending])
                .flatten()
                .try_fold(0u64, |sum, spent| {
                    let charged = bounded((spent.cost * 1_000_000.0).ceil(), "settled cost")?;
                    sum.checked_add(charged)
                        .ok_or_else(|| "Earlier stage cost accounting overflowed.".to_string())
                })?;
            Some(cap.saturating_sub(actual))
        }
        None => None,
    };
    let deadline = match frozen.budget.max_hours {
        Some(max) => {
            let total = bounded(max * 3_600_000.0, "duration")?;
            Some(
                rovik_harness::runner::now_ms()
                    .map_err(|e| e.to_string())?
                    .checked_add(total.saturating_sub(spent.ms))
                    .ok_or("Task deadline overflow.")?,
            )
        }
        None => None,
    };
    Ok((tokens, cost, deadline))
}

/// Bind the implementation stage to one durable intake requiring human approvals.
pub(super) async fn start(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    round: u32,
    settings: &Settings,
    existing: Option<String>,
) -> Result<(), String> {
    let frozen = task(records)
        .ok_or("Verified task binding is missing.")?
        .clone();
    context(core, dialog_id, &frozen, settings, false).await?;
    let first = &records[0].record;
    let record = match existing {
        Some(id) => id,
        None => {
            rows::add_stage(
                core,
                dialog_id,
                &StageRecord::running(&first.task, Stage::Implement, round, first.effort),
            )
            .await?
        }
    };
    let result=async {
        let provider=selection(&frozen,settings)?;
        let plan_path=super::artifacts::task_plan_file(core,dialog_id).await?.ok_or("Save the reviewed implementation plan before continuing.")?;
        let (tokens_remaining,cost_remaining,deadline_ms)=remaining(&frozen,records)?;
        let status=ide::ensure_autopilot(ide::AutopilotIntake {state_root:core.paths.data_dir.join("harness"),root:PathBuf::from(&frozen.root),project_id:frozen.project_id.clone(),task_key:serde_json::to_string(&(dialog_id,&first.task)).map_err(|e|e.to_string())?,plan_path,configuration:frozen.configuration.clone(),provider,baseline_reference:frozen.baseline_reference.clone(),tracked_paths:frozen.tracked_paths.clone(),tokens_remaining,cost_remaining,deadline_ms}).await.map_err(|e|e.to_string())?;
        let run_id=status["runId"].as_str().ok_or("Controller returned no durable run identity.")?.to_string();
        rows::end_stage(core,dialog_id,&record,|r| {r.strict_run=Some(StrictRun {run_id,stopped:false});r.summary=Some("Review the exact plan, source, criteria and protected tests before verified implementation.".into());}).await?;
        Ok::<(),String>(())
    }.await;
    if let Err(why) = result {
        rows::end_stage(core, dialog_id, &record, |r| {
            r.summary = Some(format!("Verified implementation needs review: {why}"))
        })
        .await?;
    }
    watch(core, dialog_id, &first.task, settings);
    Ok(())
}

/// Intercept every ordinary start after strict ownership begins, including Retry.
pub(in crate::agent_run) async fn guard_start(
    core: &Core,
    request: &StartRequest,
) -> Result<bool, String> {
    let starting = core.agents.begin_start(&request.dialog_id);
    starting.check()?;
    let Some((task_id, records)) = owned_task(core, &request.dialog_id).await? else {
        return Ok(false);
    };
    starting.check()?;
    let Some(frozen) = task(&records) else {
        return Ok(false);
    };
    if matches!(&request.start,StartKind::Send{message} if message.meta.as_ref().and_then(|m|m.get("harness")).is_none() && driver::record_named(message.meta.as_ref()).is_none())
    {
        if released(&records) {
            return Ok(false);
        }
        return Err("This chat has an unfinished verified task. Review, resume or cancel its controller before starting another task.".into());
    }
    let reached = records.iter().any(|r| r.record.name == Stage::Implement);
    if !reached {
        let settings = Settings::of(request);
        context(core, &request.dialog_id, frozen, &settings, false).await?;
        starting.check()?;
        selection(frozen, &settings)?;
        if !matches!(request.start, StartKind::Send { .. }) {
            if let Some(last) = records.last().filter(|r| {
                r.record.status == Status::Running
                    && matches!(r.record.name, Stage::Prepare | Stage::Tests)
            }) {
                let settings = Settings::of(request);
                let prepared = preparation(
                    core,
                    &request.dialog_id,
                    &records,
                    last.record.name,
                    last.record.round,
                    &settings,
                    Some(last.id.clone()),
                )
                .await?;
                starting.check()?;
                if prepared {
                    advance(core, &request.dialog_id, &task_id, &settings).await?;
                }
                return Ok(true);
            }
        }
        return Ok(false);
    }
    let settings = Settings::of(request);
    context(
        core,
        &request.dialog_id,
        frozen,
        &settings,
        linked(&records).is_some(),
    )
    .await?;
    starting.check()?;
    if let Some((record, link)) = linked(&records) {
        let status = execute(
            core,
            frozen,
            Operation::Status {
                run_id: link.run_id.clone(),
            },
        )
        .await?;
        starting.check()?;
        let asked = matches!(
            request.start,
            StartKind::Retry | StartKind::ContinueRun | StartKind::ContinueReply
        );
        if status["canResume"] == true && (!link.stopped || asked) {
            execute(
                core,
                frozen,
                Operation::ResumeImplementation {
                    run_id: link.run_id.clone(),
                    provider: selection(frozen, &settings)?,
                },
            )
            .await?;
            starting.check()?;
            if link.stopped {
                rows::end_stage(core, &request.dialog_id, &record.id, |r| {
                    if let Some(link) = &mut r.strict_run {
                        link.stopped = false;
                    }
                })
                .await?;
            }
        }
        watch(core, &request.dialog_id, &task_id, &settings);
        refresh(core, &request.dialog_id, &task_id, &settings).await?;
    } else if let Some(record) = records.iter().find(|r| r.record.name == Stage::Implement) {
        start(
            core,
            &request.dialog_id,
            &records,
            record.record.round,
            &settings,
            Some(record.id.clone()),
        )
        .await?;
    }
    Ok(true)
}

fn accepted(status: &Value) -> bool {
    status["status"] == "verified"
        || status["status"] == "applied" && status["applicationReceipt"]["current"] == true
}
fn delivered(frozen: &StrictTask, status: &Value) -> bool {
    let receipt = &status["applicationReceipt"];
    accepted(status)
        && receipt["applied"] == true
        && receipt["current"] == true
        && receipt["candidateDigest"] == status["candidateDigest"]
        && receipt["candidateDigest"].as_str().is_some()
        && receipt["root"].as_str() == Some(frozen.root.as_str())
        && (receipt["deliveredLocally"] == true
            || frozen.keeps_on_host && receipt["location"] == "host")
}

/// Record post-implementation stages only from the controller's authenticated gate.
pub(super) async fn inside(
    core: &Core,
    records: &[Recorded],
    stage: Stage,
    round: u32,
) -> Result<StageRecord, String> {
    let frozen = task(records).ok_or("Verified task binding is missing.")?;
    let (_, link) = linked(records).ok_or("Verified controller linkage is missing.")?;
    let status = execute(
        core,
        frozen,
        Operation::Status {
            run_id: link.run_id.clone(),
        },
    )
    .await?;
    let first = &records[0].record;
    let mut record = StageRecord::ended(
        &first.task,
        stage,
        round,
        first.effort,
        if accepted(&status) {
            Status::Done
        } else {
            Status::Failed
        },
    );
    record.reference = Some(link.run_id.clone());
    record.outcome = Some(
        if accepted(&status) {
            "verified"
        } else {
            "verified-stale"
        }
        .into(),
    );
    record.summary=Some(if accepted(&status){match stage{Stage::Review=>"Reviewed contract and design are bound to this candidate.",Stage::Checks=>"Required native checks passed for the exact candidate.",Stage::Break=>"Protected negative-control checks validated the acceptance oracle.",_=>"The authenticated controller accepted the required behavioral and architectural evidence."}}else{"Controller evidence is stale or blocked; review the exact inputs again."}.into());
    Ok(record)
}

/// Keep delivery pending until the exact application receipt is independently current.
pub(super) async fn delivery(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    round: u32,
) -> Result<(), String> {
    let first = &records[0].record;
    let mut record = StageRecord::running(&first.task, Stage::Deliver, round, first.effort);
    record.strict_run = linked(records).map(|(_, link)| link.clone());
    record.summary =
        Some("Review and apply the exact verified candidate; it has not been delivered.".into());
    rows::add_stage(core, dialog_id, &record).await?;
    Ok(())
}

// Strict reconciliation can re-enter the driver through start_run. Erasing
// this back edge bounds both the recursive future's size and its Send proof.
fn advance<'a>(
    core: &'a Core,
    dialog_id: &'a str,
    task_id: &'a str,
    settings: &'a Settings,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(driver::advance(core, dialog_id, task_id, settings))
}

async fn refresh(
    core: &Core,
    dialog_id: &str,
    task_id: &str,
    settings: &Settings,
) -> Result<bool, String> {
    let Some((current, records)) = owned_task(core, dialog_id).await? else {
        return Ok(false);
    };
    if current != task_id {
        return Ok(false);
    }
    let Some(frozen) = task(&records) else {
        return Ok(false);
    };
    let Some((implementation, link)) = linked(&records) else {
        return Ok(false);
    };
    context(core, dialog_id, frozen, settings, true).await?;
    let status = execute(
        core,
        frozen,
        Operation::Status {
            run_id: link.run_id.clone(),
        },
    )
    .await?;
    let spent = crate::agent_run::budget::Spent {
        tokens: status["budgets"]["tokens"]["spent"].as_u64().unwrap_or(0),
        cost: status["budgets"]["cost_microunits"]["spent"]
            .as_u64()
            .unwrap_or(0) as f64
            / 1_000_000.0,
        ..Default::default()
    };
    if implementation.record.status == Status::Running {
        let done = accepted(&status) && !link.stopped;
        let cancelled = status["status"] == "cancelled";
        let summary = if done {
            "Candidate verified; awaiting exact reviewed application.".into()
        } else if link.stopped {
            "Verified task stopped; its accounting and candidate are retained.".into()
        } else {
            status["reason"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "Verified controller: {}",
                        status["status"].as_str().unwrap_or("blocked")
                    )
                })
        };
        if implementation.record.summary.as_ref() != Some(&summary)
            || implementation.record.spent != Some(spent)
            || done
        {
            rows::end_stage(core, dialog_id, &implementation.id, |r| {
                r.summary = Some(summary);
                r.spent = Some(spent);
                if cancelled {
                    if let Some(link) = &mut r.strict_run {
                        link.stopped = true;
                    }
                }
                if done {
                    r.status = Status::Done;
                    r.outcome = Some("verified".into());
                }
            })
            .await?;
        }
    }
    // A crash may land between ending Implement and recording later stages.
    // Re-enter only the strict stage mapping; it has no model or landing loop.
    if accepted(&status) && !link.stopped {
        advance(core, dialog_id, task_id, settings).await?;
    }
    let Some((_, latest)) = owned_task(core, dialog_id).await? else {
        return Ok(false);
    };
    if let Some(delivery) = latest
        .iter()
        .rev()
        .find(|r| r.record.name == Stage::Deliver)
    {
        if delivery.record.status == Status::Done {
            return Ok(false);
        }
        let ready = delivered(frozen, &status);
        let summary = if ready {
            if status["applicationReceipt"]["location"] == "host" {
                "Applied on host; the desktop workspace was not changed."
            } else {
                "Verified changes applied to the reviewed local destination."
            }
        } else if status["applicationReceipt"]["location"] == "host"
            && status["applicationReceipt"]["current"] == true
        {
            "Verified changes applied on host; the requested desktop delivery is not confirmed."
        } else if accepted(&status) {
            "Review and apply the exact verified candidate; it has not been delivered."
        } else {
            "Delivery blocked: controller evidence or the destination is no longer current."
        };
        if delivery.record.summary.as_deref() != Some(summary) || ready {
            rows::end_stage(core, dialog_id, &delivery.id, |r| {
                r.summary = Some(summary.into());
                if ready {
                    r.status = Status::Done;
                    r.outcome = Some(
                        if status["applicationReceipt"]["location"] == "host" {
                            "host-applied"
                        } else {
                            "verified-delivered"
                        }
                        .into(),
                    );
                }
            })
            .await?;
        }
        return Ok(!ready && !link.stopped);
    }
    Ok(!link.stopped)
}

fn watch(core: &Core, dialog_id: &str, task_id: &str, settings: &Settings) {
    static WATCHING: OnceLock<Mutex<BTreeSet<(PathBuf, String, String)>>> = OnceLock::new();
    let key = (
        core.paths.data_dir.clone(),
        dialog_id.to_string(),
        task_id.to_string(),
    );
    let watches = WATCHING.get_or_init(Default::default);
    if !watches
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.clone())
    {
        return;
    }
    let core = core.clone();
    let dialog = dialog_id.to_string();
    let task = task_id.to_string();
    let mut settings = settings.clone();
    settings.profile.api_key.clear();
    settings.options = None;
    let monitoring: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if core.agents.draining() {
                    break;
                }
                let Some(_guard) = driver::try_drive_lock(&dialog) else {
                    continue;
                };
                match refresh(&core, &dialog, &task, &settings).await {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        driver::stopped_by(&core, &dialog, &error);
                        break;
                    }
                }
            }
            watches
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        });
    crate::rt::spawn(monitoring);
}

/// Rearm an existing strict task after management UI opens or changes its run.
/// This is read-only reconciliation: no credentials, approvals or model calls.
pub async fn reconcile_verified(core: &Core, dialog_id: &str) {
    let result = async {
        let Some((task_id, records)) = owned_task(core, dialog_id).await? else {
            return Ok(());
        };
        let Some(frozen) = task(&records) else {
            return Ok(());
        };
        if linked(&records).is_none() {
            return Ok(());
        }
        let settings = Settings {
            profile: rovik_protocol::agent::ModelProfile {
                id: frozen.profile_id.clone(),
                label: "Verified task".into(),
                model: frozen.model.clone(),
                base_url: frozen
                    .endpoint
                    .strip_suffix("/chat/completions")
                    .ok_or("Invalid frozen model endpoint.")?
                    .into(),
                api_key: String::new(),
                stream: true,
                vision: false,
                pdf: false,
                context_tokens: None,
                no_tools: false,
                first_byte_timeout_ms: None,
                api_route: None,
                connector: None,
            },
            options: None,
            context_mode: None,
            root: Some(frozen.root.clone()),
            local_date: None,
            isolate: None,
        };
        watch(core, dialog_id, &task_id, &settings);
        if let Some(_guard) = driver::try_drive_lock(dialog_id) {
            refresh(core, dialog_id, &task_id, &settings).await?;
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(error) = result {
        driver::stopped_by(core, dialog_id, &error);
    }
}

/// Stop only the existing controller; no acceptance or application authority changes.
pub(in crate::agent_run) async fn stop(core: &Core, dialog_id: &str) -> bool {
    // Ordinary starts can wait for a driver without owning a strict task. Do
    // not make their Stop wait behind that driver just to discover no owner.
    let Ok(Some((_, records))) = owned_task(core, dialog_id).await else {
        return false;
    };
    if task(&records).is_none() {
        return false;
    }
    let _guard = driver::drive_lock(dialog_id).await;
    if core.agents.stop(core, dialog_id) {
        return true;
    }
    let Ok(Some((_, records))) = owned_task(core, dialog_id).await else {
        return false;
    };
    let Some(frozen) = task(&records) else {
        return false;
    };
    let Some((record, link)) = linked(&records) else {
        if let Some(last) = records.last() {
            if let Err(error) = rows::end_stage(core, dialog_id, &last.id, |r| {
                r.status = Status::Failed;
                r.outcome = Some("stopped".into());
                r.summary = Some("Verified task stopped before controller implementation.".into());
            })
            .await
            {
                driver::stopped_by(core, dialog_id, &error);
            }
            return true;
        }
        return false;
    };
    let stopped = async {
        let status = execute(
            core,
            frozen,
            Operation::Status {
                run_id: link.run_id.clone(),
            },
        )
        .await?;
        if status["canCancel"] == true {
            execute(
                core,
                frozen,
                Operation::Cancel {
                    run_id: link.run_id.clone(),
                },
            )
            .await?;
        }
        rows::end_stage(core, dialog_id, &record.id, |r| {
            if let Some(link) = &mut r.strict_run {
                link.stopped = true;
            }
            r.summary =
                Some("Verified task stopped; its accounting and candidate are retained.".into());
        })
        .await?;
        Ok::<(), String>(())
    }
    .await;
    if let Err(error) = stopped {
        driver::stopped_by(core, dialog_id, &error);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent_run::{AgentConfig, UserMessage},
        chat_db::{projects, store},
        testing::{Reply, TestCore},
    };
    use rovik_protocol::{
        agent::ModelProfile,
        autopilot::{Budget, Effort},
        harness::ModelRates,
    };
    use serde_json::json;

    fn frozen(root: &str) -> StrictTask {
        StrictTask {
            schema_version: 1,
            project_id: "project".into(),
            root: root.into(),
            configuration: VerifiedDeliverySettings {
                setup_id: Digest::of(b"setup").to_string(),
                configuration_digest: Digest::of(b"config").to_string(),
                untracked: vec![],
                rates: ModelRates {
                    input: 0,
                    cached_input: 0,
                    output: 0,
                },
            },
            profile_id: "fixture".into(),
            model: "fixture".into(),
            endpoint: "http://localhost:8000/v1/chat/completions".into(),
            baseline_reference: None,
            tracked_paths: None,
            keeps_on_host: false,
            budget: Budget::default(),
        }
    }

    fn record(frozen: StrictTask) -> Vec<Recorded> {
        let mut plan = StageRecord::ended("task", Stage::Plan, 1, Effort::Thorough, Status::Done);
        plan.strict_task = Some(frozen);
        vec![Recorded {
            id: "plan".into(),
            seq: 1,
            record: plan,
        }]
    }

    #[test]
    fn accepted_candidates_need_a_current_matching_application_receipt_for_delivery() {
        let mut task = frozen("/workspace/project");
        let mut status = json!({"status":"verified","candidateDigest":"candidate"});
        assert!(!delivered(&task, &status));
        status["applicationReceipt"] = json!({"applied":true,"current":true,"candidateDigest":"candidate","root":task.root,"location":"host","deliveredLocally":false});
        assert!(
            !delivered(&task, &status),
            "host-only output is not desktop delivery"
        );
        task.keeps_on_host = true;
        assert!(delivered(&task, &status));
        for (field, value) in [
            ("current", json!(false)),
            ("candidateDigest", json!("other")),
            ("root", json!("/other")),
        ] {
            let mut changed = status.clone();
            changed["applicationReceipt"][field] = value;
            assert!(!delivered(&task, &changed), "{field}");
        }
        status["status"] = json!("stale");
        assert!(!delivered(&task, &status));
        status["status"] = json!("applied");
        task.keeps_on_host = false;
        status["applicationReceipt"]["location"] = json!("local");
        status["applicationReceipt"]["deliveredLocally"] = json!(true);
        assert!(delivered(&task, &status));
    }

    #[test]
    fn strict_implementation_receives_only_the_remaining_frozen_task_budget() {
        let mut frozen = frozen("/project");
        frozen.budget = Budget {
            max_hours: None,
            max_cost: Some(2.0),
            max_tokens: Some(1000),
        };
        let mut rows = record(frozen.clone());
        rows[0].record.spent = Some(crate::agent_run::budget::Spent {
            cost: 0.25,
            tokens: 300,
            ..Default::default()
        });
        assert_eq!(
            remaining(&frozen, &rows).unwrap(),
            (Some(700), Some(1_750_000), None)
        );
        rows[0].record.spent.as_mut().unwrap().uncosted = 1;
        assert!(remaining(&frozen, &rows)
            .unwrap_err()
            .contains("unresolved provider cost"));
    }

    #[test]
    fn missing_or_invalid_planning_accounting_cannot_expand_a_strict_budget() {
        let mut frozen = frozen("/project");
        frozen.budget.max_cost = Some(1.0);
        let mut rows = record(frozen.clone());
        assert!(remaining(&frozen, &rows)
            .unwrap_err()
            .contains("no durable usage"));
        rows[0].record.spent = Some(Default::default());
        for invalid in [f64::NAN, f64::INFINITY, -1.0, f64::MAX] {
            frozen.budget.max_cost = Some(invalid);
            assert!(remaining(&frozen, &rows).is_err(), "limit {invalid}");
        }
        frozen.budget.max_cost = Some(1.0);
        for invalid in [f64::NAN, f64::INFINITY, -1.0, f64::MAX] {
            rows[0].record.spent.as_mut().unwrap().cost = invalid;
            assert!(remaining(&frozen, &rows).is_err(), "spent {invalid}");
        }
    }

    #[test]
    fn reviewed_prices_cover_only_actual_counts_from_the_exact_planning_model() {
        let mut frozen = frozen("/project");
        frozen.configuration.rates = ModelRates {
            input: 2_000_000,
            cached_input: 1_000_000,
            output: 3_000_000,
        };
        let mut profile:ModelProfile=serde_json::from_value(json!({"id":"fixture","label":"Fixture","model":"fixture","baseUrl":"http://localhost:8000/v1","apiKey":"private-key"})).unwrap();
        let counts = json!({"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":50}});
        let priced = priced_usage(&frozen, &profile, counts.clone());
        assert_eq!(priced["rovikPricing"]["costMicrounits"], 210);
        assert_eq!(priced["cost"], json!(0.00021));
        assert!(!priced.to_string().contains("private-key"));
        profile.model = "other-reviewer".into();
        assert!(priced_usage(&frozen, &profile, counts.clone())
            .get("cost")
            .is_none());
        profile.model = frozen.model.clone();
        profile.base_url = "https://another-provider.example/v1".into();
        assert!(priced_usage(&frozen, &profile, counts.clone())
            .get("cost")
            .is_none());
        profile.base_url = "http://localhost:8000/v1".into();
        profile.id = "different-profile".into();
        assert!(priced_usage(&frozen, &profile, counts.clone())
            .get("cost")
            .is_none());
        profile.id = frozen.profile_id.clone();
        for incomplete in [
            json!({"prompt_tokens":100}),
            json!({"completion_tokens":20}),
            json!({"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":101}}),
            json!({"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":-1}}),
        ] {
            let result = priced_usage(&frozen, &profile, incomplete);
            assert!(result.get("cost").is_none());
            assert_eq!(result["rovikPricing"]["kind"], "unresolved");
        }
        frozen.configuration.rates.cached_input = 4_000_000;
        let unknown_split = priced_usage(
            &frozen,
            &profile,
            json!({"prompt_tokens":100,"completion_tokens":20}),
        );
        assert_eq!(unknown_split["rovikPricing"]["costMicrounits"], 460);
        assert_eq!(unknown_split["rovikPricing"]["unknownCacheSplit"], true);
        let actual = priced_usage(
            &frozen,
            &profile,
            json!({"prompt_tokens":100,"completion_tokens":20,"cost":0.5}),
        );
        assert_eq!(actual["cost"], 0.5);
        assert!(actual.get("rovikPricing").is_none());
    }

    #[test]
    fn planning_receipts_use_reviewed_rates_and_keep_other_model_usage_unresolved() {
        let state = crate::agent_run::AgentState::default();
        let lead = format!("priced-{}", uuid::Uuid::new_v4());
        let child = format!("child-{}", uuid::Uuid::new_v4());
        state.register(&lead, "fixture", None).unwrap();
        state.set_autopilot(&lead, Effort::Balanced, Default::default());
        let mut frozen = frozen("/project");
        frozen.configuration.rates = ModelRates {
            input: 1_000_000,
            cached_input: 1_000_000,
            output: 2_000_000,
        };
        state.set_strict_task(&lead, frozen.clone());
        state
            .register_child(&child, &lead, "review", "fixture", None)
            .unwrap();
        let mut profile =
            request(&frozen, &lead, "http://localhost:8000/v1", StartKind::Retry).profile;
        let usage = state.price_usage(
            &child,
            &profile,
            Some(json!({"prompt_tokens":100,"completion_tokens":20})),
        );
        state.note_reply(&child, usage.as_ref(), false);
        let spent = state.run_spent(&lead).unwrap();
        assert_eq!(spent.uncosted, 0);
        assert_eq!(spent.cost, 0.00014);
        profile.model = "unpriced-reviewer".into();
        let usage = state.price_usage(
            &child,
            &profile,
            Some(json!({"prompt_tokens":100,"completion_tokens":20})),
        );
        state.note_reply(&child, usage.as_ref(), false);
        assert_eq!(state.run_spent(&lead).unwrap().uncosted, 1);
        assert_eq!(state.run_spent(&lead).unwrap().cost, 0.00014);
    }

    async fn stored(stage: Stage, link: Option<StrictRun>) -> (TestCore, StrictTask, String) {
        let test = TestCore::new();
        let root = test.dir.0.join("project");
        git2::Repository::init(&root).unwrap();
        let opened_root = root.clone();
        let project = test
            .core
            .db
            .write(move |db| projects::open_project(db, &opened_root))
            .await
            .unwrap()
            .project
            .id;
        let dialog = uuid::Uuid::new_v4().to_string();
        let d = store::NewDialog {
            id: dialog.clone(),
            title: "strict lifecycle".into(),
            created_at: 0,
            updated_at: 0,
            project_id: Some(project.clone()),
        };
        test.core
            .db
            .write(move |db| store::create_dialog(db, &d))
            .await
            .unwrap();
        let mut frozen = frozen(root.canonicalize().unwrap().to_str().unwrap());
        frozen.project_id = project;
        let plan = record(frozen.clone()).remove(0).record;
        rows::add_stage(&test.core, &dialog, &plan).await.unwrap();
        let mut running = StageRecord::running("task", stage, 1, Effort::Thorough);
        running.strict_run = link;
        rows::add_stage(&test.core, &dialog, &running)
            .await
            .unwrap();
        (test, frozen, dialog)
    }

    fn request(task: &StrictTask, dialog: &str, url: &str, start: StartKind) -> StartRequest {
        let profile:ModelProfile=serde_json::from_value(json!({"id":"fixture","label":"Fixture","model":"fixture","baseUrl":url,"apiKey":"ephemeral-only"})).unwrap();
        StartRequest {
            dialog_id: dialog.into(),
            start,
            profile,
            options: None,
            mode: Some("agent".into()),
            context_mode: None,
            root: Some(task.root.clone()),
            current_user_content: None,
            local_date: None,
            config: Some(AgentConfig::default()),
            isolate: Some(false),
            project_settings: None,
            mcp_secrets: Default::default(),
        }
    }

    #[tokio::test]
    async fn stopping_an_accepted_resume_prevents_strict_recovery_from_reviving_its_stage() {
        let (test, frozen, dialog) = stored(Stage::Prepare, None).await;
        // Routing accepted Resume before it got as far as the task driver.
        let accepted = test.core.agents.begin_start(&dialog);
        assert!(crate::agent_run::agent_stop(&test.core, dialog.clone()).await);
        let resume = request(
            &frozen,
            &dialog,
            "http://localhost:8000/v1",
            StartKind::Resume,
        );
        let error = driver::go_on(&test.core, &resume).await.unwrap_err();
        assert!(error.contains("start was stopped"), "{error}");
        let retry = request(
            &frozen,
            &dialog,
            "http://localhost:8000/v1",
            StartKind::Retry,
        );
        let error = guard_start(&test.core, &retry).await.unwrap_err();
        assert!(error.contains("start was stopped"), "{error}");
        let (_, rows) = owned_task(&test.core, &dialog).await.unwrap().unwrap();
        assert_eq!(rows.len(), 2, "cancellation did not add another stage");
        let last = &rows.last().unwrap().record;
        assert_eq!(last.status, Status::Failed);
        assert_eq!(last.outcome.as_deref(), Some("stopped"));
        drop(accepted);
        assert!(test.core.agents.begin_start(&dialog).check().is_ok());
    }

    #[tokio::test]
    async fn persisted_strict_ownership_survives_disabled_settings_and_all_ordinary_resume_kinds() {
        let (test, frozen, dialog) = stored(
            Stage::Implement,
            Some(StrictRun {
                run_id: "missing-but-still-owned".into(),
                stopped: false,
            }),
        )
        .await;
        let (url, seen) = crate::testing::model(vec![Reply::Text("must not run")]).await;
        assert!(requires_verified(&test.core, &dialog).await.unwrap());
        for kind in [
            StartKind::Resume,
            StartKind::Retry,
            StartKind::ContinueRun,
            StartKind::ContinueReply,
        ] {
            let error =
                crate::agent_run::agent_start(&test.core, request(&frozen, &dialog, &url, kind))
                    .await
                    .unwrap_err();
            assert!(
                error.contains("does not belong") || error.contains("unavailable"),
                "{error}"
            );
        }
        assert!(seen.lock().unwrap().is_empty());
        assert!(!test.core.agents.is_running(&dialog));
        let (_, rows) = rows::current_task(&test.core, &dialog)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(linked(&rows).unwrap().1.run_id, "missing-but-still-owned");
    }

    #[tokio::test]
    async fn a_flushed_user_row_cannot_hide_an_unfinished_strict_owner_or_its_strip() {
        let (test, frozen, dialog) = stored(
            Stage::Implement,
            Some(StrictRun {
                run_id: "retained".into(),
                stopped: false,
            }),
        )
        .await;
        let (url, seen) = crate::testing::model(vec![Reply::Text("must not run")]).await;
        let message = UserMessage {
            id: "later-flushed-user-row".into(),
            content: "Ordinary agent work".into(),
            meta: Some(json!({"mode":"agent"})),
            attachments: vec![],
        };
        rows::append(
            &test.core,
            &dialog,
            crate::agent_run::stored_message(&message),
        )
        .await
        .unwrap();
        assert!(requires_verified(&test.core, &dialog).await.unwrap());
        assert!(rows::task_stages(&test.core, &dialog)
            .await
            .unwrap()
            .is_some());
        let error = crate::agent_run::agent_start(
            &test.core,
            request(&frozen, &dialog, &url, StartKind::Send { message }),
        )
        .await
        .unwrap_err();
        assert!(error.contains("unfinished verified task"));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn interrupted_planning_preserves_unknown_usage_before_its_first_reply() {
        let (test, mut frozen, dialog) = stored(Stage::Plan, None).await;
        let (_, records) = owned_task(&test.core, &dialog).await.unwrap().unwrap();
        rows::end_stage(&test.core, &dialog, &records[0].id, |r| {
            r.spent = Some(Default::default())
        })
        .await
        .unwrap();
        let plan = &records.last().unwrap().id;
        rows::keep_cut_spending(&test.core, &dialog, plan)
            .await
            .unwrap();
        let begun = rows::record_in(&test.core, &dialog, plan).await.unwrap();
        assert!(begun.strict_accounting_open);
        // Re-entering before settle is an interrupted external attempt, even
        // when no response checkpoint reached the chat database.
        rows::keep_cut_spending(&test.core, &dialog, plan)
            .await
            .unwrap();
        rows::settle_spent(&test.core, &dialog, plan, Default::default())
            .await
            .unwrap();
        let (_, records) = owned_task(&test.core, &dialog).await.unwrap().unwrap();
        assert!(records.last().unwrap().record.strict_interrupted);
        assert_eq!(records.last().unwrap().record.spent.unwrap().uncosted, 1);
        frozen.budget.max_cost = Some(1.0);
        assert!(remaining(&frozen, &records)
            .unwrap_err()
            .contains("interrupted planning request"));
    }

    #[tokio::test]
    async fn missing_prepared_assets_pause_both_readiness_stages_without_ordinary_agents() {
        let (url, seen) =
            crate::testing::model(vec![Reply::Text("must not install or write tests")]).await;
        for stage in [Stage::Prepare, Stage::Tests] {
            let (test, mut frozen, dialog) = stored(stage, None).await;
            frozen.endpoint = format!("{url}/chat/completions");
            let (_, records) = owned_task(&test.core, &dialog).await.unwrap().unwrap();
            rows::end_stage(&test.core, &dialog, &records[0].id, |r| {
                r.strict_task = Some(frozen.clone())
            })
            .await
            .unwrap();
            assert_eq!(
                crate::agent_run::agent_start(
                    &test.core,
                    request(&frozen, &dialog, &url, StartKind::Retry)
                )
                .await
                .unwrap(),
                None
            );
            let (_, rows) = rows::current_task(&test.core, &dialog)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(rows.last().unwrap().record.status, Status::Running);
            assert!(rows
                .last()
                .unwrap()
                .record
                .summary
                .as_ref()
                .unwrap()
                .contains("Select the prepared verification setup"));
            assert!(!test.core.agents.is_running(&dialog));
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_fresh_ordinary_message_is_released_after_explicit_stop_but_retry_stays_pinned() {
        let (test, frozen, dialog) = stored(
            Stage::Implement,
            Some(StrictRun {
                run_id: "retained".into(),
                stopped: true,
            }),
        )
        .await;
        let send = StartKind::Send {
            message: UserMessage {
                id: "fresh-user-message".into(),
                content: "Discuss another topic".into(),
                meta: Some(json!({"mode":"agent"})),
                attachments: vec![],
            },
        };
        assert!(!guard_start(
            &test.core,
            &request(&frozen, &dialog, "http://localhost:8000/v1", send)
        )
        .await
        .unwrap());
        assert!(guard_start(
            &test.core,
            &request(
                &frozen,
                &dialog,
                "http://localhost:8000/v1",
                StartKind::Retry
            )
        )
        .await
        .is_err());
    }
}
