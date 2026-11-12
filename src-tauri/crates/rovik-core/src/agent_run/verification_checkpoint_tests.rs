use super::*;
use crate::agent_run::{
    project::ProjectCheck,
    recovery::{after_crash, Recovery},
    subagent::{Original, Source},
};
use crate::chat_db::store::{self, NewDialog};
use crate::core::Core;
use crate::testing::{Recorder, TestCore};
use std::sync::Arc;

fn run(core: &Core, dialog: &str, root: &Path, reuse: bool) -> Run {
    let mut config = crate::agent_run::AgentConfig::default();
    config.permissions.allow_any_command = true;
    core.agents.set_config(config);
    let controls = core.agents.register(dialog, "test", None).unwrap();
    let profile = serde_json::from_value(json!({
        "id":"test", "label":"Test", "model":"test", "baseUrl":"http://localhost"
    }))
    .unwrap();
    let mut run = Run::blank(
        core.clone(),
        crate::file_guard::Seen::new(dialog),
        dialog.into(),
        profile,
        None,
        controls,
        crate::agent_run::run::Role::Lead,
    );
    run.root = Some(root.to_string_lossy().into());
    run.project_root = run.root.clone();
    run.checks = vec![ProjectCheck {
        kind: "lint".into(),
        command: "printf checkpoint-pass".into(),
        enabled: true,
        reuse_success: reuse,
    }];
    run.changed_files = vec!["a.txt".into()];
    run.verification.writes_since_verify = true;
    run.verification.carry.baseline.insert(
        "a.txt".into(),
        Original::new(Some(b"before".to_vec()), Source::ViewAtRunStart),
    );
    run.seen.record(&root.join("a.txt"), b"after");
    run
}

async fn fixture(reuse: bool) -> (TestCore, Run) {
    let host = TestCore::new();
    let root = host.dir.0.join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.txt"), "after").unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let dialog = NewDialog {
        id: id.clone(),
        title: "Checkpoint".into(),
        created_at: 0,
        updated_at: 0,
        project_id: None,
    };
    host.core
        .db
        .write(move |db| store::create_dialog(db, &dialog))
        .await
        .unwrap();
    let run = run(&host.core, &id, &root, reuse);
    (host, run)
}

fn reopened(host: &TestCore) -> Core {
    Core::open(
        host.core.paths.clone(),
        Arc::new(Recorder(host.events.clone())),
    )
    .unwrap()
}

async fn record(run: &Run, status: &str) -> String {
    run.add_record(super::super::record(
        status,
        1,
        &[],
        &run.changed_files,
        None,
    ))
    .await
    .unwrap()
}

async fn checks(run: &mut Run, record: &str) -> Result<Vec<CheckResult>, String> {
    let root = run.root.clone().unwrap();
    let checks = run.checks.clone();
    let changed = run.changed_files.clone();
    run.run_checks(
        &root,
        &checks,
        &run.config().permissions,
        &changed,
        record,
        1,
        "restart-test",
        false,
    )
    .await
}

#[tokio::test]
async fn completed_opt_in_check_survives_a_new_core_and_reopened_database() {
    let (host, mut original) = fixture(true).await;
    let dialog = original.dialog_id.clone();
    let root = original.root.clone().unwrap();
    let id = record(&original, "running").await;
    INTERRUPT_AT
        .lock()
        .unwrap()
        .insert(dialog.clone(), Phase::CheckCompleted);
    assert!(checks(&mut original, &id)
        .await
        .unwrap_err()
        .contains("injected"));
    let saved = read(&directory(&host.core.paths.data_dir), &dialog)
        .unwrap()
        .unwrap();
    assert_eq!(saved.results[0].status, "passed");
    assert_eq!(saved.receipts.len(), 1);
    drop(original);
    let core = reopened(&host);
    assert_eq!(after_crash(&core, &dialog).await.unwrap(), Recovery::Paused);
    assert!(!core.agents.has_carry(&dialog));
    let mut fresh = run(&core, &dialog, Path::new(&root), true);
    fresh.restore_verify_state(&id).await;
    assert!(
        fresh.verification.needs_checks(),
        "a restored result alone never verifies the run"
    );
    let results = checks(&mut fresh, &id).await.unwrap();
    assert!(
        results[0]
            .note
            .as_deref()
            .is_some_and(|s| s.contains("Reused unchanged")),
        "{results:?}"
    );
    assert_eq!(results[0].duration_ms, Some(0));
    assert_eq!(
        fresh.verification.carry.baseline["a.txt"].bytes.as_deref(),
        Some(b"before".as_slice())
    );
}

#[tokio::test]
async fn completed_default_checks_run_again_after_restart() {
    let (host, mut original) = fixture(false).await;
    let dialog = original.dialog_id.clone();
    let root = original.root.clone().unwrap();
    let id = record(&original, "running").await;
    INTERRUPT_AT
        .lock()
        .unwrap()
        .insert(dialog.clone(), Phase::CheckCompleted);
    assert!(checks(&mut original, &id).await.is_err());
    drop(original);
    let core = reopened(&host);
    let mut fresh = run(&core, &dialog, Path::new(&root), false);
    fresh.restore_verify_state(&id).await;
    let results = checks(&mut fresh, &id).await.unwrap();
    assert_eq!(results[0].status, "passed");
    assert!(!results[0]
        .note
        .as_deref()
        .is_some_and(|s| s.contains("Reused")));
    assert!(fresh.verification.receipts.is_empty());
}

#[tokio::test]
async fn unknown_command_boundary_is_paused_for_approval_without_replay() {
    let (host, mut original) = fixture(false).await;
    let marker = host.dir.0.join("must-not-run");
    original.checks[0].command = format!("printf ran > '{}'", marker.display());
    let dialog = original.dialog_id.clone();
    let id = record(&original, "running").await;
    INTERRUPT_AT
        .lock()
        .unwrap()
        .insert(dialog.clone(), Phase::CommandRunning);
    assert!(checks(&mut original, &id)
        .await
        .unwrap_err()
        .contains("injected"));
    drop(original);
    let core = reopened(&host);
    assert_eq!(after_crash(&core, &dialog).await.unwrap(), Recovery::Paused);
    assert!(!marker.exists());
    let rows = crate::agent_run::run::recent_messages(&core.db, &dialog)
        .await
        .unwrap();
    let v = &rows.last().unwrap().meta.as_ref().unwrap()["verification"];
    assert_eq!(v["status"], "needs-approval");
    assert!(v["reason"].as_str().unwrap().contains("outcome is unknown"));
}

#[tokio::test]
async fn changed_tree_drops_reuse_and_rollback_without_erasing_repair_history() {
    let (host, mut original) = fixture(true).await;
    let dialog = original.dialog_id.clone();
    let root = original.root.clone().unwrap();
    original
        .verification
        .carry
        .verify_history
        .push(Default::default());
    original
        .verification
        .carry
        .integrity_open
        .push("weakened test".into());
    original.verification.carry.checkpoint = Some(Default::default());
    let id = record(&original, "running").await;
    checks(&mut original, &id).await.unwrap();
    drop(original);
    std::fs::write(Path::new(&root).join("a.txt"), "outside edit").unwrap();
    let core = reopened(&host);
    let mut fresh = run(&core, &dialog, Path::new(&root), true);
    fresh.restore_verify_state(&id).await;
    assert_eq!(fresh.verification.carry.verify_history.len(), 1);
    assert_eq!(fresh.verification.carry.integrity_open, ["weakened test"]);
    assert!(fresh.verification.carry.checkpoint.is_none());
    assert!(fresh.verification.integrity_unchecked);
    assert!(fresh.verification.receipts.is_empty());
    let results = checks(&mut fresh, &id).await.unwrap();
    assert!(!results[0]
        .note
        .as_deref()
        .is_some_and(|s| s.contains("Reused")));
}

#[tokio::test]
async fn changed_check_configuration_requires_new_approval_and_keeps_consumed_budget() {
    let (host, mut original) = fixture(false).await;
    let dialog = original.dialog_id.clone();
    let root = original.root.clone().unwrap();
    original.verification.verify_rounds = 2;
    original.verification.carry.bonus_rounds = 1;
    original.ledger.tokens = 700;
    original
        .core
        .agents
        .restore_request_spending(&dialog, 900, 700);
    let id = record(&original, "needs-approval").await;
    original
        .checkpoint_verification(&id, Phase::AwaitingApproval, &[])
        .await;
    drop(original);
    let core = reopened(&host);
    let mut fresh = run(&core, &dialog, Path::new(&root), false);
    fresh.checks[0].command = "printf newly-configured".into();
    fresh.restore_verify_state(&id).await;
    assert!(fresh.verification.approval_changed);
    assert_eq!(fresh.ledger.tokens, 700);
    assert_eq!(fresh.core.agents.request_spending(&dialog), (900, 700));
    assert_eq!(fresh.verification.verify_rounds, 2);
    assert_eq!(fresh.verification.carry.bonus_rounds, 1);
    assert_eq!(
        fresh.verify_at_end(true, Some(id)).await.unwrap(),
        "verify-needs-approval"
    );
    let rows = crate::agent_run::run::recent_messages(&core.db, &dialog)
        .await
        .unwrap();
    assert_eq!(
        rows.last().unwrap().meta.as_ref().unwrap()["verification"]["checks"][0]["command"],
        "printf newly-configured"
    );
}

#[tokio::test]
async fn corrupt_checkpoint_requires_fresh_approval_and_preserves_incomplete_integrity() {
    let (host, original) = fixture(false).await;
    let dialog = original.dialog_id.clone();
    let root = original.root.clone().unwrap();
    let id = record(&original, "running").await;
    let dir = directory(&host.core.paths.data_dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(path(&dir, &dialog), b"not json").unwrap();
    drop(original);
    let core = reopened(&host);
    assert_eq!(after_crash(&core, &dialog).await.unwrap(), Recovery::Paused);
    let mut fresh = run(&core, &dialog, Path::new(&root), false);
    fresh.restore_verify_state(&id).await;
    assert!(fresh.verification.approval_changed);
    fresh.rebuild_baseline(&id).await;
    assert!(fresh.verification.integrity_unchecked);
    assert!(fresh
        .verification
        .checkpoint_notice
        .as_deref()
        .unwrap()
        .contains("corrupt"));
}

#[tokio::test]
async fn held_review_findings_and_progress_are_retained_without_invented_answers() {
    let (host, mut original) = fixture(false).await;
    let dialog = original.dialog_id.clone();
    let root = original.root.clone().unwrap();
    let mut review = serde_json::to_value(&original.review).unwrap();
    review["review_held"] = json!([{"severity":"high","location":"a.txt:1","path":"a.txt","line":1,
        "title":"Unresolved","scenario":"Existing request fails","fix":"Handle the failure","confidence":"confirmed"}]);
    review["review_blocked"] = json!(true);
    review["carry"]["reviews_total"] = json!(2);
    original.review = serde_json::from_value(review.clone()).unwrap();
    let id = record(&original, "needs-approval").await;
    original
        .checkpoint_verification(&id, Phase::AwaitingApproval, &[])
        .await;
    drop(original);
    let core = reopened(&host);
    let mut fresh = run(&core, &dialog, Path::new(&root), false);
    fresh.restore_verify_state(&id).await;
    assert_eq!(serde_json::to_value(&fresh.review).unwrap(), review);
    assert!(fresh.review.has_held_findings());
    assert_eq!(fresh.review.total(), 2);
    assert!(fresh.verification.needs_checks());
}

#[tokio::test]
async fn dropping_carry_deletes_private_checkpoint_across_a_new_core() {
    let (host, mut original) = fixture(false).await;
    let dialog = original.dialog_id.clone();
    let id = record(&original, "needs-approval").await;
    original
        .checkpoint_verification(&id, Phase::AwaitingApproval, &[])
        .await;
    let file = path(&directory(&host.core.paths.data_dir), &dialog);
    assert!(file.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(file.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    drop(original);
    let core = reopened(&host);
    core.agents.drop_carry(&dialog);
    assert!(!file.exists());
    assert_eq!(
        after_crash(&core, &dialog).await.unwrap(),
        Recovery::Interrupted
    );
}

#[tokio::test]
async fn process_interruption_during_an_actual_command_never_replays_it_on_recovery() {
    let (host, mut original) = fixture(false).await;
    let marker = host.dir.0.join("command-started");
    original.checks[0].command = format!("printf x >> '{}'; sleep 30", marker.display());
    let dialog = original.dialog_id.clone();
    let id = record(&original, "running").await;
    {
        let future = checks(&mut original, &id);
        tokio::pin!(future);
        let started = async {
            while !marker.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            result = &mut future => panic!("command ended before interruption: {result:?}"),
            seen = tokio::time::timeout(std::time::Duration::from_secs(10), started) => seen.unwrap(),
        }
        // Dropping the pending command future models losing the process after
        // dispatch, before its result is confirmed. Recovery cannot know whether
        // its external effects happened and must not execute it automatically.
    }
    let saved = read(&directory(&host.core.paths.data_dir), &dialog)
        .unwrap()
        .unwrap();
    assert_eq!(saved.phase, Phase::CommandRunning);
    drop(original);
    let core = reopened(&host);
    assert_eq!(after_crash(&core, &dialog).await.unwrap(), Recovery::Paused);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
}

#[test]
fn checkpoint_storage_evicts_old_records_and_stays_bounded_across_chats() {
    let dir = crate::testing::temp_dir();
    for i in 0..MAX_STORE_FILES + 2 {
        let file = dir.0.join(format!("{i}.json"));
        make_room(&dir.0, &file, 1).unwrap();
        std::fs::write(file, "x").unwrap();
    }
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), MAX_STORE_FILES);
}

#[tokio::test]
async fn crash_after_judgement_resumes_with_history_before_the_final_acceptance_gate() {
    let (host, mut original) = fixture(true).await;
    let dialog = original.dialog_id.clone();
    let root = original.root.clone().unwrap();
    let id = record(&original, "running").await;
    INTERRUPT_AT
        .lock()
        .unwrap()
        .insert(dialog.clone(), Phase::FinalizationPending);
    assert!(original
        .verify_at_end(true, Some(id.clone()))
        .await
        .unwrap_err()
        .contains("injected"));
    let saved = read(&directory(&host.core.paths.data_dir), &dialog)
        .unwrap()
        .unwrap();
    assert_eq!(saved.phase, Phase::FinalizationPending);
    assert_eq!(saved.rounds.verify_history.len(), 1);
    assert_eq!(saved.rounds.last_verify_results[0].status, "passed");
    drop(original);
    let core = reopened(&host);
    assert_eq!(after_crash(&core, &dialog).await.unwrap(), Recovery::Paused);
    let mut fresh = run(&core, &dialog, Path::new(&root), true);
    fresh.restore_verify_state(&id).await;
    assert_eq!(fresh.verification.carry.verify_history.len(), 1);
    assert!(
        fresh.verification.needs_checks(),
        "restored passing commands must enter the complete gate again"
    );
    let rows = crate::agent_run::run::recent_messages(&core.db, &dialog)
        .await
        .unwrap();
    assert_eq!(
        rows.last().unwrap().meta.as_ref().unwrap()["verification"]["status"],
        "needs-approval"
    );
}

#[tokio::test]
async fn pending_finalization_can_restart_checks_without_usable_checkpoint_state() {
    for status in ["passed", "failed"] {
        for unavailable in ["missing", "expired", "corrupt", "different-record"] {
            let (host, mut original) = fixture(false).await;
            let dialog = original.dialog_id.clone();
            let id = original
                .add_record(super::super::with(
                    super::super::record(status, 1, &[], &original.changed_files, None),
                    json!({"finalizationPending": true}),
                ))
                .await
                .unwrap();
            let saved_id = if unavailable == "different-record" {
                "older-record"
            } else {
                &id
            };
            original
                .checkpoint_verification(saved_id, Phase::FinalizationPending, &[])
                .await;
            let dir = directory(&host.core.paths.data_dir);
            match unavailable {
                "missing" => remove(&dir, &dialog),
                "expired" => age_for_test(&dir, &dialog, super::super::CARRY_TTL),
                "corrupt" => std::fs::write(path(&dir, &dialog), b"invalid JSON").unwrap(),
                _ => {}
            }
            drop(original);
            let core = reopened(&host);
            assert_eq!(
                after_crash(&core, &dialog).await.unwrap(),
                Recovery::Paused,
                "{status} with {unavailable} checkpoint must still allow fresh checks"
            );
            let rows = crate::agent_run::run::recent_messages(&core.db, &dialog)
                .await
                .unwrap();
            let latest = rows.last().unwrap();
            assert_eq!(latest.id, id);
            assert_eq!(
                latest.meta.as_ref().unwrap()["verification"]["status"],
                "needs-approval"
            );
        }
    }
}

#[tokio::test]
async fn another_approval_keeps_incomplete_integrity_and_full_review_state_in_memory() {
    let (host, mut original) = fixture(false).await;
    let dialog = original.dialog_id.clone();
    original.verification.integrity_unchecked = true;
    original.verification.stalled_rounds = 1;
    let mut review = serde_json::to_value(&original.review).unwrap();
    review["review_blocked"] = json!(true);
    review["review_answer_nudges"] = json!(2);
    original.review = serde_json::from_value(review.clone()).unwrap();
    let carry = original.take_verify_state("waiting".into());
    host.core.agents.put_carry(&dialog, carry);
    original.restore_verify_state("waiting").await;
    assert!(original.verification.integrity_unchecked);
    assert_eq!(original.verification.stalled_rounds, 1);
    assert_eq!(serde_json::to_value(&original.review).unwrap(), review);
}

#[tokio::test]
async fn run_checks_entry_after_restart_cannot_bypass_restored_review_findings() {
    for recover_first in [true, false] {
        let (host, mut original) = fixture(true).await;
        let dialog = original.dialog_id.clone();
        let root = original.root.clone().unwrap();
        let checks = original.checks.clone();
        let config = original.config();
        crate::project_profile::save_project_settings(
            &host.core.paths.config_dir,
            root.clone(),
            json!({"checks": checks}),
        )
        .unwrap();
        let mut review = serde_json::to_value(&original.review).unwrap();
        review["review_held"] = json!([{"severity":"high","location":"a.txt:1","path":"a.txt","line":1,
        "title":"Unresolved","scenario":"Existing request fails","fix":"Handle the failure","confidence":"confirmed"}]);
        review["review_blocked"] = json!(true);
        original.review = serde_json::from_value(review).unwrap();
        let id = record(&original, "running").await;
        INTERRUPT_AT
            .lock()
            .unwrap()
            .insert(dialog.clone(), Phase::FinalizationPending);
        assert!(original
            .verify_at_end(true, Some(id.clone()))
            .await
            .unwrap_err()
            .contains("injected"));
        drop(original);
        let core = reopened(&host);
        if recover_first {
            assert_eq!(after_crash(&core, &dialog).await.unwrap(), Recovery::Paused);
        }
        let (url, requests) =
            crate::testing::model(vec![crate::testing::Reply::Text("Done.")]).await;
        let request: crate::agent_run::StartRequest = serde_json::from_value(json!({
            "dialogId": dialog, "start": {"kind":"runChecks", "recordId": id},
            "profile": {"id":"test", "label":"Test", "model":"test", "baseUrl":url},
            "mode":"agent", "contextMode":"trimming", "root":root, "config":config,
            "isolate":false, "projectSettings":{"checks": checks}
        }))
        .unwrap();
        assert!(crate::agent_run::start_run(&core, request)
            .await
            .unwrap()
            .is_some());
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let Some(outcome) = host
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|e| e["type"] == "runEnded")
                    .and_then(|e| e["outcome"].as_str())
                    .map(str::to_string)
                {
                    break outcome;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(outcome, "review-open");
        let rows = crate::agent_run::run::recent_messages(&core.db, &dialog)
            .await
            .unwrap();
        let settled = rows
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .meta
            .as_ref()
            .unwrap();
        assert_eq!(settled["verification"]["finalizationPending"], false);
        assert_eq!(
            after_crash(&core, &dialog).await.unwrap(),
            Recovery::Interrupted
        );
        assert!(
            requests.lock().unwrap().is_empty(),
            "a restored held finding cannot be silently accepted by the lead"
        );
        assert!(
            !path(&directory(&core.paths.data_dir), &dialog).exists(),
            "finished runs release checkpoint storage"
        );
    }
}

#[tokio::test]
async fn another_workspace_keeps_budget_but_never_restores_old_worker_routes() {
    for durable in [false, true] {
        let (host, mut original) = fixture(false).await;
        let dialog = original.dialog_id.clone();
        original
            .verification
            .carry
            .groups
            .push(crate::agent_run::repair_run::GroupRecord {
                name: "old workspace".into(),
                owners: Vec::new(),
                children: vec![("old worker".into(), 1, "old-child-chat".into())],
                contract: Vec::new(),
            });
        original
            .verification
            .carry
            .worker_steps
            .push(crate::agent_run::worker::DoneStep {
                number: 1,
                title: "old step".into(),
                summary: "old workspace".into(),
                decisions: Vec::new(),
                light: None,
            });
        original.verification.carry.last_triage = Some(crate::agent_run::triage::Triage {
            check: "lint".into(),
            root_cause: "old workspace".into(),
            files: vec!["a.txt".into()],
            next_step: "fix old worker".into(),
            confidence: "high".into(),
            confidence_note: String::new(),
            other_causes: Vec::new(),
        });
        original.ledger.tokens = 40;
        original
            .core
            .agents
            .restore_request_spending(&dialog, 100, 40);
        let id = record(&original, "needs-approval").await;
        original
            .checkpoint_verification(&id, Phase::AwaitingApproval, &[])
            .await;
        let changed_root = host.dir.0.join("another-project");
        std::fs::create_dir(&changed_root).unwrap();
        std::fs::write(changed_root.join("a.txt"), "after").unwrap();
        let mut fresh = if durable {
            drop(original);
            let core = reopened(&host);
            run(&core, &dialog, &changed_root, false)
        } else {
            let carry = original.take_verify_state(id.clone());
            original.core.agents.put_carry(&dialog, carry);
            original.root = Some(changed_root.to_string_lossy().into());
            original.project_root = original.root.clone();
            original
        };
        fresh.restore_verify_state(&id).await;
        assert!(fresh.verification.approval_changed);
        assert!(fresh.verification.integrity_unchecked);
        assert!(fresh.verification.carry.groups.is_empty());
        assert!(fresh.verification.carry.worker_steps.is_empty());
        assert!(fresh.verification.carry.last_triage.is_none());
        assert_eq!(fresh.ledger.tokens, 40);
        assert_eq!(fresh.core.agents.request_spending(&dialog), (100, 40));
    }
}

#[tokio::test]
async fn expired_state_is_dropped_without_reasking_for_unchanged_checks_just_approved() {
    let (host, mut original) = fixture(false).await;
    let dialog = original.dialog_id.clone();
    let root = original.root.clone().unwrap();
    let id = record(&original, "needs-approval").await;
    original
        .checkpoint_verification(&id, Phase::AwaitingApproval, &[])
        .await;
    age_for_test(
        &directory(&host.core.paths.data_dir),
        &dialog,
        super::super::CARRY_TTL,
    );
    assert!(matches!(
        read(&directory(&host.core.paths.data_dir), &dialog),
        Err(ReadError::Expired)
    ));
    drop(original);
    let core = reopened(&host);
    let mut fresh = run(&core, &dialog, Path::new(&root), false);
    fresh.restore_verify_state(&id).await;
    assert_eq!(fresh.verification.carry_lost.as_deref(), Some(id.as_str()));
    assert!(!fresh.verification.approval_changed);
    assert!(fresh
        .verification
        .checkpoint_notice
        .as_deref()
        .unwrap()
        .contains("expired"));
    fresh.rebuild_baseline(&id).await;
    assert!(
        fresh.verification.integrity_unchecked,
        "missing originals are still incomplete even with approval"
    );
}
