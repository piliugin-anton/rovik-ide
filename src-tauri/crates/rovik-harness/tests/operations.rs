#![cfg(target_os = "linux")]
mod support;
use rovik_harness::{
    approvals::Principal,
    artifacts::ArtifactStore,
    controller::Controller,
    fixtures,
    identity::Digest,
    ledger::Ledger,
    operations::Reconciliation,
    runner::{self, DockerBackend},
    state::State,
};
fn controller(path: &std::path::Path) -> Controller {
    Controller {
        ledger: Ledger::open(path).unwrap(),
        backend: DockerBackend::local(std::path::Path::new("/bin/true")).unwrap(),
    }
}
#[test]
fn recovery_fences_callbacks_and_requires_actual_accounting_before_resume() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let mut c = controller(&root);
    let setup = fixtures::quota_rust(&c.ledger, "recover", Digest::of(b"image"), "base").unwrap();
    let now = runner::now_ms().unwrap();
    c.create(&setup, now).unwrap();
    let input = c
        .ledger
        .store_json(
            "operation",
            &serde_json::json!({"attempt":"interrupted"}),
            now,
        )
        .unwrap();
    let lease = c
        .ledger
        .claim(
            "recover",
            "model-1",
            "model",
            &input.digest,
            "lost-controller",
            now,
            60_000,
            &[
                ("model_calls".into(), 1),
                ("tokens".into(), 100),
                ("cost_microunits".into(), 10),
            ]
            .into(),
        )
        .unwrap();
    drop(c);
    let mut c = controller(&root);
    let operator = Principal::current().unwrap();
    c.recover(&operator, "recover").unwrap();
    assert_eq!(c.ledger.run("recover").unwrap().state, State::Blocked);
    assert!(c
        .ledger
        .claim(
            "recover",
            "late",
            "model",
            &input.digest,
            "late-owner",
            now,
            60_000,
            &[("model_calls".into(), 1)].into()
        )
        .is_err());
    assert!(c
        .ledger
        .verify_lease(&lease, runner::now_ms().unwrap())
        .is_err());
    assert!(c.resume(&operator, "recover").is_err());
    let usage = Reconciliation {
        operation_id: lease.operation_id,
        usage: [
            ("model_calls".into(), 1),
            ("tokens".into(), 137),
            ("cost_microunits".into(), 11),
        ]
        .into(),
        reason: "Fixture provider accounting receipt after simulated interruption".into(),
    };
    c.reconcile(&operator, "recover", &usage).unwrap();
    assert!(c.reconcile(&operator, "recover", &usage).is_err());
    let accounts = c.ledger.budgets("recover").unwrap();
    assert_eq!(accounts["tokens"].spent, 137);
    assert_eq!(accounts["tokens"].reserved, 0);
    c.resume(&operator, "recover").unwrap();
    assert_eq!(c.ledger.run("recover").unwrap().state, State::Intake);
    assert!(c.verify("recover", &Base).is_err());
}
struct Base;
impl rovik_harness::controller::Integration for Base {
    fn current_revision(&self, _: &str) -> rovik_harness::Result<String> {
        Ok("base".into())
    }
}
#[test]
fn cancelled_runs_reject_late_usage_and_overruns_are_recorded_before_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let setup = fixtures::quota_rust(&c.ledger, "cancel", Digest::of(b"image"), "base").unwrap();
    let now = runner::now_ms().unwrap();
    c.create(&setup, now).unwrap();
    let input = c.ledger.store_json("operation", &"request", now).unwrap();
    let lease = c
        .ledger
        .claim(
            "cancel",
            "attempt",
            "model",
            &input.digest,
            "owner",
            now,
            60000,
            &[("model_calls".into(), 1)].into(),
        )
        .unwrap();
    let operator = Principal::current().unwrap();
    c.cancel(&operator, "cancel").unwrap();
    c.cancel(&operator, "cancel").unwrap();
    assert!(c
        .ledger
        .complete(
            &lease,
            &input.digest,
            &[("model_calls".into(), 1)].into(),
            now
        )
        .is_err());
    assert!(c.resume(&operator, "cancel").is_err());
    let setup = fixtures::quota_rust(&c.ledger, "overrun", Digest::of(b"image"), "base").unwrap();
    c.create(&setup, now).unwrap();
    let lease = c
        .ledger
        .claim(
            "overrun",
            "attempt",
            "model",
            &input.digest,
            "owner",
            now,
            60000,
            &[("model_calls".into(), 1)].into(),
        )
        .unwrap();
    c.recover(&operator, "overrun").unwrap();
    c.reconcile(
        &operator,
        "overrun",
        &Reconciliation {
            operation_id: lease.operation_id,
            usage: [("model_calls".into(), 9)].into(),
            reason: "Actual usage exceeds the task limit".into(),
        },
    )
    .unwrap();
    assert_eq!(c.ledger.budgets("overrun").unwrap()["model_calls"].spent, 9);
    assert_eq!(c.ledger.run("overrun").unwrap().state, State::Stopped);
}

#[test]
fn an_abandoned_zero_cost_operation_still_requires_explicit_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = controller(&dir.path().join("state"));
    let setup = fixtures::quota_rust(&c.ledger, "zero-cost", Digest::of(b"image"), "base").unwrap();
    let now = runner::now_ms().unwrap();
    c.create(&setup, now).unwrap();
    let input = c
        .ledger
        .store_json("operation", &"free-provider-attempt", now)
        .unwrap();
    let lease = c
        .ledger
        .claim(
            &setup.run_id,
            "cost-only-fixture",
            "model",
            &input.digest,
            "owner",
            now,
            60_000,
            &[("cost_microunits".into(), 0)].into(),
        )
        .unwrap();
    let principal = Principal::current().unwrap();
    c.recover(&principal, &setup.run_id).unwrap();
    assert!(c
        .ledger
        .budgets(&setup.run_id)
        .unwrap()
        .values()
        .all(|b| b.reserved == 0));
    assert!(c.resume(&principal, &setup.run_id).is_err());
    c.reconcile(
        &principal,
        &setup.run_id,
        &Reconciliation {
            operation_id: lease.operation_id,
            usage: [("cost_microunits".into(), 0)].into(),
            reason: "Fixture free-provider receipt explicitly reports zero monetary cost".into(),
        },
    )
    .unwrap();
    c.resume(&principal, &setup.run_id).unwrap();
}

#[test]
fn the_complete_initial_schema_is_atomic_and_reopening_preserves_runs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let artifact = ArtifactStore::open(&root.join("artifacts"), 64 * 1024 * 1024)
        .unwrap()
        .put("contract", "application/json", b"{}")
        .unwrap();
    let database = rusqlite::Connection::open(root.join("ledger.sqlite3")).unwrap();
    // A late collision must roll back every table and the version marker together.
    database.execute_batch("CREATE TABLE failure_resolutions(conflict TEXT); INSERT INTO failure_resolutions VALUES('preserve');").unwrap();
    assert!(Ledger::open(&root).is_err());
    assert_eq!(
        database
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT conflict FROM failure_resolutions", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
    database
        .execute_batch("DROP TABLE failure_resolutions;")
        .unwrap();
    let mut ledger = Ledger::open(&root).unwrap();
    assert_eq!(
        database
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM schema_versions WHERE version=1",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM schema_versions", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        1
    );
    for table in [
        "artifact_types",
        "jobs",
        "usage_receipts",
        "reconciliations",
        "failure_resolutions",
    ] {
        assert_eq!(
            database
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
            1,
            "missing {table}"
        );
    }
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='failure_dispositions'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        0
    );
    let registered = ledger
        .store("contract", "application/json", b"{}", 0)
        .unwrap();
    assert_eq!(registered, artifact);
    let run = ledger
        .create_run(
            "current",
            &artifact.digest,
            &[("checks".into(), 1)].into(),
            0,
        )
        .unwrap();
    drop(ledger);
    let reopened = Ledger::open(&root).unwrap();
    assert_eq!(reopened.run("current").unwrap().state, State::Intake);
    assert_eq!(reopened.run("current").unwrap().revision, run.revision);
    assert_eq!(reopened.read(&artifact.digest).unwrap(), b"{}");
    assert_eq!(reopened.budgets("current").unwrap()["checks"].limit, 1);
    reopened.verify_artifacts().unwrap();
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM schema_versions", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        1
    );
}
#[test]
fn retention_preserves_registered_evidence_and_only_removes_aged_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let ledger = Ledger::open(&root).unwrap();
    let registered = ledger
        .store("source", "text/plain", b"registered", 0)
        .unwrap();
    let store = ArtifactStore::open(&root.join("artifacts"), 64 * 1024 * 1024).unwrap();
    let orphan = store
        .put("source", "text/plain", b"interrupted publication")
        .unwrap();
    let recent = store
        .put("source", "text/plain", b"recent publication")
        .unwrap();
    for reference in [&registered, &orphan] {
        std::fs::File::open(root.join("artifacts").join(reference.digest.hex()))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(172800))
            .unwrap();
    }
    assert!(ledger.retention(0, true).is_err());
    let report = ledger.retention(86400, false).unwrap();
    assert!(!report.applied);
    assert_eq!(report.orphan_digests, vec![orphan.digest.clone()]);
    assert!(store.read(&orphan).is_ok());
    ledger.retention(86400, true).unwrap();
    assert!(store.read(&orphan).is_err());
    assert!(store.read(&recent).is_ok());
    assert!(ledger.read(&registered.digest).is_ok());
    let recovered = ledger
        .store(
            "source",
            "text/plain",
            b"interrupted publication",
            runner::now_ms().unwrap(),
        )
        .unwrap();
    assert_eq!(recovered.digest, orphan.digest);
    ledger.verify_artifacts().unwrap();
}

#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and a pinned Alpine image"]
fn killed_controller_recovery_reaps_native_job_and_fences_its_unsettled_attempt() {
    use rovik_harness::{
        identity::EvidenceKey,
        ledger::Lease,
        normalizer,
        plan::{CheckKind, CheckSpec},
        registry::{CommandRegistry, Parser},
        runner::{Cancellation, Environment, ExecutionInput},
        snapshot::Snapshot,
        wire,
    };
    use std::process::{Child, Command, Stdio};
    type Job = (Lease, Snapshot, Environment, CommandRegistry, CheckSpec);
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    // Re-execute this test in a child process; SIGKILL exercises the actual loss
    // of RAII cleanup, SQLite connection and in-memory execution state.
    if let Ok(root) = std::env::var("ROVIK_HARNESS_CRASH_CHILD_STATE") {
        let ledger = Ledger::open(std::path::Path::new(&root)).unwrap();
        let c = Controller {
            ledger,
            backend: DockerBackend::local(&support::docker_path()).unwrap(),
        };
        let setup = c.setup("recover-native").unwrap();
        let (lease, oracle, environment, registry, spec): Job = wire::decode(
            &std::fs::read(std::path::Path::new(&root).join("crash-job.json")).unwrap(),
        )
        .unwrap();
        c.backend
            .execute_leased(
                &c.ledger,
                &setup.base,
                &oracle,
                &setup.dependencies,
                &environment,
                &registry,
                &spec,
                &Cancellation::default(),
                &lease,
            )
            .unwrap();
        panic!("parent must kill the controller while native work is running");
    }
    fn docker() -> Command {
        let mut command = Command::new(support::docker_path());
        for name in [
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            "DOCKER_TLS",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
        ] {
            command.env_remove(name);
        }
        command.arg("--host=unix:///var/run/docker.sock");
        command
    }
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                self.0.kill().unwrap();
            }
            self.0.wait().unwrap();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let mut c = Controller {
        ledger: Ledger::open(&root).unwrap(),
        backend: DockerBackend::local(&support::docker_path()).unwrap(),
    };
    let setup = fixtures::quota_rust(
        &c.ledger,
        "recover-native",
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
        "base",
    )
    .unwrap();
    let now = runner::now_ms().unwrap();
    c.create(&setup, now).unwrap();
    let oracle = fixtures::snapshot(
        &c.ledger,
        "protected-crash-probe",
        &[(
            "hold.sh",
            "set -eu\nprintf untrusted > /build/untrusted-leftover\nsleep 60\n",
        )],
    )
    .unwrap();
    let mut command = setup.registry.commands[0].clone();
    command.id = "crash-probe".into();
    command.executable = "/bin/sh".into();
    command.argv_template = vec!["/oracle/hold.sh".into()];
    command.parameters.clear();
    command.environment.clear();
    command.kind = CheckKind::Static;
    command.parser = Parser::ExitCode;
    command.checker_digest = normalizer::checker_digest(Parser::ExitCode);
    command.report_file = None;
    command.limits.timeout_ms = 120_000;
    let registry = CommandRegistry {
        commands: vec![command.clone()],
    };
    let mut spec = setup.plan.checks[0].clone();
    spec.command_id = command.id;
    spec.checker_digest = command.checker_digest;
    spec.expected_case_ids.clear();
    spec.minimum_test_count = 0;
    spec.validated_parameters.clear();
    spec.timeout_ms = command.limits.timeout_ms;
    let mut environment = setup.environment.clone();
    environment.registry_digest = Digest::of_json(&registry).unwrap();
    let request = c
        .ledger
        .store_json("operation", &"crash-rehearsal", now)
        .unwrap();
    let key = EvidenceKey {
        candidate_digest: setup.base.digest().unwrap(),
        contract_digest: request.digest.clone(),
        design_digest: request.digest.clone(),
        policy_digest: request.digest.clone(),
        oracle_digest: oracle.digest().unwrap(),
        plan_digest: Digest::of_json(&spec).unwrap(),
        environment_digest: Digest::of_json(&environment).unwrap(),
    };
    let binding = c.ledger.store_json("operation", &key, now).unwrap();
    c.ledger
        .bind(&c.ledger.run(&setup.run_id).unwrap(), &binding.digest, now)
        .unwrap();
    let input = c
        .ledger
        .store_json(
            "operation",
            &ExecutionInput {
                evidence_key: key,
                check: spec.clone(),
            },
            now,
        )
        .unwrap();
    let lease = c
        .ledger
        .claim(
            &setup.run_id,
            "crash-check",
            "check",
            &input.digest,
            "lost-process",
            now,
            300_000,
            &[("checks".into(), 1)].into(),
        )
        .unwrap();
    std::fs::write(
        root.join("crash-job.json"),
        serde_json::to_vec(&(lease.clone(), oracle, environment, registry, spec)).unwrap(),
    )
    .unwrap();
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "killed_controller_recovery_reaps_native_job_and_fences_its_unsettled_attempt",
                "--nocapture",
            ])
            .env("ROVIK_HARNESS_CRASH_CHILD_STATE", &root)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let database = rusqlite::Connection::open(root.join("ledger.sqlite3")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let (job, workspace) = loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child controller exited before the crash"
        );
        let registered: rusqlite::Result<(String, String)> = database.query_row(
            "SELECT job_id,workspace FROM jobs WHERE operation_id=?1",
            [&lease.operation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        if let Ok((job, workspace)) = registered {
            if docker()
                .args(["exec", &job, "test", "-f", "/build/untrusted-leftover"])
                .output()
                .unwrap()
                .status
                .success()
            {
                break (job, workspace);
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native job did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    assert_eq!(
        String::from_utf8(
            docker()
                .args(["inspect", "--format", "{{.State.Running}}", &job])
                .output()
                .unwrap()
                .stdout
        )
        .unwrap()
        .trim(),
        "true"
    );
    assert!(std::path::Path::new(&workspace).exists());
    drop(c);
    let mut c = Controller {
        ledger: Ledger::open(&root).unwrap(),
        backend: DockerBackend::local(&support::docker_path()).unwrap(),
    };
    let operator = Principal::current().unwrap();
    assert_eq!(
        c.recover(&operator, &setup.run_id).unwrap(),
        vec![job.clone()]
    );
    assert!(!docker()
        .args(["inspect", &job])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!std::path::Path::new(&workspace).exists());
    assert!(c.recover(&operator, &setup.run_id).unwrap().is_empty());
    assert_eq!(c.ledger.run(&setup.run_id).unwrap().state, State::Blocked);
    assert!(c
        .ledger
        .verify_lease(&lease, runner::now_ms().unwrap())
        .is_err());
    assert!(c
        .ledger
        .complete(
            &lease,
            &input.digest,
            &[("checks".into(), 1)].into(),
            runner::now_ms().unwrap()
        )
        .is_err());
    assert!(c
        .ledger
        .admitted_results(&setup.run_id, &binding.digest)
        .unwrap()
        .is_empty());
    assert_eq!(
        c.ledger.budgets(&setup.run_id).unwrap()["checks"].reserved,
        1
    );
    assert!(c.resume(&operator, &setup.run_id).is_err());
    c.reconcile(
        &operator,
        &setup.run_id,
        &Reconciliation {
            operation_id: lease.operation_id,
            usage: [("checks".into(), 1)].into(),
            reason: "Observed one actual native launch before SIGKILL; no output is accepted"
                .into(),
        },
    )
    .unwrap();
    c.resume(&operator, &setup.run_id).unwrap();
    assert_eq!(c.ledger.run(&setup.run_id).unwrap().state, State::Intake);
    assert_eq!(c.ledger.budgets(&setup.run_id).unwrap()["checks"].spent, 1);
}
