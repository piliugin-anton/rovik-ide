use verified_code_memory::{
    MemoryStats, MemoryStore, NewMemory, RetrievalQuery, SharedMemoryStore, Verification,
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

