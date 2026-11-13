//! Reuse runs the real commands through both mid-run and final gates.

// Command environment is process-wide; keep host-toolchain tests from
// changing it between the before/after snapshots (also under cargo test).
#![allow(clippy::await_holding_lock)]

use super::*;

async fn prepare(h: &Harness) -> crate::agent_run::run::Run {
    let (url, _) = model(vec![]).await;
    let request = h.request(
        &url,
        Harness::send("Greet"),
        Permissions {
            allow_any_command: true,
            ..checks_on()
        },
    );
    let state = h.core.agents.clone();
    state.set_config(request.config.clone().unwrap());
    let controls = state.register(&h.dialog_id, "p::m", None).unwrap();
    let (mut run, _) = crate::agent_run::run::Run::prepare(&h.core, request, controls)
        .await
        .unwrap()
        .unwrap();
    run.changed_files = vec!["a.txt".into()];
    run.verification.changes_made();
    run
}

fn counted_check(h: &Harness, reuse: bool) -> PathBuf {
    // Instrumentation lives outside the checked tree; the check only reads
    // project inputs. Writing a counter inside it would correctly invalidate it.
    let counter = h._dir.0.join("check-count");
    let command = format!("printf x >> '{}'", counter.display());
    h.set_checks(
        json!([{ "kind": "test", "command": command, "enabled": true, "reuseSuccess": reuse }]),
    );
    counter
}

#[tokio::test]
async fn successful_midrun_checks_are_reused_at_unchanged_completion() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "hello\n")]).await;
    let counter = counted_check(&h, true);
    let mut run = prepare(&h).await;
    assert!(run.verify_now(&[]).await.unwrap().contains("pass"));
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "x");
    let rows = h.messages().await;
    let final_check = rows
        .iter()
        .rev()
        .find_map(|m| m.meta.as_ref()?.pointer("/verification/checks/0"))
        .unwrap();
    assert!(final_check["note"]
        .as_str()
        .unwrap()
        .starts_with("Reused unchanged results"));
}

#[tokio::test]
async fn checks_without_opt_in_execute_again_at_completion() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "hello\n")]).await;
    let counter = counted_check(&h, false);
    let mut run = prepare(&h).await;
    run.verify_now(&[]).await.unwrap();
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "xx");
}

#[tokio::test]
async fn external_edit_of_an_ignored_input_forces_a_new_execution() {
    let _environment = stable_check_environment();
    let h = harness(&[
        ("a.txt", "hello\n"),
        (".gitignore", "dependency\n"),
        ("dependency", "old"),
    ])
    .await;
    let counter = counted_check(&h, true);
    let mut run = prepare(&h).await;
    run.verify_now(&[]).await.unwrap();
    std::fs::write(h.project.join("dependency"), "new").unwrap();
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "xx");
}

#[tokio::test]
async fn a_potentially_modifying_command_invalidates_the_receipt_even_without_an_edit() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "hello\n")]).await;
    let counter = counted_check(&h, true);
    let mut run = prepare(&h).await;
    run.verify_now(&[]).await.unwrap();
    run.note_command("sed -i 's/hello/hello/' a.txt");
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "xx");
}

#[tokio::test]
async fn checks_that_change_their_inputs_cannot_leave_a_reusable_receipt() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "hello\n")]).await;
    let counter = h._dir.0.join("check-count");
    let command = format!("printf x >> '{}'; printf x >> a.txt", counter.display());
    h.set_checks(
        json!([{ "kind": "test", "command": command, "enabled": true, "reuseSuccess": true }]),
    );
    let mut run = prepare(&h).await;
    run.verify_now(&[]).await.unwrap();
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "xx");
}

#[tokio::test]
async fn a_changed_check_command_never_uses_the_earlier_receipt() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "hello\n")]).await;
    let counter = counted_check(&h, true);
    let mut run = prepare(&h).await;
    run.verify_now(&[]).await.unwrap();
    run.checks[0].command = format!("printf y >> '{}'", counter.display());
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "verified");
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "xy");
}

#[tokio::test]
async fn failed_midrun_checks_are_never_reused() {
    let _environment = stable_check_environment();
    let h = harness(&[("a.txt", "hello\n")]).await;
    let counter = h._dir.0.join("check-count");
    let command = format!(
        "printf x >> '{}'; echo 'a.txt:1: error: broken'; exit 1",
        counter.display()
    );
    h.set_checks(
        json!([{ "kind": "test", "command": command, "enabled": true, "reuseSuccess": true }]),
    );
    let mut run = prepare(&h).await;
    assert!(run.verify_now(&[]).await.unwrap().contains("some fail"));
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "xx");
}

#[tokio::test]
async fn reused_passing_commands_still_flag_weakened_tests_at_the_final_gate() {
    let _environment = stable_check_environment();
    let before =
        "test('a', () => { expect(1).toBe(1); });\ntest('b', () => { expect(2).toBe(2); });\n";
    let after = "test('b', () => { expect(2).toBe(2); });\n";
    let h = harness(&[("a.txt", "hello\n"), ("src/a.test.ts", after)]).await;
    let counter = counted_check(&h, true);
    let mut run = prepare(&h).await;
    run.verification.original_found(
        "src/a.test.ts".into(),
        subagent::Original::new(
            Some(before.as_bytes().to_vec()),
            subagent::Source::ViewAtRunStart,
        ),
    );
    assert!(run.verify_now(&[]).await.unwrap().contains("pass"));
    assert_eq!(run.verify_at_end(false, None).await.unwrap(), "continue");
    assert_eq!(
        run.verify_at_end(false, None).await.unwrap(),
        "verify-flagged"
    );
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "x");
    assert!(h
        .messages()
        .await
        .iter()
        .any(|m| m.content.contains("fewer assertions")));
}
