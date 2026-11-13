//! Operator entry point for the opt-in durable verification workflow.

use rovik_core::harness::{GitIntegration, HarnessProvider, Rates};
use rovik_harness::{
    approvals::{ApprovalRequest, Principal},
    controller::{Controller, TaskSetup},
    identity::Digest,
    ledger::Ledger,
    operations::{FailureResolution, Reconciliation},
    runner::{self, Cancellation, ContainerBackend},
    wire, Error, Result,
};
use std::path::Path;

fn print(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| Error::new(e.to_string()))?
    );
    Ok(())
}
fn read<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    wire::decode(&std::fs::read(path)?)
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
        Err(Error::classified(
            rovik_harness::FailureCategory::Capability,
            "offline verification cannot make model calls",
        ))
    }
}
fn integration(
    repository_id: String,
    root: &Path,
    reference: Option<&String>,
) -> Result<GitIntegration> {
    match reference {
        Some(reference) => GitIntegration::for_reference(repository_id, root, reference),
        None => GitIntegration::new(repository_id, root),
    }
}
fn run() -> Result<i32> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.last().is_some_and(|arg| arg == "--json") {
        args.pop();
    }
    let usage="usage: rovik-harness <absolute-state-dir> init | import <repository-id> <repository-path> [--dirty <untracked-paths.json>] | inventory <snapshot.json> | profile resolve <inventory.json> <selections.json> | contract import <contract.json> <registry.json> | plan <planning.json> | prepare <preparation.json> | draft <run-id> <domain|architect|oracle|reviewer> <inputs.json> <url> <rates.json> | promote <setup.json> | stage-replan <setup.json> | activate-replan <run-id> <setup-digest> | load-image <archive.tar> | put <kind> <file> | create <setup.json> | approve <run-id> <request.json> | run <run-id> <repository-path> <chat-completions-url> <rates.json> [reference] | status <run-id> | approvals <run-id> | revoke <run-id> <approval-id> <reason> | resume <run-id> | cancel <run-id> | recover <run-id> | reconcile <run-id> <usage.json> | resolve-failure <run-id> <resolution.json> | resolve-replan <run-id> <resolution.json> | retention <grace-seconds> [--apply] | verify <run-id> <repository-path> [reference] | gate <run-id> <repository-path> [reference] | verify-run <run-id> <repository-path> [reference] | merge-prepare <run-id> <new-run-id> <repository-path> <reference> <expected-head> | prepare-delivery <run-id> <repository-path> <reference> <message-file> | deliver <delivery.json> <bare-repository> | export <run-id> <new-directory>\nSet ROVIK_HARNESS_API_KEY for run. ROVIK_HARNESS_ENGINE selects docker (default) or podman; ROVIK_HARNESS_DOCKER_BIN and ROVIK_HARNESS_PODMAN_BIN select absolute client paths. The state directory must be outside the repository.";
    if args.len() < 2 {
        return Err(Error::new(usage.into()));
    }
    let state = Path::new(&args[0]);
    let mut ledger = Ledger::open(state)?;
    // Registration and drafting need the trusted ledger, not an installed container engine.
    match args[1].as_str() {
        "init" if args.len() == 2 => {
            print(
                &serde_json::json!({"schema_version":1,"state_directory":state,"initialized":true}),
            )?;
            return Ok(0);
        }
        "inventory" if args.len() == 3 => {
            let snapshot = read(&args[2])?;
            let inventory = rovik_harness::onboarding::inspect(&ledger, &snapshot)?;
            let artifact = ledger.store_json("report", &inventory, runner::now_ms()?)?;
            print(&serde_json::json!({"inventory":inventory,"artifact":artifact}))?;
            return Ok(0);
        }
        "profile" if args.len() == 5 && args[2] == "resolve" => {
            let profiles = rovik_harness::onboarding::resolve(
                &ledger,
                &read(&args[3])?,
                &read::<Vec<_>>(&args[4])?,
            )?;
            ledger.store_json("profile", &profiles, runner::now_ms()?)?;
            print(&profiles)?;
            return Ok(0);
        }
        "plan" if args.len() == 3 => {
            let plan = read::<rovik_harness::planning::PlanningInput>(&args[2])?.derive()?;
            ledger.store_json("plan", &plan, runner::now_ms()?)?;
            print(&plan)?;
            return Ok(0);
        }
        "contract" if args.len() == 5 && args[2] == "import" => {
            let contract: rovik_harness::contract::TaskContract = read(&args[3])?;
            contract.validate(&read(&args[4])?)?;
            print(&ledger.store_json("contract", &contract, runner::now_ms()?)?)?;
            return Ok(0);
        }
        "prepare" if args.len() == 3 => {
            let preparation: rovik_harness::roles::Preparation = read(&args[2])?;
            print(&preparation.create(&mut ledger)?)?;
            return Ok(0);
        }
        "draft" if args.len() == 7 => {
            let role = match args[3].as_str() {
                "domain" => rovik_harness::provider::Role::DomainAnalyst,
                "architect" => rovik_harness::provider::Role::Architect,
                "oracle" => rovik_harness::provider::Role::OracleAuthor,
                "reviewer" => rovik_harness::provider::Role::Reviewer,
                _ => {
                    return Err(Error::new(
                        "draft role must be domain, architect, oracle or reviewer".into(),
                    ))
                }
            };
            let run = ledger.run(&args[2])?;
            let preparation: rovik_harness::roles::Preparation =
                wire::decode(&ledger.read(&run.input_digest)?)?;
            let mut inputs: std::collections::BTreeMap<String, Digest> = read(&args[4])?;
            inputs.insert(
                "request".into(),
                ledger
                    .store(
                        "context",
                        "text/plain",
                        preparation.request.as_bytes(),
                        runner::now_ms()?,
                    )?
                    .digest,
            );
            let runtime = tokio::runtime::Runtime::new().map_err(|e| Error::new(e.to_string()))?;
            let key = std::env::var("ROVIK_HARNESS_API_KEY").map_err(|_| {
                Error::new("set ROVIK_HARNESS_API_KEY in the trusted controller environment".into())
            })?;
            let mut provider = HarnessProvider::new(
                runtime.handle().clone(),
                args[5].clone(),
                key,
                read::<Rates>(&args[6])?,
            )?;
            let input = rovik_harness::roles::RoleInput {
                run_id: &preparation.run_id,
                role,
                snapshot: &preparation.base,
                registry: &preparation.registry,
                artifacts: &inputs,
                limits: &preparation.worker,
                deadline_ms: preparation.deadline_ms,
            };
            let draft = rovik_harness::roles::generate(
                &mut ledger,
                &input,
                &mut provider,
                &Cancellation::default(),
            )?;
            print(&serde_json::json!({"draft_digest":Digest::of_json(&draft)?,"draft":draft}))?;
            return Ok(0);
        }
        "put" if args.len() == 4 => {
            print(&ledger.store(
                &args[2],
                "application/octet-stream",
                &std::fs::read(&args[3])?,
                runner::now_ms()?,
            )?)?;
            return Ok(0);
        }
        "import" if args.len() == 4 || (args.len() == 6 && args[4] == "--dirty") => {
            let repository = Path::new(&args[3]).canonicalize()?;
            if state.canonicalize()?.starts_with(&repository) {
                return Err(Error::new(
                    "controller state must be outside the repository".into(),
                ));
            }
            let integration = GitIntegration::new(args[2].clone(), &repository)?;
            let snapshot = if args.len() == 6 {
                integration.snapshot_worktree(&ledger, &read(&args[5])?)?
            } else {
                integration.snapshot(&ledger)?
            };
            ledger.store_json("snapshot", &snapshot, runner::now_ms()?)?;
            print(&snapshot)?;
            return Ok(0);
        }
        _ => {}
    }
    let management_only = matches!(
        args[1].as_str(),
        "status"
            | "approve"
            | "approvals"
            | "revoke"
            | "resume"
            | "cancel"
            | "recover"
            | "reconcile"
            | "resolve-failure"
            | "resolve-replan"
            | "retention"
            | "export"
            | "gate"
            | "merge-prepare"
            | "prepare-delivery"
            | "deliver"
    );
    let backend = match std::env::var("ROVIK_HARNESS_ENGINE")
        .as_deref()
        .unwrap_or("docker")
    {
        "docker" => {
            let path = std::env::var("ROVIK_HARNESS_DOCKER_BIN")
                .unwrap_or_else(|_| "/usr/bin/docker".into());
            if management_only && !Path::new(&path).is_file() {
                ContainerBackend::management()
            } else {
                ContainerBackend::local(Path::new(&path))?
            }
        }
        "podman" => {
            let path = std::env::var("ROVIK_HARNESS_PODMAN_BIN")
                .unwrap_or_else(|_| "/usr/bin/podman".into());
            if management_only && !Path::new(&path).is_file() {
                ContainerBackend::management()
            } else {
                ContainerBackend::podman(Path::new(&path), &state.join("podman"))?
            }
        }
        _ => {
            return Err(Error::new(
                "ROVIK_HARNESS_ENGINE must be docker or podman".into(),
            ))
        }
    };
    let mut controller = Controller { ledger, backend };
    let mut exit_code = 0;
    match args[1].as_str() {
        "load-image" if args.len() == 3 => {
            controller.backend.import_image(Path::new(&args[2]))?;
            print(&serde_json::json!({"loaded":true,"backend":controller.backend.version()}))
        }
        "create" if args.len() == 3 => {
            let setup: TaskSetup = read(&args[2])?;
            let run = controller.create(&setup, runner::now_ms()?)?;
            print(
                &serde_json::json!({"run_id":run.run_id,"state":run.state,"approval_requirements":setup.mandatory_approvals()?,"policy_digest":Digest::of_json(&setup.policy)?,"scope_digest":Digest::of_json(&setup.contract.scope)?}),
            )
        }
        "promote" if args.len() == 3 => {
            let setup: TaskSetup = read(&args[2])?;
            print(&controller.promote_preparation(&Principal::current()?, &setup)?)
        }
        "stage-replan" if args.len() == 3 => {
            let setup: TaskSetup = read(&args[2])?;
            let approval = controller.stage_replan(&Principal::current()?, &setup)?;
            print(
                &serde_json::json!({"replan":approval,"requirements":setup.approval_requirements(None)?,
                "policy_digest":Digest::of_json(&setup.policy)?,"scope_digest":Digest::of_json(&setup.contract.scope)?}),
            )
        }
        "activate-replan" if args.len() == 4 => print(&controller.activate_replan(
            &Principal::current()?,
            &args[2],
            &Digest::parse(&args[3])?,
        )?),
        "approve" if args.len() == 4 => {
            let setup = controller.setup(&args[2])?;
            let request: ApprovalRequest = read(&args[3])?;
            if request.run_id.as_deref() != Some(&args[2]) {
                return Err(Error::new("CLI approval must name this exact run".into()));
            }
            print(&controller.ledger.grant(
                &Principal::current()?,
                &setup.authority,
                request,
                runner::now_ms()?,
            )?)
        }
        "run" if args.len() == 6 || args.len() == 7 => {
            let setup = controller.setup(&args[2])?;
            let repository = Path::new(&args[3]).canonicalize()?;
            if state.canonicalize()?.starts_with(&repository) {
                return Err(Error::new(
                    "controller state must be outside the repository".into(),
                ));
            }
            let integration = integration(setup.repository_id, &repository, args.get(6))?;
            let runtime = tokio::runtime::Runtime::new().map_err(|e| Error::new(e.to_string()))?;
            let key = std::env::var("ROVIK_HARNESS_API_KEY").map_err(|_| {
                Error::new("set ROVIK_HARNESS_API_KEY in the trusted controller environment".into())
            })?;
            let mut provider = HarnessProvider::new(
                runtime.handle().clone(),
                args[4].clone(),
                key,
                read::<Rates>(&args[5])?,
            )?;
            let cancel = Cancellation::default();
            let interrupt = cancel.clone();
            let signal = runtime.spawn(async move {
                if let Err(error) = tokio::signal::ctrl_c().await {
                    eprintln!("cannot observe interrupt signal: {error}");
                }
                interrupt.cancel();
            });
            let result = controller.execute(&args[2], &mut provider, &integration, &cancel);
            signal.abort();
            let report = result?;
            exit_code = report.state.execution_exit_code();
            print(&report)
        }
        "status" if args.len() == 3 => {
            let run = controller.ledger.run(&args[2])?;
            print(
                &serde_json::json!({"run_id":run.run_id,"state":run.state,"revision":run.revision,"restart_state":run.restart_state,"binding":run.binding_digest,"budgets":controller.ledger.budgets(&args[2])?,"operations":controller.ledger.operations(&args[2])?}),
            )
        }
        "approvals" if args.len() == 3 => print(
            &serde_json::json!({"pending":controller.pending_approvals(&args[2])?,"history":controller.ledger.approval_history(&args[2])?}),
        ),
        "revoke" if args.len() == 5 => {
            controller.revoke_approval(&Principal::current()?, &args[2], &args[3], &args[4])?;
            print(&serde_json::json!({"revoked":args[3]}))
        }
        "resume" if args.len() == 3 => {
            controller.resume(&Principal::current()?, &args[2])?;
            print(&controller.ledger.run(&args[2])?)
        }
        "cancel" if args.len() == 3 => {
            controller.cancel(&Principal::current()?, &args[2])?;
            let removed = controller.recover(&Principal::current()?, &args[2])?;
            print(
                &serde_json::json!({"state":controller.ledger.run(&args[2])?.state,"removed_jobs":removed,"budgets":controller.ledger.budgets(&args[2])?}),
            )
        }
        "recover" if args.len() == 3 => {
            let removed = controller.recover(&Principal::current()?, &args[2])?;
            print(
                &serde_json::json!({"state":controller.ledger.run(&args[2])?.state,"removed_jobs":removed,"budgets":controller.ledger.budgets(&args[2])?}),
            )
        }
        "reconcile" if args.len() == 4 => {
            controller.reconcile(
                &Principal::current()?,
                &args[2],
                &read::<Reconciliation>(&args[3])?,
            )?;
            print(&controller.ledger.budgets(&args[2])?)
        }
        "resolve-failure" if args.len() == 4 => print(&controller.resolve_failure(
            &Principal::current()?,
            &args[2],
            &read::<FailureResolution>(&args[3])?,
        )?),
        "resolve-replan" if args.len() == 4 => print(&controller.resolve_replanned_failure(
            &Principal::current()?,
            &args[2],
            &read::<rovik_harness::resolution::ReplannedFailure>(&args[3])?,
        )?),
        "retention" if args.len() == 3 || (args.len() == 4 && args[3] == "--apply") => {
            let grace = args[2]
                .parse::<u64>()
                .map_err(|_| Error::new("retention grace must be seconds".into()))?;
            print(&controller.ledger.retention(grace, args.len() == 4)?)
        }
        "merge-prepare" if args.len() == 7 => {
            let setup = controller.setup(&args[2])?;
            setup
                .authority
                .authorize(&Principal::current()?, "operate")?;
            let candidate = controller
                .current_candidate(&args[2])?
                .ok_or_else(|| Error::new("source run has no candidate".into()))?;
            let integration = GitIntegration::for_reference(
                setup.repository_id.clone(),
                Path::new(&args[4]),
                &args[5],
            )?;
            let merged =
                integration.prepare_merge(&controller.ledger, &setup.base, &candidate, &args[6])?;
            let proposed = integration.merge_setup(&setup, &merged, &args[3])?;
            let artifact =
                controller
                    .ledger
                    .store_json("merge-candidate", &merged, runner::now_ms()?)?;
            print(
                &serde_json::json!({"merge": merged, "artifact": artifact, "proposed_setup": proposed, "requires_new_approvals_and_full_verification": true}),
            )
        }
        "prepare-delivery" if args.len() == 6 => {
            let setup = controller.setup(&args[2])?;
            setup
                .authority
                .authorize(&Principal::current()?, "operate")?;
            let integration = GitIntegration::for_reference(
                setup.repository_id.clone(),
                Path::new(&args[3]),
                &args[4],
            )?;
            let delivery = integration.prepare_delivery(
                &controller,
                &args[2],
                &std::fs::read_to_string(&args[5])?,
            )?;
            print(
                &serde_json::json!({"subject_digest": Digest::of_json(&delivery)?, "approval_type":"delivery", "delivery":delivery,
                "scope_digest":Digest::of_json(&setup.contract.scope)?, "policy_digest":Digest::of_json(&setup.policy)?}),
            )
        }
        "deliver" if args.len() == 4 => {
            let delivery: rovik_core::harness::DeliveryCandidate = read(&args[2])?;
            let integration = GitIntegration::for_reference(
                delivery.repository_id.clone(),
                Path::new(&args[3]),
                &delivery.destination_reference,
            )?;
            print(&integration.deliver(&controller, &Principal::current()?, &delivery)?)
        }
        "verify-run" if args.len() == 4 || args.len() == 5 => {
            let setup = controller.setup(&args[2])?;
            setup
                .authority
                .authorize(&Principal::current()?, "operate")?;
            let integration = integration(setup.repository_id, Path::new(&args[3]), args.get(4))?;
            let report = controller.execute(
                &args[2],
                &mut OfflineProvider,
                &integration,
                &Cancellation::default(),
            )?;
            exit_code = report.state.execution_exit_code();
            print(&report)
        }
        "verify" | "gate" if args.len() == 4 || args.len() == 5 => {
            let setup = controller.setup(&args[2])?;
            let integration = integration(setup.repository_id, Path::new(&args[3]), args.get(4))?;
            let gate = if args[1] == "verify" {
                controller.verify_fresh(
                    &Principal::current()?,
                    &args[2],
                    &integration,
                    &Cancellation::default(),
                )?
            } else {
                controller.verify(&args[2], &integration)?
            };
            exit_code = match gate.verdict {
                rovik_harness::evidence::Verdict::Ready => 0,
                rovik_harness::evidence::Verdict::Blocked => 2,
                rovik_harness::evidence::Verdict::Failed => 3,
            };
            print(&gate)
        }
        "export" if args.len() == 4 => {
            print(&controller.export_bundle(&args[2], Path::new(&args[3]))?)
        }
        _ => Err(Error::new(usage.into())),
    }?;
    Ok(exit_code)
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"error":error.message,"category":error.category,"exit_code":error.exit_code()})
            );
            std::process::exit(error.exit_code());
        }
    }
}
