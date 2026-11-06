use verified_code_memory::{
    MemoryRecord, MemoryStats, MemoryStore, NewMemory, RetrievalQuery, SharedMemoryStore,
    Verification,
};

fn sample() -> NewMemory {
    NewMemory::episode(
        "payments-service",
        "Checkout retry race condition",
        "Serialize retries by order ID; increasing retry count did not fix the race.",
    )
    .with_files(["src/checkout/retry.py", "tests/test_retry.py"])
    .with_tags(["checkout", "retry", "race condition"])
    .with_code_version("abc123")
}

#[test]
fn fts_retrieval_finds_verified_memory() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store.remember(sample()).unwrap();
    store.verify(memory.id, "12/12 tests passed").unwrap();

    let (results, telemetry) = store
        .retrieve_with_telemetry(
            &RetrievalQuery::new("payments-service", "fix checkout retry race condition")
                .with_files(["src/checkout/retry.py"])
                .with_failure("race condition remained"),
        )
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].record.id, memory.id);
    assert!(telemetry.used_full_text_search);
    assert_eq!(telemetry.fts_matches, 1);
    assert_eq!(telemetry.returned, 1);
}

#[test]
fn stale_versions_are_deprecated() {
    let store = MemoryStore::open_in_memory().unwrap();
    let old = store.remember(sample()).unwrap();
    let current = store
        .remember(
            NewMemory::episode(
                "payments-service",
                "Current retry approach",
                "Use the new retry queue.",
            )
            .with_code_version("def456"),
        )
        .unwrap();

    let changed = store
        .invalidate_stale_for_version("payments-service", "def456", "Repository moved")
        .unwrap();

    assert_eq!(changed, 1);
    assert_eq!(
        store.get(old.id).unwrap().verification,
        Verification::Deprecated
    );
    assert_eq!(
        store.get(current.id).unwrap().verification,
        Verification::Provisional
    );
}

#[test]
fn conflict_resolution_keeps_verified_memory() {
    let store = MemoryStore::open_in_memory().unwrap();
    let keeper = store
        .remember(NewMemory::project_fact(
            "repo",
            "Use new auth",
            "Use OAuth device flow.",
        ))
        .unwrap();
    let loser = store
        .remember(NewMemory::project_fact(
            "repo",
            "Use old auth",
            "Use legacy token.",
        ))
        .unwrap();

    store.verify(keeper.id, "review approved").unwrap();
    let kept = store
        .resolve_conflict(keeper.id, &[loser.id], "Superseded by verified OAuth flow")
        .unwrap();

    assert_eq!(kept.id, keeper.id);
    assert_eq!(
        store.get(loser.id).unwrap().verification,
        Verification::Deprecated
    );
}

#[test]
fn validation_and_limits_are_enforced() {
    let store = MemoryStore::open_in_memory().unwrap();

    assert!(store
        .remember(NewMemory::episode("", "title", "content"))
        .is_err());
    assert!(store
        .remember(NewMemory::episode("repo", " ", "content"))
        .is_err());
    assert!(store
        .remember(NewMemory::episode("repo", "title", ""))
        .is_err());
    assert!(store
        .retrieve(&RetrievalQuery::new("repo", "task").with_limit(0))
        .is_err());
    assert!(store
        .retrieve(&RetrievalQuery::new("repo", "task").with_limit(33))
        .is_err());
}

#[test]
fn shared_store_is_concurrent() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let memory = store.remember(sample()).unwrap();
    store.verify(memory.id, "tests passed").unwrap();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || {
                store
                    .retrieve(&RetrievalQuery::new(
                        "payments-service",
                        "checkout retry race",
                    ))
                    .unwrap()
                    .len()
            })
        })
        .collect();

    for handle in handles {
        assert_eq!(handle.join().unwrap(), 1);
    }
}

#[test]
fn memories_persist_across_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.db");

    let memory = {
        let store = MemoryStore::open(&path).unwrap();
        let memory = store.remember(sample()).unwrap();
        store.verify(memory.id, "tests passed").unwrap();
        memory
    };

    let reopened = MemoryStore::open(&path).unwrap();
    assert_eq!(
        reopened.get(memory.id).unwrap().verification,
        Verification::Verified
    );
}

#[test]
fn stats_counts_each_state_of_one_project_only() {
    let store = MemoryStore::open_in_memory().unwrap();

    let verified = store
        .remember(NewMemory::episode(
            "a",
            "Retry race",
            "Serialize retries by order ID.",
        ))
        .unwrap();
    store
        .remember(NewMemory::episode(
            "a",
            "Retry tests",
            "The retries are covered twice.",
        ))
        .unwrap();
    let deprecated = store
        .remember(NewMemory::episode(
            "b",
            "Old retry queue",
            "The old queue was replaced.",
        ))
        .unwrap();

    store.verify(verified.id, "cargo test passed").unwrap();
    store
        .deprecate(deprecated.id, "the old queue was replaced")
        .unwrap();

    assert_eq!(
        store.stats("a").unwrap(),
        MemoryStats {
            total: 2,
            verified: 1,
            provisional: 1,
            deprecated: 0,
        },
        "project a counts both of its memories, one per trust state"
    );
    assert_eq!(
        store.stats("b").unwrap(),
        MemoryStats {
            total: 1,
            verified: 0,
            provisional: 0,
            deprecated: 1,
        },
        "project b counts only its own memory"
    );
    assert_eq!(
        store.stats("c").unwrap(),
        MemoryStats::default(),
        "a project with no memories is all zeros"
    );
    assert!(store.stats("").is_err(), "an empty repo is refused");
}

#[test]
fn the_shared_store_passes_stats_and_conflict_resolution_through() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let keeper = store.remember(sample()).unwrap();
    let loser = store
        .remember(
            NewMemory::project_fact("payments-service", "Retry approach", "Retry the whole order."),
        )
        .unwrap();
    store.verify(keeper.id, "12/12 tests passed").unwrap();

    assert_eq!(
        store.stats("payments-service").unwrap(),
        MemoryStats {
            total: 2,
            verified: 1,
            provisional: 1,
            deprecated: 0,
        }
    );

    let kept = store
        .resolve_conflict(keeper.id, &[loser.id], "a newer run's fix replaced it")
        .unwrap();

    assert_eq!(kept.id, keeper.id);
    assert_eq!(
        store.stats("payments-service").unwrap(),
        MemoryStats {
            total: 2,
            verified: 1,
            provisional: 0,
            deprecated: 1,
        },
        "the superseded memory is counted as deprecated"
    );
}

fn ids(memories: &[MemoryRecord]) -> Vec<String> {
    memories.iter().map(|memory| memory.id.to_string()).collect()
}

fn webhook_memory() -> NewMemory {
    NewMemory::episode(
        "payments-service",
        "Webhook signatures",
        "Compare the HMAC with a constant-time check.",
    )
    .with_files(["src/payments/webhook.py"])
    .with_tags(["security", "webhook"])
}

#[test]
fn listing_a_projects_memories_is_newest_first_and_leaves_other_projects_out() {
    let store = MemoryStore::open_in_memory().unwrap();
    let older = store
        .remember(NewMemory::episode(
            "payments-service",
            "Serialize retries",
            "Serialize retries by order ID.",
        ))
        .unwrap();
    let newer = store
        .remember(NewMemory::episode(
            "payments-service",
            "Retry tests",
            "The retries are covered twice.",
        ))
        .unwrap();
    store
        .remember(NewMemory::episode(
            "billing-service",
            "Billing retry",
            "The billing worker retries at night.",
        ))
        .unwrap();

    assert_eq!(
        ids(&store.list("payments-service", "").unwrap()),
        vec![newer.id.to_string(), older.id.to_string()],
        "the memory remembered last is listed first"
    );

    store
        .deprecate(older.id, "the retry queue replaced it")
        .unwrap();
    store
        .edit(
            older.id,
            "Serialize retries",
            "Serialize every retry by order ID.",
        )
        .unwrap();

    let listed = store.list("payments-service", "").unwrap();
    assert_eq!(
        ids(&listed),
        vec![older.id.to_string(), newer.id.to_string()],
        "the edited memory is the newest, and a discontinued one is listed too"
    );
    assert_eq!(
        listed[0].verification,
        Verification::Deprecated,
        "deprecated memories are listed, not hidden"
    );
    assert_eq!(listed.len(), 2, "another project's memories are left out");
    assert!(
        listed
            .iter()
            .all(|memory| memory.repo == "payments-service"),
        "every listed memory belongs to the project asked about"
    );
    assert!(
        store.list("", "").is_err(),
        "a listing without a project is refused"
    );
}

#[test]
fn the_list_narrows_to_what_the_query_mentions() {
    let store = MemoryStore::open_in_memory().unwrap();
    let retry = store.remember(sample()).unwrap();
    let webhook = store.remember(webhook_memory()).unwrap();
    let formatting = store
        .remember(
            NewMemory::project_fact(
                "payments-service",
                "Percent signs are literal",
                "A 100% match is a display quirk; snake_case names stay as typed.",
            )
            .with_tags(["formatting"]),
        )
        .unwrap();

    assert_eq!(
        store.list("payments-service", "").unwrap().len(),
        3,
        "an empty query matches every memory of the project"
    );

    assert_eq!(
        ids(&store.list("payments-service", "webhook").unwrap()),
        vec![webhook.id.to_string()],
        "a word of a title matches, whatever its case"
    );
    assert_eq!(
        ids(&store.list("payments-service", "constant-time").unwrap()),
        vec![webhook.id.to_string()],
        "a word of the text matches"
    );
    assert_eq!(
        ids(&store.list("payments-service", "src/checkout/retry.py").unwrap()),
        vec![retry.id.to_string()],
        "a file path a memory names matches"
    );
    assert_eq!(
        ids(&store.list("payments-service", "security").unwrap()),
        vec![webhook.id.to_string()],
        "a tag matches"
    );
    assert_eq!(
        ids(&store.list("payments-service", "%").unwrap()),
        vec![formatting.id.to_string()],
        "a typed % is a % and not a wildcard"
    );
    let underscored = ids(&store.list("payments-service", "_").unwrap());
    assert!(
        underscored.contains(&retry.id.to_string()),
        "a query of _ matches the file path that holds one"
    );
    assert!(
        underscored.contains(&formatting.id.to_string()),
        "a query of _ matches the text that holds one"
    );
    assert!(
        !underscored.contains(&webhook.id.to_string()),
        "a typed _ is a _ and not a wildcard"
    );
    assert!(
        store
            .list("payments-service", "no memory mentions this")
            .unwrap()
            .is_empty(),
        "a query nothing mentions finds nothing"
    );
    assert!(
        store.list("billing-service", "webhook").unwrap().is_empty(),
        "another project's memories are not searched"
    );
}

#[test]
fn an_edit_rewrites_a_memorys_title_and_text_and_leaves_its_trust_state_alone() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store.remember(sample()).unwrap();
    store.verify(memory.id, "12/12 tests passed").unwrap();
    let before = store.get(memory.id).unwrap();
    let edited_text = "Serialize every retry by order ID through the retryqueue.";

    let edited = store
        .edit(
            memory.id,
            "  Checkout retry serialization  ",
            "  Serialize every retry by order ID through the retryqueue.  ",
        )
        .unwrap();

    assert_eq!(edited.id, memory.id, "the same memory, corrected");
    assert_eq!(
        edited.title, "Checkout retry serialization",
        "the new title is kept, trimmed"
    );
    assert_eq!(edited.content, edited_text, "the new text is kept, trimmed");
    assert_eq!(edited.kind, before.kind);
    assert_eq!(
        edited.verification,
        Verification::Verified,
        "the trust state the agent gave it stays"
    );
    assert_eq!(edited.files, before.files, "the files stay as they were");
    assert_eq!(edited.tags, before.tags, "the tags stay as they were");
    assert!(
        edited.updated_at >= before.updated_at,
        "the edit marks the memory as changed"
    );

    let stored = store.get(memory.id).unwrap();
    assert_eq!(stored.title, "Checkout retry serialization");
    assert_eq!(stored.content, edited_text);
    assert_eq!(stored.verification, Verification::Verified);

    let found = store
        .retrieve(&RetrievalQuery::new("payments-service", "retryqueue"))
        .unwrap();
    assert_eq!(
        found.len(),
        1,
        "retrieval finds the memory by the word the edit wrote"
    );
    assert_eq!(found[0].record.id, memory.id);
    assert_eq!(
        found[0].record.content, edited_text,
        "retrieval reads the edited text"
    );
}

#[test]
fn an_edit_that_would_empty_the_title_or_the_text_is_refused() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store.remember(sample()).unwrap();

    assert!(
        store
            .edit(memory.id, "   ", "Serialize retries by order ID.")
            .is_err(),
        "a title of spaces is refused"
    );
    assert!(
        store
            .edit(memory.id, "Checkout retry race condition", " \n\t ")
            .is_err(),
        "a text of spaces is refused"
    );

    let unchanged = store.get(memory.id).unwrap();
    assert_eq!(
        unchanged.title, "Checkout retry race condition",
        "the refused edit left the title as it was"
    );
    assert_eq!(
        unchanged.content,
        "Serialize retries by order ID; increasing retry count did not fix the race.",
        "the refused edit left the text as it was"
    );
}

#[test]
fn deleting_a_memory_removes_it_and_deleting_it_twice_is_not_found() {
    let store = MemoryStore::open_in_memory().unwrap();
    let stale = store.remember(sample()).unwrap();
    let kept = store.remember(webhook_memory()).unwrap();

    store.delete(stale.id).unwrap();

    assert!(
        store.get(stale.id).is_err(),
        "the deleted memory is no longer there"
    );
    assert!(
        store.delete(stale.id).is_err(),
        "deleting it a second time is not found"
    );
    assert_eq!(
        ids(&store.list("payments-service", "").unwrap()),
        vec![kept.id.to_string()],
        "only the other memory is left"
    );
    assert_eq!(
        store.stats("payments-service").unwrap(),
        MemoryStats {
            total: 1,
            verified: 0,
            provisional: 1,
            deprecated: 0,
        },
        "the count no longer includes the deleted memory"
    );
    assert!(
        store
            .retrieve(&RetrievalQuery::new(
                "payments-service",
                "checkout retry race condition"
            ))
            .unwrap()
            .iter()
            .all(|memory| memory.record.id != stale.id),
        "a deleted memory is no longer recalled"
    );
}

#[test]
fn clearing_removes_one_projects_memories_and_says_how_many() {
    let store = MemoryStore::open_in_memory().unwrap();
    store.remember(sample()).unwrap();
    store.remember(webhook_memory()).unwrap();
    let other = store
        .remember(NewMemory::episode(
            "billing-service",
            "Billing retry",
            "The billing worker retries at night.",
        ))
        .unwrap();

    assert_eq!(
        store.clear("payments-service").unwrap(),
        2,
        "both memories of the project went"
    );
    assert!(
        store.list("payments-service", "").unwrap().is_empty(),
        "nothing of the cleared project is listed"
    );
    assert_eq!(
        store.stats("payments-service").unwrap(),
        MemoryStats::default(),
        "the cleared project counts nothing"
    );
    assert_eq!(
        ids(&store.list("billing-service", "").unwrap()),
        vec![other.id.to_string()],
        "another project's memories stay"
    );
    assert_eq!(
        store.clear("payments-service").unwrap(),
        0,
        "clearing an empty project says nothing went"
    );
    assert!(
        store.clear("").is_err(),
        "a clear without a project is refused"
    );
}

#[test]
fn the_shared_store_lists_edits_deletes_and_clears_through_its_lock() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let memory = store.remember(sample()).unwrap();
    let other = store.remember(webhook_memory()).unwrap();

    assert_eq!(
        store.list("payments-service", "").unwrap().len(),
        2,
        "the shared store lists the project's memories"
    );

    let edited = store
        .edit(
            memory.id,
            "Retry serialization",
            "Serialize every retry by order ID.",
        )
        .unwrap();
    assert_eq!(edited.title, "Retry serialization");
    assert_eq!(
        ids(&store.list("payments-service", "serialization").unwrap()),
        vec![memory.id.to_string()],
        "the shared store narrows a listing by the query"
    );

    store.delete(memory.id).unwrap();
    assert_eq!(
        ids(&store.list("payments-service", "").unwrap()),
        vec![other.id.to_string()],
        "the shared store deletes a memory"
    );
    assert_eq!(
        store.clear("payments-service").unwrap(),
        1,
        "the shared store clears a project"
    );
    assert!(store.list("payments-service", "").unwrap().is_empty());
}

