//! Unit tests for the run's memory: the plan a check round makes of the
//! memories the run used, the association a run's end reports, and the
//! reflection a run leaves behind.
//!
//! They are offline and leave no files: the store is `open_in_memory`, so a
//! process per test (nextest) and threads of one (cargo test) both behave.

use super::*;
use crate::agent_run::verify::{CheckResult, ErrorFiles};
use rovik_protocol::events::MemoryAssociation;
use verified_code_memory::model::claim_hash;
use verified_code_memory::{MemoryRecord, NewMemory, SharedMemoryStore, Verification};

#[test]
fn a_long_query_keeps_its_goal_and_diagnostic_tail() {
    let request = format!(
        "Repair the scheduler {} failure: retry-lock-deadlock",
        "x".repeat(4000)
    );
    let query = task_query(&request);
    assert!(query.starts_with("Repair the scheduler"));
    assert!(query.ends_with("failure: retry-lock-deadlock"));
    assert_eq!(query.chars().count(), TASK_CHARS);
}

#[test]
fn durable_text_scrubs_credentials_without_erasing_the_project_fact() {
    let text = "Store credentials in the keychain; api_key=abcdefghijklmnop123456";
    let safe = redact_secrets(text);
    assert!(safe.contains("Store credentials in the keychain"));
    assert!(!safe.contains("abcdefghijklmnop123456"));
    assert_eq!(
        redact_secrets("Use per-order locking."),
        "Use per-order locking."
    );
}

#[test]
fn private_key_bodies_and_truncated_keys_are_not_persisted() {
    let key = "-----BEGIN PRIVATE KEY-----\nsensitive-key-body\n-----END PRIVATE KEY-----";
    assert_eq!(
        redact_secrets(&format!("before {key} after")),
        "before [redacted] after"
    );
    assert_eq!(
        redact_secrets("-----BEGIN OPENSSH PRIVATE KEY-----\ntruncated-key"),
        "[redacted]"
    );
}

#[test]
fn a_scoped_version_tracks_dirty_source_changes_and_ignores_other_files() {
    let dir = crate::testing::temp_dir();
    std::fs::write(dir.0.as_path().join("a.rs"), "lock(order);").unwrap();
    let files = vec!["a.rs".to_string()];
    let version = scoped_version(dir.0.as_path().to_str(), &files).unwrap();
    std::fs::write(dir.0.as_path().join("unrelated.rs"), "changed").unwrap();
    assert_eq!(
        scoped_version(dir.0.as_path().to_str(), &files).unwrap(),
        version
    );
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let record = store
        .remember(
            NewMemory::procedure("p", "order lock", "lock per order")
                .with_files(files.clone())
                .with_code_version(version),
        )
        .unwrap();
    let record = store.verify(record.id, "order regression passed").unwrap();
    assert!(fingerprint_matches(&record, dir.0.as_path().to_str()));
    std::fs::write(dir.0.as_path().join("a.rs"), "unlock(order);").unwrap();
    assert!(!fingerprint_matches(&record, dir.0.as_path().to_str()));
    std::fs::remove_file(dir.0.as_path().join("a.rs")).unwrap();
    assert!(!fingerprint_matches(&record, dir.0.as_path().to_str()));
}

#[test]
fn source_versions_do_not_read_files_outside_the_project() {
    let dir = crate::testing::temp_dir();
    let project = dir.0.as_path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(dir.0.as_path().join("outside.txt"), "outside").unwrap();
    assert!(scoped_version(project.to_str(), &["../outside.txt".to_owned()]).is_none());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside.txt", project.join("link.txt")).unwrap();
        assert!(scoped_version(project.to_str(), &["link.txt".to_owned()]).is_none());
    }
}

#[test]
fn a_paused_run_carries_its_original_generation_after_clear() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let memory = run_with(store.clone(), Vec::new());
    store.clear("p").unwrap();
    assert_eq!(memory.carry().unwrap().generation, 0);
    assert_ne!(
        store.generation("p").unwrap(),
        memory.carry().unwrap().generation
    );
}

#[test]
fn a_paused_run_keeps_its_task_and_observed_file_cache() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let mut memory = run_with(store, Vec::new());
    memory.task = "Inspect the scheduler".into();
    memory.observed_files.insert("src/scheduler.rs".into());
    let carry = memory.carry().unwrap();
    assert_eq!(carry.task, memory.task);
    assert_eq!(carry.observed_files, memory.observed_files);
}

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

/// A memory the project already knows is right.
fn verified(store: &SharedMemoryStore, title: &str, files: &[&str]) -> MemoryRecord {
    let memory = remembered(store, title, files);
    store
        .verify(memory.id, "12/12 tests passed")
        .expect("the store verifies a memory")
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

// A round of checks: what it plans, and what it never plans.

#[test]
fn a_green_round_plans_nothing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "how the tests here run", &["src/a.rs"]);
    let run = run_with(store, vec![memory]);

    let plan = run.check_plan(&[result("cargo test", "passed", &[])]);

    assert!(
        plan.revalidate.is_empty(),
        "a passing suite says nothing about this claim, so nothing is asked of it, was {:?}",
        plan.revalidate
    );
}

#[test]
fn a_green_round_plans_nothing_for_a_memory_the_project_already_verified() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = verified(&store, "how the tests here run", &["src/a.rs"]);
    let id = memory.id;
    let run = run_with(store, vec![memory]);

    let plan = run.check_plan(&[result("cargo test", "passed", &[])]);

    assert!(
        plan.revalidate.is_empty(),
        "a green round promotes nothing and asks nothing, was {:?}",
        plan.revalidate
    );
    let stored = run.store.as_ref().unwrap().get(id).unwrap();
    assert_eq!(
        stored.verification,
        Verification::Verified,
        "the trust the memory had is not disturbed"
    );
    assert_eq!(
        stored.applicability.to_string(),
        "current",
        "the claim is still expected to hold"
    );
}

#[test]
fn a_failure_in_the_memorys_own_file_plans_it_to_be_checked_again() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let about_a = verified(&store, "the helper is pure", &["src/a.rs"]);
    let about_b = remembered(&store, "the loader is lazy", &["src/b.rs"]);
    let (id, other) = (about_a.id, about_b.id);
    let run = run_with(store, vec![about_a, about_b]);

    let plan = run.check_plan(&[result("cargo test", "failed", &["src/a.rs"])]);

    assert_eq!(
        plan.revalidate.len(),
        1,
        "only the memory about the file the failure names is planned, was {:?}",
        plan.revalidate
    );
    assert_eq!(plan.revalidate[0].0, id);
    let reason = &plan.revalidate[0].1;
    assert!(
        reason.contains("cargo test"),
        "the plan says which check failed, was {reason:?}"
    );
    assert!(
        reason.contains("src/a.rs"),
        "the plan says which file it failed in, was {reason:?}"
    );
    assert!(
        !plan.revalidate.iter().any(|(planned, _)| *planned == other),
        "the memory about another file keeps its state"
    );
}

#[test]
fn a_failure_elsewhere_plans_nothing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = verified(&store, "the helper is pure", &["src/a.rs"]);
    let run = run_with(store, vec![memory]);

    let plan = run.check_plan(&[result("cargo test", "failed", &["src/b.rs"])]);

    assert!(
        plan.revalidate.is_empty(),
        "a failure outside the memory's files proves nothing, was {:?}",
        plan.revalidate
    );
}

#[test]
fn a_deprecated_memory_is_never_planned() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let dropped = store
        .deprecate(memory.id, "the project moved on")
        .expect("the store deprecates a memory");
    // The run recalled it before the project dropped it: the plan reads that
    // snapshot, so the snapshot has to say what the store says.
    let run = run_with(store, vec![dropped]);

    let plan = run.check_plan(&[result("cargo test", "failed", &["src/a.rs"])]);

    assert!(
        plan.revalidate.is_empty(),
        "a memory the project already dropped is not asked to be checked again, was {:?}",
        plan.revalidate
    );
}

#[test]
fn a_run_that_recalled_nothing_plans_nothing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let run = run_with(store, Vec::new());

    let plan = run.check_plan(&[result("cargo test", "failed", &["src/a.rs"])]);

    assert!(
        plan.revalidate.is_empty(),
        "there is nothing to check again"
    );
}

#[test]
fn a_round_with_no_results_plans_nothing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let run = run_with(store, vec![memory]);

    let plan = run.check_plan(&[]);

    assert!(
        plan.revalidate.is_empty(),
        "no check failed, so nothing is blamed"
    );
}

// The end of a run: which checks followed, and never whether a memory helped.

#[test]
fn the_end_of_a_green_run_is_associated_with_the_checks_passing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let run = run_with(store, vec![memory]);

    for outcome in ["verified", "verify-unchecked", "repaired"] {
        assert_eq!(
            association(outcome, &run),
            Some(MemoryAssociation::ChecksPassed),
            "a run that ended {outcome:?} left passing checks behind"
        );
    }
}

#[test]
fn the_end_of_a_run_whose_checks_fell_short_is_associated_with_the_checks_failing() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let run = run_with(store, vec![memory]);

    for outcome in [
        "verify-flagged",
        "verify-unresolved",
        "verify-gave-up",
        "unmet",
    ] {
        assert_eq!(
            association(outcome, &run),
            Some(MemoryAssociation::ChecksFailed),
            "a run that ended {outcome:?} left failing checks behind"
        );
    }
}

#[test]
fn a_run_that_ended_done_is_associated_only_when_its_last_round_passed() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let mut run = run_with(store, vec![memory]);

    assert_eq!(
        association("done", &run),
        None,
        "a run whose end only says it is done reported no check"
    );

    run.note_round(&[result("cargo test", "passed", &[])]);
    assert_eq!(
        association("done", &run),
        Some(MemoryAssociation::ChecksPassed),
        "the round the run ended on passed"
    );

    run.note_round(&[result("cargo test", "failed", &["src/a.rs"])]);
    assert_eq!(
        association("done", &run),
        None,
        "the round the run ended on failed, so the run is not credited with passing checks"
    );
}

#[test]
fn a_run_that_checked_nothing_reports_no_association() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let run = run_with(store, vec![memory]);

    for outcome in ["unverified", "verify-needs-approval", "stopped", ""] {
        assert_eq!(
            association(outcome, &run),
            None,
            "a run that ended {outcome:?} checked nothing, so it says nothing about the checks"
        );
    }
}

#[test]
fn recording_the_association_counts_the_use_and_leaves_the_claim_alone() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = verified(&store, "the helper is pure", &["src/a.rs"]);
    let before = store.get(memory.id).unwrap();

    let credited = record_association(
        &store,
        std::slice::from_ref(&memory),
        MemoryAssociation::ChecksPassed,
    );

    assert_eq!(credited.len(), 1, "every recalled memory is credited");
    assert_eq!(credited[0].id, before.id);
    let after = store.get(before.id).unwrap();
    assert_eq!(after.use_count, before.use_count + 1, "the run used it");
    assert_eq!(
        after.success_count,
        before.success_count + 1,
        "the checks passed while it was in use"
    );
    assert_eq!(
        after.content_updated_at, before.content_updated_at,
        "an association says how the run went, not that the claim changed"
    );
    assert_eq!(
        after.verification, before.verification,
        "it is no verdict on the claim"
    );
    assert_eq!(
        after.applicability, before.applicability,
        "nor a statement about the code of today"
    );
}

#[test]
fn recording_the_association_of_a_run_whose_checks_failed_counts_no_success() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let before = store.get(memory.id).unwrap();

    record_association(
        &store,
        std::slice::from_ref(&memory),
        MemoryAssociation::ChecksFailed,
    );

    let after = store.get(before.id).unwrap();
    assert_eq!(after.use_count, before.use_count + 1, "the run used it");
    assert_eq!(
        after.success_count, before.success_count,
        "the checks did not pass while it was in use"
    );
    assert_eq!(
        after.verification,
        Verification::Provisional,
        "a failing round is not a verdict on the claim either"
    );
    assert_eq!(after.applicability.to_string(), "unknown");
    assert_eq!(after.content_updated_at, before.content_updated_at);
}

// What the chat's panel is told about a memory.

#[test]
fn the_panels_memory_carries_its_applicability_and_the_association() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let provisional = remembered(&store, "the helper is pure", &["src/a.rs"]);
    let current = verified(&store, "the loader is lazy", &["src/b.rs"]);
    let stale = remembered(&store, "the queue is bounded", &["src/c.rs"]);
    let stale = store
        .mark_needs_revalidation(stale.id)
        .expect("the store marks a memory for revalidation");

    let passing = serde_json::to_value(recalled(
        &provisional,
        Some(MemoryAssociation::ChecksPassed),
    ))
    .expect("a recalled memory is serialised");
    assert_eq!(passing["applicability"], "unknown");
    assert_eq!(passing["association"], "checksPassed");
    assert_eq!(passing["verification"], "provisional");
    assert!(
        passing.get("helped").is_none(),
        "the panel is no longer told whether the memory helped: {passing}"
    );

    let failing = serde_json::to_value(recalled(&current, Some(MemoryAssociation::ChecksFailed)))
        .expect("a recalled memory is serialised");
    assert_eq!(failing["applicability"], "current");
    assert_eq!(failing["association"], "checksFailed");

    let unmarked = serde_json::to_value(recalled(&stale, Some(MemoryAssociation::ChecksFailed)))
        .expect("a recalled memory is serialised");
    assert_eq!(unmarked["applicability"], "needs_revalidation");

    let unchecked =
        serde_json::to_value(recalled(&current, None)).expect("a recalled memory is serialised");
    assert!(
        unchecked.get("association").is_none(),
        "a run that checked nothing says nothing about the checks: {unchecked}"
    );
}

// The reflection a run leaves behind.

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

// What the run may put in front of the model at all: a memory reaches it
// only when the evidence behind it still holds of its claim and it names no
// file the run may not read. The two rules are asked of their own questions:
// `steers` of the evidence, `admits`/`admit` of the files.

const PROJECT: &str = "/project";

/// The run of a project rooted at `/project` that holds `recalled`, with the
/// files the user restricted (`restricted`, as Settings → Restricted files
/// saves them) and the files a child's kind sealed (`sealed`) kept from the
/// model.
fn run_keeping(
    store: SharedMemoryStore,
    recalled: Vec<MemoryRecord>,
    restricted: &[&str],
    sealed: &[&str],
) -> RunMemory {
    let entries: Vec<serde_json::Value> = restricted
        .iter()
        .map(|path| serde_json::json!({ "path": path }))
        .collect();
    RunMemory {
        store: Some(store),
        repo: "p".to_string(),
        task: "tidy the helper".to_string(),
        recalled,
        restricted: std::sync::Arc::new(crate::restricted_files::RestrictedFiles::from_settings(
            &serde_json::json!({ "restrictedFiles": entries }),
        )),
        root: Some(PROJECT.to_string()),
        sealed: sealed.iter().map(|path| path.to_string()).collect(),
        ..Default::default()
    }
}

/// Whether the model is given this record: both rules have to hold.
fn reaches_the_model(run: &RunMemory, record: &MemoryRecord) -> bool {
    run.steers(record) && run.admits(record)
}

#[test]
fn a_memory_naming_a_file_the_user_restricted_never_reaches_the_model() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = verified(
        &store,
        "the text file the check reads wants ok",
        &["src/ok.txt"],
    );
    let evidence_holds = memory.evidence_matches_claim();
    let run = run_keeping(store, vec![memory], &["src/ok.txt"], &[]);

    assert!(
        !run.admits(&run.recalled[0]),
        "the record speaks about a file the user keeps from the agent, so the \
         model is told nothing of it, whatever its evidence says"
    );
    assert!(
        evidence_holds,
        "its evidence was never the question: the file is what keeps it out"
    );
    assert!(
        !reaches_the_model(&run, &run.recalled[0]),
        "and so the model is not given it"
    );
}

#[test]
fn the_same_memory_reaches_the_model_when_the_user_restricts_no_file() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = verified(
        &store,
        "the text file the check reads wants ok",
        &["src/ok.txt"],
    );
    let run = run_keeping(store, vec![memory], &[], &[]);

    assert!(
        run.admits(&run.recalled[0]),
        "nothing keeps the file from the agent"
    );
    assert!(
        run.steers(&run.recalled[0]),
        "and the evidence backs the claim"
    );
    assert!(
        reaches_the_model(&run, &run.recalled[0]),
        "a verified claim about a file the run may read is exactly what the \
         model should be given"
    );
}

#[test]
fn a_memory_naming_a_file_a_child_kept_sealed_never_reaches_the_model() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let sealed_test = verified(
        &store,
        "the writer's test greets by name",
        &["src/greet.test.ts"],
    );
    let ordinary = verified(&store, "the retry is serialized per order", &["src/a.rs"]);
    let run = run_keeping(
        store,
        vec![sealed_test, ordinary],
        &[],
        &["src/greet.test.ts"],
    );

    assert!(
        !run.admits(&run.recalled[0]),
        "a file a child's kind keeps to itself is no file the run may read, so \
         a memory naming it never reaches the model"
    );
    assert!(
        !reaches_the_model(&run, &run.recalled[0]),
        "the seal keeps it from the model even though its evidence holds"
    );
    assert!(
        run.admits(&run.recalled[1]),
        "the seal is only over the writer's file: a memory about another file \
         still reaches the model"
    );
}

#[test]
fn a_provisional_memory_never_reaches_the_model() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = remembered(
        &store,
        "the text file the check reads wants ok",
        &["src/ok.txt"],
    );
    assert_eq!(
        memory.verification,
        Verification::Provisional,
        "nothing was ever checked against this claim"
    );
    let run = run_keeping(store, vec![memory], &[], &[]);

    assert!(
        !run.steers(&run.recalled[0]),
        "an unchecked guess does not steer the model, whatever files it names"
    );
    assert!(
        !reaches_the_model(&run, &run.recalled[0]),
        "no file was restricted, so the evidence is what kept it out"
    );
}

#[test]
fn a_memory_the_checks_marked_to_be_checked_again_never_reaches_the_model() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let memory = verified(
        &store,
        "the text file the check reads wants ok",
        &["src/ok.txt"],
    );
    let stale = store
        .mark_needs_revalidation(memory.id)
        .expect("the store marks a memory to be checked again");
    assert_eq!(
        stale.verification,
        Verification::Verified,
        "the claim keeps the evidence it was given"
    );
    assert_eq!(
        stale.applicability.to_string(),
        "needs_revalidation",
        "but the code of today may have moved past it"
    );
    let run = run_keeping(store, vec![stale], &[], &[]);

    assert!(
        !run.steers(&run.recalled[0]),
        "a verified claim a failing check called into question does not steer \
         the model as though it still held"
    );
    assert!(
        !reaches_the_model(&run, &run.recalled[0]),
        "and so the model is not given it"
    );
}

#[test]
fn a_memory_whose_claim_was_edited_after_it_was_verified_never_reaches_the_model() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let mut memory = verified(
        &store,
        "the text file the check reads wants ok",
        &["src/ok.txt"],
    );
    assert!(
        memory.evidence_matches_claim(),
        "the evidence was given for this claim"
    );

    // The edit the model would read: new words, and the claim's own hash kept
    // in step with them, as the store's edit does. The evidence stays as it
    // was given, for the words that were there then.
    memory.content = format!("{} It is the only file the check reads.", memory.content);
    memory.claim_hash = claim_hash(&memory.title, &memory.content);
    assert!(
        !memory.evidence_matches_claim(),
        "the evidence was given for the claim as it was, so it no longer backs \
         the claim as it is"
    );
    let run = run_keeping(store, vec![memory], &[], &[]);

    assert!(
        !run.steers(&run.recalled[0]),
        "an edited claim keeps the old evidence, which is no evidence for what \
         it now says, so it does not steer the model"
    );
    assert!(
        !reaches_the_model(&run, &run.recalled[0]),
        "and so the model is not given it"
    );
}

#[test]
fn admit_drops_the_memories_that_name_a_file_the_run_may_not_read() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let good = verified(&store, "the retry is serialized per order", &["src/a.rs"]);
    let shown = good.id;
    let restricted = verified(
        &store,
        "the text file the check reads wants ok",
        &["src/ok.txt"],
    );
    let sealed = verified(
        &store,
        "the writer's test greets by name",
        &["src/greet.test.ts"],
    );
    let run = run_keeping(store, Vec::new(), &["src/ok.txt"], &["src/greet.test.ts"]);

    let admitted = run.admit(vec![restricted, sealed, good]);

    assert_eq!(
        admitted.len(),
        1,
        "the file rule is a filter over the list, not a pass-through, and {} \
         came through",
        admitted.len()
    );
    assert_eq!(
        admitted[0].id, shown,
        "the verified memory about a file the run may read is the one shown"
    );
}

#[test]
fn both_rules_have_to_hold_for_the_model_to_be_given_a_memory() {
    let store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let good = verified(&store, "the retry is serialized per order", &["src/a.rs"]);
    let restricted = verified(
        &store,
        "the text file the check reads wants ok",
        &["src/ok.txt"],
    );
    let provisional = remembered(&store, "the loader is lazy", &["src/b.rs"]);
    let stale = verified(&store, "the queue is bounded", &["src/c.rs"]);
    let stale = store
        .mark_needs_revalidation(stale.id)
        .expect("the store marks a memory to be checked again");
    let run = run_keeping(
        store,
        vec![good, restricted, provisional, stale],
        &["src/ok.txt"],
        &[],
    );

    let reachable: Vec<bool> = run
        .recalled
        .iter()
        .map(|record| reaches_the_model(&run, record))
        .collect();

    assert_eq!(
        reachable,
        vec![true, false, false, false],
        "only the memory whose evidence holds and that names no file the run \
         may not read reaches the model: the one naming the restricted file \
         and the two the evidence does not back do not"
    );
}

#[test]
fn a_read_only_run_never_plans_a_memory_to_be_checked_again() {
    let round = [result("cargo test", "failed", &["src/a.rs"])];

    // The leader's run: the failing check falls in the file the memory it used
    // speaks about, so the claim is asked to be checked again.
    let leader_store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let leader_memory = verified(&leader_store, "the helper is pure", &["src/a.rs"]);
    let leader = run_with(leader_store, vec![leader_memory]);
    assert_eq!(
        leader.check_plan(&round).revalidate.len(),
        1,
        "the same round does plan a memory the leader's run used"
    );

    // A child that may only read: its checks are the leader's to act on.
    let child_store = SharedMemoryStore::open_in_memory().expect("an in-memory store");
    let child_memory = verified(&child_store, "the helper is pure", &["src/a.rs"]);
    let child = RunMemory {
        read_only: true,
        ..run_with(child_store, vec![child_memory])
    };
    let plan = child.check_plan(&round);
    assert!(
        plan.revalidate.is_empty(),
        "a read-only child never plans anything, so it never marks the leader's \
         memory to be checked again, was {:?}",
        plan.revalidate
    );
}

// PR 4: the repair a run writes down — what it says when it has no cause to
// give, and how the memory tells one repair of a failure from another.

/// A repair of `npm test` failing on `failing`, made by changing `fixed`, and
/// proved by a round that passed.
fn repair(failing: &[&str], fixed: &[&str]) -> super::claim::Repair {
    super::claim::Repair::of(
        "npm test",
        &failing
            .iter()
            .map(|key| key.to_string())
            .collect::<Vec<_>>(),
        &fixed
            .iter()
            .map(|file| file.to_string())
            .collect::<Vec<_>>(),
        "npm test passed".to_string(),
    )
}

#[test]
fn a_repair_given_no_cause_says_the_cause_was_not_established() {
    let rendered = repair(&["src/a.rs"], &["src/a.rs"]).render();
    let lines: Vec<&str> = rendered.lines().collect();

    assert!(
        lines[0].starts_with("Problem: "),
        "the repair says what went wrong first: {rendered}"
    );
    assert_eq!(
        lines[1], "Cause: not established by this run — check the code before relying on a cause.",
        "a repair that was given no cause says so rather than inventing one: {rendered}"
    );
    assert!(
        lines[2].starts_with("Fix: "),
        "then what fixed it: {rendered}"
    );
    assert!(
        lines[3].starts_with("Applies to: "),
        "then the files it is about: {rendered}"
    );
    assert!(
        lines[4].starts_with("Validated by: npm test passed"),
        "and last what proved it: {rendered}"
    );
}

#[test]
fn a_repair_that_changed_no_file_says_nothing_about_what_it_applies_to() {
    let rendered = repair(&["src/a.rs"], &[]).render();

    assert!(
        !rendered.contains("Applies to:"),
        "a repair that names no file has nothing to say about what it applies to: {rendered}"
    );
    assert!(
        rendered.contains("Validated by: npm test passed"),
        "it still says what proved it: {rendered}"
    );
}

#[test]
fn two_failures_of_one_command_are_told_apart_by_what_failed() {
    let checkout = super::claim::failure_signature("npm test", &["src/checkout.txt".to_string()]);
    assert_eq!(
        checkout, "npm test|src/checkout.txt",
        "the command first, then what failed"
    );
    assert_eq!(
        super::claim::failure_signature("npm   test ", &[" src/checkout.txt ".to_string()]),
        checkout,
        "spacing is not part of the failure, so the same failure keeps one signature"
    );
    assert_ne!(
        super::claim::failure_signature("npm test", &["src/queue.txt".to_string()]),
        checkout,
        "the same command failing on another file is another failure"
    );

    let unordered = [
        "src/b.rs".to_string(),
        "src/a.rs".to_string(),
        "src/a.rs".to_string(),
    ];
    let sorted = ["src/a.rs".to_string(), "src/b.rs".to_string()];
    assert_eq!(
        super::claim::failure_signature("npm test", &unordered),
        super::claim::failure_signature("npm test", &sorted),
        "the order and the repeats of what failed do not make a second failure"
    );
}

#[test]
fn a_repairs_identity_is_the_failure_and_the_files_it_applies_to() {
    let signature = super::claim::failure_signature("npm test", &["src/a.rs".to_string()]);
    let both = repair(&["src/a.rs"], &["src/b.rs", "src/a.rs"]).identity(&signature);

    assert_eq!(
        both,
        repair(&["src/a.rs"], &["src/a.rs", "src/b.rs", "src/a.rs"]).identity(&signature),
        "the order and the repeats of the files do not make a second repair"
    );
    assert_eq!(
        both,
        format!("repair:{signature}|src/a.rs,src/b.rs"),
        "a repair is the failure it answers and the files it changes, sorted"
    );
    assert_ne!(
        both,
        repair(&["src/a.rs"], &["src/a.rs"]).identity(&signature),
        "a repair that changed another set of files is another repair"
    );
    assert_ne!(
        both,
        repair(&["src/a.rs"], &["src/a.rs"]).identity(&super::claim::failure_signature(
            "npm test",
            &["src/b.rs".to_string()]
        )),
        "and so is a repair of another failure that changed the same files"
    );
}
