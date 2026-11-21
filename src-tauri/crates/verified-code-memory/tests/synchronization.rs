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
        assert_eq!(version, 5);
        assert!(
            version > 3,
            "the previous binary's schema guard must reject reopening it"
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_team_scan_reports_invalid_revisions_without_blocking_later_records() {
    use verified_code_memory::sync::MAX_PAGE_BYTES;
    let app = store();
    let invalid = app
        .remember(
            NewMemory::project_fact("app", "Invalid", "out of scope").with_files(["../private"]),
        )
        .unwrap();
    let large = app
        .remember(NewMemory::project_fact(
            "app",
            "Too large",
            "x".repeat(MAX_PAGE_BYTES),
        ))
        .unwrap();
    let valid = app
        .remember(NewMemory::project_fact("app", "Eligible", "within scope"))
        .unwrap();
    let scanned = app.sync_scan("app", 0).unwrap();
    assert_eq!(
        scanned.rejected.iter().map(|r| r.id).collect::<Vec<_>>(),
        [invalid.id, large.id]
    );
    assert_eq!(scanned.page.changes[0].id, valid.id);
    assert!(!scanned.page.more);
    assert!(app
        .sync_scan("app", scanned.page.through)
        .unwrap()
        .page
        .changes
        .is_empty());
}

#[test]
fn team_clear_preserves_private_notes_and_offline_edits_on_personal_hosts() {
    let (team, alice, bob, daemon) = (store(), store(), store(), store());
    let link = uuid::Uuid::new_v4();
    team.remember(NewMemory::project_fact("team", "Agreed", "Shared claim"))
        .unwrap();
    let first = team.sync_export("team", 0).unwrap();
    for app in [&alice, &bob] {
        app.team_import("desktop", link, 0, &first).unwrap();
    }
    let id = alice.list("desktop", "").unwrap()[0].id;
    alice.edit(id, "Agreed", "Offline correction").unwrap();
    let private = alice
        .remember(NewMemory::project_fact("desktop", "Private", "Local note"))
        .unwrap();
    exchange(&alice, &daemon);
    team.clear("team").unwrap();
    let cleared = team.sync_export("team", first.through).unwrap();
    for app in [&alice, &bob] {
        app.team_import("desktop", link, first.through, &cleared)
            .unwrap();
    }
    exchange(&alice, &daemon);
    assert_eq!(daemon.get(id).unwrap().content, "Offline correction");
    assert!(alice.get(private.id).is_ok());
    assert!(bob.list("desktop", "").unwrap().is_empty());
    assert_eq!(
        alice.team_import_cursor("desktop", link).unwrap(),
        cleared.through
    );
}

#[test]
fn a_reviewed_publication_can_be_edited_and_withdrawn_without_deleting_the_personal_note() {
    use verified_code_memory::{team::project_record, SourceKind};
    let (app, team, hub, teammate) = (store(), store(), store(), store());
    let link = uuid::Uuid::new_v4();
    let scope = link.to_string();
    let record = app
        .remember(
            NewMemory::project_fact("mine", "Rule", "Run checks")
                .with_tags(["chat:private-session", "build"])
                .with_source_kind(SourceKind::UserAssertion),
        )
        .unwrap();
    let preview = project_record(&record, |_| true).unwrap();
    assert_eq!(preview.tags, ["build"]);
    let team_id = app
        .team_queue_publication("mine", link, &preview, |_| true)
        .unwrap();
    assert!(app
        .team_flush_publication("mine", link, &team, |_| true)
        .unwrap());
    let first = team.sync_export(&scope, 0).unwrap();
    let device = team.sync_replica().unwrap();
    let server = hub.sync_replica().unwrap();
    for _ in 0..2 {
        hub.team_accept("project", "alice", &device, &server, &first, |_| true)
            .unwrap();
    }
    teammate
        .team_import("other", link, 0, &hub.sync_export("project", 0).unwrap())
        .unwrap();
    assert_eq!(teammate.list("other", "").unwrap()[0].content, "Run checks");
    let edit = app.edit(record.id, "Rule", "Run full checks").unwrap();
    let preview = project_record(&edit, |_| true).unwrap();
    assert_eq!(
        app.team_queue_publication("mine", link, &preview, |_| true)
            .unwrap(),
        team_id
    );
    app.team_flush_publication("mine", link, &team, |_| true)
        .unwrap();
    let second = team.sync_export(&scope, first.through).unwrap();
    hub.team_accept("project", "alice", &device, &server, &second, |_| true)
        .unwrap();
    assert_eq!(hub.get(team_id).unwrap().content, "Run full checks");
    app.team_withdraw("mine", link, record.id).unwrap();
    app.team_flush_publication("mine", link, &team, |_| true)
        .unwrap();
    let deleted = team.sync_export(&scope, second.through).unwrap();
    hub.team_accept("project", "alice", &device, &server, &deleted, |_| true)
        .unwrap();
    assert!(hub.get(team_id).is_err());
    assert_eq!(app.get(record.id).unwrap(), edit);
    assert!(app
        .team_queue_publication("mine", link, &preview, |_| true)
        .is_err());
}

#[test]
fn publication_refuses_private_or_restricted_records_and_changed_previews() {
    use verified_code_memory::{team::project_record, SourceKind};
    let app = store();
    let record = app
        .remember(
            NewMemory::procedure("mine", "Rule", "Run checks")
                .with_source_kind(SourceKind::CodeObservation)
                .with_files(["private/file"]),
        )
        .unwrap();
    assert!(project_record(&record, |_| false).is_err());
    let preview = project_record(&record, |_| true).unwrap();
    let edit = app
        .edit(record.id, "Rule", "api_key=abcdefgh1234567890")
        .unwrap();
    assert!(project_record(&edit, |_| true).is_err());
    assert!(app
        .team_queue_publication("mine", uuid::Uuid::new_v4(), &preview, |_| true)
        .is_err());
    let mut private = record;
    private.tags.push("private".into());
    assert!(project_record(&private, |_| true).is_err());
}

#[test]
fn restoring_a_team_snapshot_preserves_notes_missing_from_the_restored_hub() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.db");
    let backup = dir.path().join("backup.db");
    let app = store();
    let old = MemoryStore::open(&path).unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
        .unwrap();
    let link = uuid::Uuid::new_v4();
    let key = link.to_string();
    old.remember(NewMemory::project_fact(
        &key,
        "Incident note",
        "After the backup",
    ))
    .unwrap();
    app.team_import("mine", link, 0, &old.sync_export(&key, 0).unwrap())
        .unwrap();
    let local = app.list("mine", "").unwrap()[0].id;
    let old_identity = old.sync_replica().unwrap();
    drop(old);
    let restored = MemoryStore::open(backup).unwrap();
    assert_eq!(restored.sync_replica().unwrap(), old_identity);
    assert_ne!(restored.team_rotate_restored_hub().unwrap(), old_identity);
    app.team_rebind("mine", link, &restored).unwrap();
    app.team_import("mine", link, 0, &restored.sync_export(&key, 0).unwrap())
        .unwrap();
    assert_eq!(app.get(local).unwrap().content, "After the backup");
    assert_eq!(
        app.team_conflicts("mine", link).unwrap(),
        [local.to_string()]
    );
    app.team_resolve("mine", link, local).unwrap();
    assert!(app.team_conflicts("mine", link).unwrap().is_empty());
    assert!(app.get(local).is_ok());
}

#[test]
fn policy_tightening_before_flush_flags_one_item_and_allows_the_next_to_publish() {
    use verified_code_memory::{team::project_record, SourceKind};
    let (app, replica) = (store(), store());
    let link = uuid::Uuid::new_v4();
    for f in ["first.txt", "second.txt"] {
        let record = app
            .remember(
                NewMemory::project_fact("mine", f, "Run checks")
                    .with_files([f])
                    .with_source_kind(SourceKind::UserAssertion),
            )
            .unwrap();
        app.team_queue_publication(
            "mine",
            link,
            &project_record(&record, |_| true).unwrap(),
            |_| true,
        )
        .unwrap();
    }
    while app
        .team_flush_publication("mine", link, &replica, |f| f != "first.txt")
        .unwrap()
    {}
    assert_eq!(
        replica.list(&link.to_string(), "").unwrap()[0].title,
        "second.txt"
    );
    assert_eq!(app.team_blocked("mine", link).unwrap().len(), 1);
}

#[test]
fn migrating_a_v4_desktop_preserves_its_personal_host_cursor_and_enables_a_separate_team_link() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("desktop.db");
    let host = store().sync_replica().unwrap();
    let progress = verified_code_memory::sync::Progress {
        replica: host,
        sent: 5,
        received: 7,
    };
    let id = {
        let app = MemoryStore::open(&path).unwrap();
        let id = app
            .remember(NewMemory::project_fact(
                "root",
                "Fact",
                "Stored before linking",
            ))
            .unwrap()
            .id;
        app.sync_ack("root", "personal", &progress).unwrap();
        id
    };
    let db = rusqlite::Connection::open(&path).unwrap();
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name GLOB 'team_*'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for table in tables {
        db.execute_batch(&format!("DROP TABLE {table}")).unwrap();
    }
    db.pragma_update(None, "user_version", 4).unwrap();
    drop(db);
    let app = MemoryStore::open(&path).unwrap();
    assert_eq!(app.get(id).unwrap().content, "Stored before linking");
    assert_eq!(app.sync_progress("root", "personal").unwrap(), progress);
    assert_eq!(
        app.team_import_cursor("root", uuid::Uuid::new_v4())
            .unwrap(),
        0
    );
}

#[test]
fn one_hundred_writers_converge_after_duplicate_pushes_and_offline_catch_up() {
    use verified_code_memory::{team::project_record, SourceKind};
    let hub = store();
    let identity = hub.sync_replica().unwrap();
    let link = uuid::Uuid::new_v4();
    let key = link.to_string();
    for writer in 0..100 {
        let (app, team) = (store(), store());
        let r = app
            .remember(
                NewMemory::project_fact("local", format!("Build step {writer}"), "Run checks")
                    .with_source_kind(SourceKind::CodeObservation),
            )
            .unwrap();
        app.team_queue_publication(
            "local",
            link,
            &project_record(&r, |_| true).unwrap(),
            |_| true,
        )
        .unwrap();
        app.team_flush_publication("local", link, &team, |_| true)
            .unwrap();
        let page = team.sync_export(&key, 0).unwrap();
        for _ in 0..2 {
            hub.team_accept(
                "project",
                &format!("user-{writer}"),
                &page.replica,
                &identity,
                &page,
                |_| true,
            )
            .unwrap();
        }
    }
    let joining = store();
    let mut cursor = 0;
    loop {
        let page = hub.sync_export("project", cursor).unwrap();
        joining.sync_apply("project", &page).unwrap();
        cursor = page.through;
        if !page.more {
            break;
        }
    }
    assert_eq!(joining.stats("project").unwrap().total, 100);
    assert_eq!(hub.stats("project").unwrap().total, 100);
}
