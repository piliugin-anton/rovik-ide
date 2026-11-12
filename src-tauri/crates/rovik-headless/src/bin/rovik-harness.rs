//! Operator entry point for the opt-in durable verification workflow.

use rovik_core::harness::{GitIntegration, HarnessProvider, Rates};
use rovik_harness::{
    approvals::{ApprovalRequest, Principal},
    controller::{Controller, TaskSetup},
    identity::Digest,
    ledger::Ledger,
    runner::{self, Cancellation, DockerBackend},
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
    let usage="usage: rovik-harness <absolute-state-dir> import <repository-id> <repository-path> | put <kind> <file> | create <setup.json> | approve <run-id> <request.json> | run <run-id> <repository-path> <chat-completions-url> <rates.json> | status <run-id> | verify <run-id> <repository-path> | export <run-id> <new-directory>\nSet ROVIK_HARNESS_API_KEY for run. The state directory must be outside the repository.";
    if args.len() < 2 {
        return Err(Error(usage.into()));
    }
    let state = Path::new(&args[0]);
    let docker =
        std::env::var("ROVIK_HARNESS_DOCKER_BIN").unwrap_or_else(|_| "/usr/bin/docker".into());
    let mut controller = Controller {
        ledger: Ledger::open(state)?,
        backend: DockerBackend::local(Path::new(&docker))?,
    };
    match args[1].as_str() {
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
            print(&controller.execute(
                &args[2],
                &mut provider,
                &integration,
                &Cancellation::default(),
            )?)
        }
        "status" if args.len() == 3 => {
            let run = controller.ledger.run(&args[2])?;
            print(
                &serde_json::json!({"run_id":run.run_id,"state":run.state,"revision":run.revision,"restart_state":run.restart_state,"binding":run.binding_digest,"budgets":controller.ledger.budgets(&args[2])?}),
            )
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
