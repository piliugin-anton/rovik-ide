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
        serde_json::to_string_pretty(value).map_err(|e| Error(e.to_string()))?
    );
    Ok(())
}
fn read<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    wire::decode(&std::fs::read(path)?)
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage="usage: rovik-harness <absolute-state-dir> import <repository-id> <repository-path> | load-image <archive.tar> | put <kind> <file> | create <setup.json> | approve <run-id> <request.json> | run <run-id> <repository-path> <chat-completions-url> <rates.json> | status <run-id> | approvals <run-id> | revoke <run-id> <approval-id> <reason> | resume <run-id> | cancel <run-id> | recover <run-id> | reconcile <run-id> <usage.json> | resolve-failure <run-id> <resolution.json> | retention <grace-seconds> [--apply] | verify <run-id> <repository-path> | export <run-id> <new-directory>\nSet ROVIK_HARNESS_API_KEY for run. ROVIK_HARNESS_ENGINE selects docker (default) or podman; ROVIK_HARNESS_DOCKER_BIN and ROVIK_HARNESS_PODMAN_BIN select absolute client paths. The state directory must be outside the repository.";
    if args.len() < 2 {
        return Err(Error(usage.into()));
    }
    let state = Path::new(&args[0]);
    let ledger = Ledger::open(state)?;
    let backend = match std::env::var("ROVIK_HARNESS_ENGINE")
        .as_deref()
        .unwrap_or("docker")
    {
        "docker" => {
            let path = std::env::var("ROVIK_HARNESS_DOCKER_BIN")
                .unwrap_or_else(|_| "/usr/bin/docker".into());
            ContainerBackend::local(Path::new(&path))?
        }
        "podman" => {
            let path = std::env::var("ROVIK_HARNESS_PODMAN_BIN")
                .unwrap_or_else(|_| "/usr/bin/podman".into());
            ContainerBackend::podman(Path::new(&path), &state.join("podman"))?
        }
        _ => {
            return Err(Error(
                "ROVIK_HARNESS_ENGINE must be docker or podman".into(),
            ))
        }
    };
    let mut controller = Controller { ledger, backend };
    match args[1].as_str() {
        "load-image" if args.len() == 3 => {
            controller.backend.import_image(Path::new(&args[2]))?;
            print(&serde_json::json!({"loaded":true,"backend":controller.backend.version()}))
        }
        "import" if args.len() == 4 => {
            let repository = Path::new(&args[3]).canonicalize()?;
            if state.canonicalize()?.starts_with(&repository) {
                return Err(Error(
                    "controller state must be outside the candidate repository".into(),
                ));
            }
            let integration = GitIntegration::new(args[2].clone(), &repository)?;
            let snapshot = integration.snapshot(&controller.ledger)?;
            controller
                .ledger
                .store_json("snapshot", &snapshot, runner::now_ms()?)?;
            print(&snapshot)
        }
        "put" if args.len() == 4 => {
            let bytes = std::fs::read(&args[3])?;
            print(&controller.ledger.store(
                &args[2],
                "application/octet-stream",
                &bytes,
                runner::now_ms()?,
            )?)
        }
        "create" if args.len() == 3 => {
            let setup: TaskSetup = read(&args[2])?;
            let run = controller.create(&setup, runner::now_ms()?)?;
            print(
                &serde_json::json!({"run_id":run.run_id,"state":run.state,"approval_requirements":setup.mandatory_approvals()?,"policy_digest":Digest::of_json(&setup.policy)?,"scope_digest":Digest::of_json(&setup.contract.scope)?}),
            )
        }
        "approve" if args.len() == 4 => {
            let setup = controller.setup(&args[2])?;
            let request: ApprovalRequest = read(&args[3])?;
            if request.run_id.as_deref() != Some(&args[2]) {
                return Err(Error("CLI approval must name this exact run".into()));
            }
            print(&controller.ledger.grant(
                &Principal::current()?,
                &setup.authority,
                request,
                runner::now_ms()?,
            )?)
        }
        "run" if args.len() == 6 => {
            let setup = controller.setup(&args[2])?;
            let repository = Path::new(&args[3]).canonicalize()?;
            if state.canonicalize()?.starts_with(&repository) {
                return Err(Error(
                    "controller state must be outside the repository".into(),
                ));
            }
            let integration = GitIntegration::new(setup.repository_id, &repository)?;
            let runtime = tokio::runtime::Runtime::new().map_err(|e| Error(e.to_string()))?;
            let key = std::env::var("ROVIK_HARNESS_API_KEY").map_err(|_| {
                Error("set ROVIK_HARNESS_API_KEY in the trusted controller environment".into())
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
            print(&result?)
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
        "retention" if args.len() == 3 || (args.len() == 4 && args[3] == "--apply") => {
            let grace = args[2]
                .parse::<u64>()
                .map_err(|_| Error("retention grace must be seconds".into()))?;
            print(&controller.ledger.retention(grace, args.len() == 4)?)
        }
        "verify" if args.len() == 4 => {
            let setup = controller.setup(&args[2])?;
            let integration = GitIntegration::new(setup.repository_id, Path::new(&args[3]))?;
            print(&controller.verify(&args[2], &integration)?)
        }
        "export" if args.len() == 4 => {
            let snapshot = controller
                .current_candidate(&args[2])?
                .ok_or_else(|| Error("run has no candidate".into()))?;
            snapshot.materialize(&controller.ledger, Path::new(&args[3]))?;
            print(&snapshot.digest()?)
        }
        _ => Err(Error(usage.into())),
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
