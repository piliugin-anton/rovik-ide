//! Repair probes execute through the command tool, and acceptance still
//! requires the configured full suite after any successful probe.

#![allow(clippy::await_holding_lock)]

use super::*;

const RUNNER: &str = r#"import pathlib, sys
root = pathlib.Path(__file__).parent
state = (root / 'src/a.py').read_text().strip()
focused = any(arg.startswith('tests/') for arg in sys.argv[1:])
with (root / 'executions').open('a') as out:
    out.write('P' if focused else 'F')
if focused and state == 'empty':
    print('running 0 tests')
    sys.exit(0)
if focused and state == 'missing':
    print("ModuleNotFoundError: No module named 'fixture_dependency'")
    sys.exit(1)
failed = state.startswith('bad') or (not focused and state == 'other')
if failed:
    name = 'b' if state == 'other' else 'a'
    print(f'FAILED tests/test_{name}.py::test_{name} - AssertionError')
    print(f'tests/test_{name}.py:1: AssertionError')
    print('=== 1 failed, 0 passed in 0.01s ===' if focused else '=== 1 failed, 11 passed in 0.01s ===')
    sys.exit(1)
print('=== 1 passed in 0.01s ===' if focused else '=== 12 passed in 0.01s ===')
"#;

async fn prepare(command: &str, denied: Vec<String>) -> (Harness, crate::agent_run::run::Run) {
    let h = harness(&[
        ("src/a.py", "bad-first"),
        ("tests/test_a.py", "def test_a(): pass\n"),
        ("tests/test_b.py", "def test_b(): pass\n"),
        ("pytest.py", RUNNER),
    ])
    .await;
    h.set_checks(json!([{ "kind": "test", "command": command, "enabled": true }]));
    let (url, _) = model(vec![]).await;
    let request = h.request(
        &url,
        Harness::send("Repair the tests"),
        Permissions {
            allow_any_command: true,
            denied_commands: denied,
            ..checks_on()
        },
    );
    let agents = h.core.agents.clone();
    agents.set_config(request.config.clone().unwrap());
    let controls = agents.register(&h.dialog_id, "p::m", None).unwrap();
    let (mut run, _) = crate::agent_run::run::Run::prepare(&h.core, request, controls)
        .await
        .unwrap()
        .unwrap();
    run.changed_files = vec!["src/a.py".into()];
    run.verification.changes_made();
    (h, run)
}

fn repair(h: &Harness, run: &mut crate::agent_run::run::Run, state: &str) {
    std::fs::write(h.project.join("src/a.py"), state).unwrap();
    run.verification.changes_made();
}

fn executions(h: &Harness) -> String {
    std::fs::read_to_string(h.project.join("executions")).unwrap()
}

#[tokio::test]
async fn unsuccessful_focused_repairs_defer_the_suite_and_green_runs_it_in_the_same_round() {
    let _environment = stable_check_environment();
    let (h, mut run) = prepare("python -m pytest", vec![]).await;
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    assert_eq!(executions(&h), "F", "first verification is the full suite");
    repair(&h, &mut run, "bad-second");
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    assert_eq!(
        executions(&h),
        "FP",
        "a failing probe defers the expensive suite"
    );
    let rows = h.messages().await;
    let focused = rows
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification"))
        .find(|v| v["fullTestsDeferred"] == true)
        .unwrap();
    assert!(
        focused["testsRun"].is_null(),
        "a partial count is not a full-suite count"
    );
    assert!(
        focused["integrity"].is_null(),
        "fewer selected tests are not weakened tests"
    );
    assert_eq!(
        focused["targetedChecks"][0]["suiteCommand"],
        "python -m pytest"
    );
    repair(&h, &mut run, "good");
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(
        executions(&h),
        "FPPF",
        "the full suite still runs on the final budgeted round"
    );
}

#[tokio::test]
async fn a_passing_probe_does_not_hide_an_unrelated_full_suite_failure() {
    let _environment = stable_check_environment();
    let (h, mut run) = prepare("python -m pytest", vec![]).await;
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    repair(&h, &mut run, "other");
    let outcome = run.verify_at_end(false, None).await.unwrap();
    assert_ne!(outcome, "verified");
    assert_eq!(executions(&h), "FPF");
    assert!(h.messages().await.iter().any(|m| m
        .meta
        .as_ref()
        .and_then(|v| v.pointer("/verification/checks/0/output"))
        .and_then(Value::as_str)
        .is_some_and(|output| output.contains("test_b"))));
}

#[tokio::test]
async fn an_empty_probe_falls_back_to_full_verification() {
    let _environment = stable_check_environment();
    let (h, mut run) = prepare("python -m pytest", vec![]).await;
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    repair(&h, &mut run, "empty");
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(executions(&h), "FPF");
}

#[tokio::test]
async fn an_unavailable_probe_falls_back_without_consuming_an_extra_repair_round() {
    let _environment = stable_check_environment();
    let (h, mut run) = prepare("python -m pytest", vec![]).await;
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    repair(&h, &mut run, "missing");
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(executions(&h), "FPF");
    assert_eq!(run.verification.rounds(), 2);
}

#[tokio::test]
async fn runner_options_preserve_the_original_full_command() {
    let _environment = stable_check_environment();
    let (h, mut run) = prepare("python -m pytest -q", vec![]).await;
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    repair(&h, &mut run, "good");
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(executions(&h), "FF");
}

#[tokio::test]
async fn a_denied_derived_command_falls_back_to_the_allowed_full_suite() {
    let _environment = stable_check_environment();
    let (h, mut run) = prepare(
        "python -m pytest",
        vec!["python -m pytest tests/test_a.py".into()],
    )
    .await;
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    repair(&h, &mut run, "good");
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(executions(&h), "FF");
}

#[tokio::test]
async fn stopping_before_a_repair_does_not_start_a_probe_or_the_full_suite() {
    let _environment = stable_check_environment();
    let (h, mut run) = prepare("python -m pytest", vec![]).await;
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    repair(&h, &mut run, "good");
    assert!(agent_stop(&h.core, h.dialog_id.clone()).await);
    assert_eq!(
        run.verify_at_end(false, None).await.unwrap(),
        "verify-stopped"
    );
    assert_eq!(executions(&h), "F");
}
