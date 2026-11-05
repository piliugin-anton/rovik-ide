//! Unit tests for the run's memory: the plan a check round makes of the
//! memories the run used, and the reflection a run leaves behind.
//!
//! They are offline and leave no files: the store is `open_in_memory`, so a
//! process per test (nextest) and threads of one (cargo test) both behave.

use super::*;
use crate::agent_run::verify::{CheckResult, ErrorFiles};
use verified_code_memory::{MemoryRecord, NewMemory, SharedMemoryStore};

/// A check result with just what the plan decides on: which command ran, how it
/// ended and — for a failure — the files the errors point at.
fn result(command: &str, status: &'static str, changed: &[&str]) -> CheckResult {
    CheckResult {
        kind: "test".to_string(),
        command: command.to_string(),
        status,
        exit_code: Default::default(),
        timed_out: Default::default(),
        duration_ms: Default::default(),
        reason: Default::default(),
        output: Default::default(),
        error_files: if changed.is_empty() {
            None
        } else {
            Some(ErrorFiles {
                changed: changed.iter().map(|path| path.to_string()).collect(),
                other: Default::default(),
            })
        },
        note: Default::default(),
        failures: Default::default(),
        tests_ran: Default::default(),
        saved_output: Default::default(),
        environment: Default::default(),
        preexisting: Default::default(),
    }
}

/// One memory in the project's store, about the files given.
fn remembered(store: &SharedMemoryStore, title: &str, files: &[&str]) -> MemoryRecord {
    store
        .remember(
            NewMemory::procedure("p", title, "what it learned").with_files(files.iter().copied()),
        )
        .expect("the in-memory store takes a memory")
}

/// The run's memory state after a recall that found `recalled`.
fn run_with(store: SharedMemoryStore, recalled: Vec<MemoryRecord>) -> RunMemory {
    RunMemory {
        store: Some(store),
        repo: "p".to_string(),
        task: "tidy the helper".to_string(),
        recalled,
        ..Default::default()
    }
}

#[test]
fn a_green_round_confirms_the_memories_the_run_used() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "how the tests here run", &["src/a.rs"]);
    let id = memory.id;
    let run = run_with(store, vec![memory]);

    let plan = run.check_plan(&[result("cargo test", "passed", &[])]);

    assert_eq!(plan.verify.len(), 1, "a memory the run used is confirmed");
    assert_eq!(plan.verify[0].0, id);
    assert!(
        plan.verify[0].1.contains("cargo test"),
        "the evidence names the check that passed, was {:?}",
        plan.verify[0].1
    );
    assert!(
        plan.deprecate.is_empty(),
        "a round that passed invalidates nothing"
    );
}

#[test]
fn a_failure_in_the_memorys_own_file_invalidates_it() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let about_a = remembered(&store, "the helper is pure", &["src/a.rs"]);
    store
        .verify(about_a.id, "the checks passed: cargo test")
        .expect("the store verifies a memory");
    let about_b = remembered(&store, "the loader is lazy", &["src/b.rs"]);
    let (id, other) = (about_a.id, about_b.id);
    let run = run_with(store, vec![about_a, about_b]);

    let plan = run.check_plan(&[result("cargo test", "failed", &["src/a.rs"])]);

    assert_eq!(
        plan.deprecate.len(),
        1,
        "only the memory about the file the failure names goes, was {:?}",
        plan.deprecate
    );
    assert_eq!(plan.deprecate[0].0, id);
    assert!(
        plan.deprecate[0].1.contains("src/a.rs"),
        "the evidence names the file the failure is in, was {:?}",
        plan.deprecate[0].1
    );
    assert!(
        !plan
            .deprecate
            .iter()
            .any(|(deprecated, _)| *deprecated == other),
        "the memory about another file keeps its state"
    );
    assert!(
        plan.verify.is_empty(),
        "a round that failed confirms nothing"
    );
}

#[test]
fn a_failure_elsewhere_invalidates_nothing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    store
        .verify(memory.id, "the checks passed: cargo test")
        .expect("the store verifies a memory");
    let run = run_with(store, vec![memory]);

    let plan = run.check_plan(&[result("cargo test", "failed", &["src/b.rs"])]);

    assert!(
        plan.deprecate.is_empty(),
        "a failure outside the memory's files proves nothing, was {:?}",
        plan.deprecate
    );
    assert!(
        plan.verify.is_empty(),
        "a round that failed confirms nothing either"
    );
}

#[test]
fn a_run_that_recalled_nothing_plans_nothing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let run = run_with(store, Vec::new());

    let plan = run.check_plan(&[result("cargo test", "passed", &[])]);

    assert!(plan.verify.is_empty(), "there is nothing to confirm");
    assert!(plan.deprecate.is_empty(), "there is nothing to invalidate");
}

#[test]
fn a_round_with_no_results_plans_nothing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let run = run_with(store, vec![memory]);

    let plan = run.check_plan(&[]);

    assert!(plan.verify.is_empty(), "no check ran, so nothing is proven");
    assert!(
        plan.deprecate.is_empty(),
        "no check failed, so nothing is blamed"
    );
}

#[test]
fn a_reflection_states_the_outcome_the_files_and_the_checks() {
    let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
    let results = vec![
        result("cargo test", "passed", &[]),
        result("cargo clippy", "passed", &[]),
    ];

    let reflection = reflect(
        "p",
        "Fix the flaky test\nand tidy the helper",
        "verified",
        &files,
        &results,
    );

    assert_eq!(reflection.repo, "p");
    assert_eq!(reflection.title, "Fix the flaky test");
    assert!(
        reflection.content.contains("Outcome: verified"),
        "the reflection says how the run ended, was {:?}",
        reflection.content
    );
    assert!(
        reflection.content.contains("Changed: src/a.rs, src/b.rs"),
        "the reflection lists the files it changed, was {:?}",
        reflection.content
    );
    assert!(
        reflection.content.contains("Checks: cargo test passed"),
        "the reflection says what ran and how it ended, was {:?}",
        reflection.content
    );
    assert!(
        reflection.content.contains("cargo clippy passed"),
        "every check that ran is named, was {:?}",
        reflection.content
    );
    assert_eq!(reflection.files.len(), 2);
    assert!(reflection.files.iter().any(|file| file == "src/a.rs"));
    assert!(reflection.files.iter().any(|file| file == "src/b.rs"));
}

#[test]
fn a_reflection_is_titled_after_the_tasks_first_real_line() {
    let reflection = reflect(
        "p",
        "\n   \n  Fix the flaky test  \nthen tidy the helper",
        "done",
        &[],
        &[],
    );

    assert_eq!(reflection.title, "Fix the flaky test");
}

#[test]
fn a_reflection_of_a_run_that_changed_nothing_names_no_files() {
    let reflection = reflect("p", "Look at the helper", "unverified", &[], &[]);

    assert!(
        !reflection.content.contains("Changed:"),
        "a run that changed nothing has no files to list, was {:?}",
        reflection.content
    );
    assert!(reflection.files.is_empty());
    assert!(reflection.content.contains("Outcome: unverified"));
}
