use verified_code_memory::{
    claim_hash, Applicability, MemoryStore, NewMemory, RetrievalQuery, Verification,
};

fn store() -> MemoryStore {
    MemoryStore::open_in_memory().unwrap()
}
fn exchange(a: &MemoryStore, b: &MemoryStore) {
    b.sync_apply("cloud", &a.sync_export("desktop", 0).unwrap())
        .unwrap();
    a.sync_apply("desktop", &b.sync_export("cloud", 0).unwrap())
        .unwrap();
}

#[test]
fn records_keep_ids_sources_and_evidence_across_scopes_and_retries() {
    let (a, b) = (store(), store());
    let record = a
        .remember(
            NewMemory::procedure("desktop", "Retry queue", "Serialize the retry queue")
                .with_files(["src/retry.rs"]),
        )
        .unwrap();
    let proven = a.verify(record.id, "retry regression passed").unwrap();
    exchange(&a, &b);
    exchange(&a, &b);
    let copied = b.get(record.id).unwrap();
    assert_eq!(copied.repo, "cloud");
    assert_eq!(copied.evidence, proven.evidence);
    assert_eq!(copied.validated_claim_hash, proven.validated_claim_hash);
    assert_eq!(copied.verification, Verification::Verified);
    assert_eq!(b.list("cloud", "").unwrap().len(), 1);
    assert_eq!(
        b.retrieve(&RetrievalQuery::new("cloud", "retry queue"))
            .unwrap()[0]
            .record
            .id,
        record.id
    );
    assert!(b
        .retrieve(&RetrievalQuery::new("other", "retry queue"))
        .unwrap()
        .is_empty());
    assert!(a
        .sync_export("desktop", a.sync_export("desktop", 0).unwrap().through)
        .unwrap()
        .changes
        .is_empty());
}

#[test]
fn concurrent_edits_converge_without_inheriting_the_old_claims_trust() {
    let (a, b) = (store(), store());
    let r = a
        .remember(NewMemory::project_fact(
            "desktop",
            "Queue",
            "The queue is serial",
        ))
        .unwrap();
    a.verify(r.id, "serial test passed").unwrap();
    exchange(&a, &b);
    a.edit(r.id, "Queue", "Use per-project locks").unwrap();
    b.edit(r.id, "Queue", "Use per-account locks").unwrap();
    let left = a.sync_export("desktop", 0).unwrap();
    let right = b.sync_export("cloud", 0).unwrap();
    a.sync_apply("desktop", &right).unwrap();
    b.sync_apply("cloud", &left).unwrap();
    let ar = a.get(r.id).unwrap();
    let br = b.get(r.id).unwrap();
    assert_eq!(ar.content, br.content);
    assert_eq!(ar.applicability, Applicability::NeedsRevalidation);
    assert!(!ar.evidence_matches_claim());
    assert!(!br.evidence_matches_claim());
}

#[test]
fn deletion_wins_over_a_concurrent_edit_and_old_pages_cannot_resurrect_it() {
    let (a, b) = (store(), store());
    let r = a
        .remember(NewMemory::project_fact("desktop", "Queue", "Serial queue"))
        .unwrap();
    exchange(&a, &b);
    let old = a.sync_export("desktop", 0).unwrap();
    a.delete(r.id).unwrap();
    b.edit(r.id, "Queue", "Offline correction").unwrap();
    exchange(&a, &b);
    a.sync_apply("desktop", &old).unwrap();
    b.sync_apply("cloud", &old).unwrap();
    assert!(a.get(r.id).is_err());
    assert!(b.get(r.id).is_err());
    assert!(a.sync_export("desktop", 0).unwrap().changes[0].deleted);
}

#[test]
fn clear_discards_unseen_offline_history_and_rejects_writers_from_before_it() {
    let (a, b) = (store(), store());
    a.remember(NewMemory::project_fact("desktop", "Old", "Old fact"))
        .unwrap();
    exchange(&a, &b);
    let generation = b.generation("cloud").unwrap();
    let unseen = b
        .remember(NewMemory::project_fact(
            "cloud",
            "Offline",
            "Unseen old history",
        ))
        .unwrap();
    let stale = b.sync_export("cloud", 0).unwrap();
    a.clear("desktop").unwrap();
    exchange(&a, &b);
    a.sync_apply("desktop", &stale).unwrap();
    assert!(a.list("desktop", "").unwrap().is_empty());
    assert!(b.get(unseen.id).is_err());
    assert!(b
        .remember_if_current(
            NewMemory::project_fact("cloud", "Late", "Late old run"),
            generation
        )
        .unwrap()
        .is_none());
    b.remember(NewMemory::project_fact("cloud", "New", "After clear"))
        .unwrap();
    exchange(&a, &b);
    assert_eq!(a.list("desktop", "").unwrap().len(), 1);
}

#[test]
fn concurrent_clears_converge_and_an_empty_clear_still_replicates() {
    let (a, b) = (store(), store());
    a.clear("desktop").unwrap();
    b.clear("cloud").unwrap();
    a.remember(NewMemory::project_fact("desktop", "A", "After A's clear"))
        .unwrap();
    b.remember(NewMemory::project_fact("cloud", "B", "After B's clear"))
        .unwrap();
    let left = a.sync_export("desktop", 0).unwrap();
    let right = b.sync_export("cloud", 0).unwrap();
    a.sync_apply("desktop", &right).unwrap();
    b.sync_apply("cloud", &left).unwrap();
    exchange(&a, &b);
    assert_eq!(
        a.sync_export("desktop", 0).unwrap().epoch,
        b.sync_export("cloud", 0).unwrap().epoch
    );
    assert_eq!(a.list("desktop", "").unwrap().len(), 1);
    assert_eq!(b.list("cloud", "").unwrap().len(), 1);
}

#[test]
fn pagination_refills_indexes_and_does_not_miss_records_changed_between_pages() {
    let (a, b) = (store(), store());
    for n in 0..140 {
        a.remember(NewMemory::project_fact(
            "desktop",
            format!("Queue {n}"),
            "Serial queue",
        ))
        .unwrap();
    }
    let first = a.sync_export("desktop", 0).unwrap();
    assert_eq!(first.changes.len(), 64);
    assert!(first.more);
    b.sync_apply("cloud", &first).unwrap();
    a.edit(first.changes[0].id, "Queue revised", "Newest queue")
        .unwrap();
    let mut after = first.through;
    loop {
        let page = a.sync_export("desktop", after).unwrap();
        b.sync_apply("cloud", &page).unwrap();
        after = page.through;
        if !page.more {
            break;
        }
    }
    assert_eq!(b.list("cloud", "").unwrap().len(), 140);
    assert_eq!(b.get(first.changes[0].id).unwrap().content, "Newest queue");
}

#[test]
fn malformed_pages_and_cross_project_id_collisions_roll_back_the_entire_merge() {
    let (a, b) = (store(), store());
    let r = a
        .remember(NewMemory::project_fact("desktop", "Queue", "Serial queue"))
        .unwrap();
    let mut page = a.sync_export("desktop", 0).unwrap();
    page.changes[0].record.as_mut().unwrap().claim_hash = "forged".into();
    assert!(b.sync_apply("cloud", &page).is_err());
    assert!(b.list("cloud", "").unwrap().is_empty());
    let page = a.sync_export("desktop", 0).unwrap();
    b.sync_apply("other", &page).unwrap();
    a.clear("desktop").unwrap();
    let mut reset = a.sync_export("desktop", 0).unwrap();
    reset.changes = page.changes.clone();
    assert!(b.sync_apply("cloud", &reset).is_err());
    assert_eq!(b.get(r.id).unwrap().repo, "other");
    assert_eq!(b.sync_export("cloud", 0).unwrap().epoch.counter, 0);
    // The failed import must not leave the local journaling switch disabled.
    let new = b
        .remember(NewMemory::project_fact("cloud", "New", "New fact"))
        .unwrap();
    assert_eq!(b.sync_export("cloud", 0).unwrap().changes[0].id, new.id);
}

#[test]
fn progress_and_tombstones_survive_reopening_and_a_replaced_peer_resets_cursors() {
    use verified_code_memory::sync::Progress;
    let path = std::env::temp_dir().join(format!("memory-sync-{}.db", uuid::Uuid::new_v4()));
    let replica;
    {
        let a = MemoryStore::open(&path).unwrap();
        replica = a.sync_replica().unwrap();
        let r = a
            .remember(NewMemory::project_fact("repo", "Old", "Deleted fact"))
            .unwrap();
        a.delete(r.id).unwrap();
        a.sync_ack(
            "repo",
            "host",
            &Progress {
                replica: replica.clone(),
                sent: 12,
                received: 24,
            },
        )
        .unwrap();
    }
    {
        let a = MemoryStore::open(&path).unwrap();
        assert_eq!(a.sync_replica().unwrap(), replica);
        assert!(a.sync_export("repo", 0).unwrap().changes[0].deleted);
        assert_eq!(a.sync_progress("repo", "host").unwrap().received, 24);
        a.sync_ack(
            "repo",
            "host",
            &Progress {
                replica: uuid::Uuid::new_v4().to_string(),
                sent: 0,
                received: 0,
            },
        )
        .unwrap();
        assert_eq!(a.sync_progress("repo", "host").unwrap().sent, 0);
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn retrieval_alone_does_not_create_sync_changes_and_scope_paths_stay_off_the_wire() {
    let a = store();
    let r = a
        .remember(NewMemory::project_fact(
            "/private/local/root",
            "Queue",
            "Serial queue",
        ))
        .unwrap();
    let page = a.sync_export("/private/local/root", 0).unwrap();
    a.record_retrieval(&[r.id]).unwrap();
    assert!(a
        .sync_export("/private/local/root", page.through)
        .unwrap()
        .changes
        .is_empty());
    assert!(!serde_json::to_string(&page)
        .unwrap()
        .contains("/private/local/root"));
    assert_eq!(r.claim_hash, claim_hash(&r.title, &r.content));
}

#[test]
fn late_verification_of_the_old_claim_cannot_overwrite_a_users_offline_correction() {
    let (a, b) = (store(), store());
    let r = a
        .remember(NewMemory::project_fact(
            "desktop",
            "Queue",
            "Old queue claim",
        ))
        .unwrap();
    a.verify(r.id, "old check passed").unwrap();
    exchange(&a, &b);
    a.edit(r.id, "Queue", "Corrected queue claim").unwrap();
    // A busy offline host can have a higher database clock without having
    // seen this claim's edit. Its old evidence must not replace the new text.
    for n in 0..8 {
        b.remember(NewMemory::project_fact(
            "cloud",
            format!("Unrelated {n}"),
            "Other work",
        ))
        .unwrap();
    }
    b.verify(r.id, "late check of the old claim").unwrap();
    exchange(&a, &b);
    for memory in [a.get(r.id).unwrap(), b.get(r.id).unwrap()] {
        assert_eq!(memory.content, "Corrected queue claim");
        assert_eq!(memory.applicability, Applicability::NeedsRevalidation);
        assert!(!memory.evidence_matches_claim());
    }
}

#[test]
fn pages_are_bounded_by_encoded_bytes_and_oversized_records_are_reported_without_truncation() {
    use verified_code_memory::sync::MAX_PAGE_BYTES;
    let (a, b) = (store(), store());
    for title in ["First large claim", "Second large claim"] {
        a.remember(NewMemory::project_fact(
            "desktop",
            title,
            "x".repeat(300_000),
        ))
        .unwrap();
    }
    let first = a.sync_export("desktop", 0).unwrap();
    assert!(first.more);
    assert_eq!(first.changes.len(), 1);
    assert!(serde_json::to_vec(&first).unwrap().len() <= MAX_PAGE_BYTES);
    b.sync_apply("cloud", &first).unwrap();
    let second = a.sync_export("desktop", first.through).unwrap();
    assert!(!second.more);
    b.sync_apply("cloud", &second).unwrap();
    assert_eq!(b.list("cloud", "").unwrap().len(), 2);
    let oversized = a
        .remember(NewMemory::project_fact(
            "huge",
            "Oversized",
            "x".repeat(MAX_PAGE_BYTES),
        ))
        .unwrap();
    assert!(a.sync_export("huge", 0).is_err());
    assert_eq!(a.get(oversized.id).unwrap().content.len(), MAX_PAGE_BYTES);
    let mut malformed = first;
    malformed.changes[0].record.as_mut().unwrap().files = vec!["../private.key".into()];
    assert!(b.sync_apply("another", &malformed).is_err());
    assert!(b.list("another", "").unwrap().is_empty());
}

#[test]
fn upgrading_a_schema_three_store_preserves_history_and_blocks_old_writers_on_reopen() {
    let path =
        std::env::temp_dir().join(format!("memory-sync-upgrade-{}.db", uuid::Uuid::new_v4()));
    let original;
    {
        let store = MemoryStore::open(&path).unwrap();
        let record = store
            .remember(NewMemory::project_fact("repo", "Queue", "Serial queue"))
            .unwrap();
        original = store.verify(record.id, "source review passed").unwrap();
    }
    {
        // Schema 3 had the same claim table, without replication metadata.
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("DROP TRIGGER memory_sync_insert; DROP TRIGGER memory_sync_update; DROP TRIGGER memory_sync_delete; DROP TABLE memory_sync_changes; DROP TABLE memory_sync_projects; DROP TABLE memory_sync_peers; DROP TABLE memory_sync_clock; PRAGMA user_version=3;").unwrap();
    }
    {
        let upgraded = MemoryStore::open(&path).unwrap();
        assert_eq!(upgraded.get(original.id).unwrap(), original);
        assert_eq!(
            upgraded.sync_export("repo", 0).unwrap().changes[0].id,
            original.id
        );
        let connection = rusqlite::Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 4);
        assert!(
            version > 3,
            "the previous binary's schema guard must reject reopening it"
        );
    }
    std::fs::remove_file(path).unwrap();
}
