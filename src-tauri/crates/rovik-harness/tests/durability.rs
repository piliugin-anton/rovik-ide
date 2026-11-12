use rovik_harness::{ledger::Ledger, state::State};
use std::collections::BTreeMap;

fn init(dir: &std::path::Path) -> Ledger {
    let mut ledger = Ledger::open(dir).unwrap();
    let input = ledger
        .store_json("contract", &serde_json::json!({"request":"fixture"}), 0)
        .unwrap();
    ledger
        .create_run("run", &input.digest, &[("model_calls".into(), 2)].into(), 0)
        .unwrap();
    ledger
}

#[test]
fn exactly_one_of_two_controllers_can_commit_the_same_revision() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let mut a = init(&root);
    let mut b = Ledger::open(&root).unwrap();
    let before = a.run("run").unwrap();
    let competing = b.run("run").unwrap();
    let event = a
        .store_json("event", &serde_json::json!({"type":"baseline"}), 1)
        .unwrap();
    a.transition(&before, State::Baseline, &event.digest, None, 1)
        .unwrap();
    assert!(b
        .transition(&competing, State::Baseline, &event.digest, None, 1)
        .is_err());
    assert_eq!(b.run("run").unwrap().revision, 1);
    let db = rusqlite::Connection::open(root.join("ledger.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(db.execute("DELETE FROM events", []).is_err());
    assert!(db.execute("UPDATE events SET sequence=9", []).is_err());
}

#[test]
fn invalid_transitions_and_model_acceptance_text_cannot_advance_the_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = init(&dir.path().join("state"));
    let before = ledger.run("run").unwrap();
    let text = ledger
        .store("provider-response", "text/plain", b"ACCEPTED", 1)
        .unwrap();
    assert!(ledger
        .transition(&before, State::Accepted, &text.digest, None, 1)
        .is_err());
    assert_eq!(ledger.run("run").unwrap().state, State::Intake);
    let blocked = ledger
        .transition(
            &before,
            State::Blocked,
            &text.digest,
            Some(State::Intake),
            1,
        )
        .unwrap();
    assert!(ledger
        .transition(&blocked, State::CleanCi, &text.digest, None, 2)
        .is_err());
    let resumed = ledger
        .transition(&blocked, State::Intake, &text.digest, None, 2)
        .unwrap();
    let terminal = ledger
        .transition(&resumed, State::Cancelled, &text.digest, None, 3)
        .unwrap();
    assert!(ledger
        .transition(&terminal, State::Intake, &text.digest, None, 4)
        .is_err());
}

#[test]
fn crash_after_blob_publication_has_no_dangling_run_reference() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let ledger = init(&root);
    ledger
        .store_json("event", &serde_json::json!({"uncommitted":"transition"}), 1)
        .unwrap();
    drop(ledger);
    let resumed = Ledger::open(&root).unwrap();
    assert_eq!(resumed.run("run").unwrap().revision, 0);
    resumed.verify_artifacts().unwrap();
}

#[test]
fn a_failed_event_insert_rolls_back_the_state_update() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let mut ledger = init(&root);
    let db = rusqlite::Connection::open(root.join("ledger.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER injected_failure BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'simulated disk failure'); END;").unwrap();
    let before = ledger.run("run").unwrap();
    let event = ledger
        .store_json("event", &serde_json::json!({"stage":"baseline"}), 1)
        .unwrap();
    assert!(ledger
        .transition(&before, State::Baseline, &event.digest, None, 1)
        .is_err());
    assert_eq!(ledger.run("run").unwrap().revision, 0);
    assert_eq!(ledger.run("run").unwrap().state, State::Intake);
    assert_eq!(
        db.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn expired_callbacks_are_fenced_and_leftover_outputs_are_never_promoted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let mut ledger = init(&root);
    let input = ledger.run("run").unwrap().input_digest;
    let reservation: BTreeMap<String, u64> = [("model_calls".into(), 1)].into();
    let lease = ledger
        .claim(
            "run",
            "model:attempt-1",
            "model",
            &input,
            "controller",
            1,
            10,
            &reservation,
        )
        .unwrap();
    assert_eq!(
        ledger
            .claim(
                "run",
                "model:attempt-1",
                "model",
                &input,
                "controller",
                2,
                10,
                &reservation
            )
            .unwrap(),
        lease
    );
    assert_eq!(ledger.budgets("run").unwrap()["model_calls"].reserved, 1);
    drop(ledger);
    let mut resumed = Ledger::open(&root).unwrap();
    let output = resumed
        .store_json(
            "provider-response",
            &serde_json::json!({"claim":"pass"}),
            12,
        )
        .unwrap();
    assert_eq!(
        resumed.recover_expired(12).unwrap(),
        vec![lease.operation_id.clone()]
    );
    assert!(resumed
        .complete(&lease, &output.digest, &reservation, 12)
        .is_err());
    assert_eq!(resumed.run("run").unwrap().state, State::Intake);
    assert!(resumed.verify_lease(&lease, 2).is_err());
}

#[test]
fn actual_usage_over_reservation_is_recorded_and_blocks_further_spending() {
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = init(&dir.path().join("state"));
    let input = ledger.run("run").unwrap().input_digest;
    let lease = ledger
        .claim(
            "run",
            "call:1",
            "model",
            &input,
            "controller",
            1,
            100,
            &[("model_calls".into(), 1)].into(),
        )
        .unwrap();
    let output = ledger
        .store_json("provider-response", &serde_json::json!({"answer":1}), 2)
        .unwrap();
    assert!(!ledger
        .complete(
            &lease,
            &output.digest,
            &[("model_calls".into(), 3)].into(),
            2
        )
        .unwrap());
    let account = &ledger.budgets("run").unwrap()["model_calls"];
    assert_eq!((account.spent, account.reserved), (3, 0));
    assert!(ledger
        .claim(
            "run",
            "call:2",
            "model",
            &input,
            "controller",
            3,
            100,
            &[("model_calls".into(), 1)].into()
        )
        .is_err());
    assert!(ledger
        .complete(
            &lease,
            &output.digest,
            &[("model_calls".into(), 3)].into(),
            3
        )
        .is_err());
}

#[test]
fn failed_reservations_roll_back_all_units_and_unknown_migrations_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let mut ledger = init(&root);
    let input = ledger.run("run").unwrap().input_digest;
    assert!(ledger
        .claim(
            "run",
            "call:1",
            "model",
            &input,
            "controller",
            1,
            100,
            &[("model_calls".into(), 1), ("unknown".into(), 1)].into()
        )
        .is_err());
    assert_eq!(ledger.budgets("run").unwrap()["model_calls"].reserved, 0);
    drop(ledger);
    let db = rusqlite::Connection::open(root.join("ledger.sqlite3")).unwrap();
    db.execute_batch("PRAGMA user_version=99;").unwrap();
    drop(db);
    assert!(Ledger::open(&root).is_err());
}
