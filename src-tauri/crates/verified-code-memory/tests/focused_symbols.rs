use uuid::Uuid;
use verified_code_memory::{
    semantic::SemanticIndex, MemoryEmbedder, NewMemory, RetrievalQuery, SharedMemoryStore,
    Verification,
};

struct FocusModel;
impl MemoryEmbedder for FocusModel {
    fn dimensions(&self) -> usize {
        2
    }
    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        Ok(if text.contains("lock") {
            vec![0.0, 1.0]
        } else {
            vec![1.0, 0.0]
        })
    }
}

fn fixture() -> (SharedMemoryStore, SemanticIndex, Uuid) {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store",
            "Use a lock to protect shared state.",
        ))
        .unwrap();
    store.verify(wanted.id, "source evidence").unwrap();
    for n in 0..8 {
        let record = store
            .remember(
                NewMemory::procedure(
                    "a",
                    format!("Crowding observation {n}"),
                    format!("All crowding samples {n} record independent observations."),
                )
                .with_files(["src/context.rs"]),
            )
            .unwrap();
        store.verify(record.id, "separate observation").unwrap();
    }
    let index = SemanticIndex::new(store.clone(), "focus-fixture", 2).unwrap();
    index.backfill("a", &FocusModel).unwrap();
    (store, index, wanted.id)
}

fn ids(hits: &[verified_code_memory::RetrievedMemory]) -> Vec<Uuid> {
    hits.iter().map(|hit| hit.record.id).collect()
}

#[test]
fn terse_symbol_requests_recover_the_named_fact_without_moving_the_leading_results() {
    let (store, index, wanted) = fixture();
    for task in ["SharedMemoryStore crowding", "store_memory_shared crowding"] {
        let query = RetrievalQuery::new("a", task);
        // An empty supplied failure leaves both embedding and lexical inputs
        // identical, while deliberately preserving the original context pool.
        let before = index
            .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
            .unwrap();
        assert_eq!(before.len(), 8);
        assert!(before.iter().all(|hit| hit.record.id != wanted));
        let after = index.retrieve(&query, &FocusModel, 0.8).unwrap();
        assert_eq!(&ids(&after)[..7], &ids(&before)[..7]);
        assert_eq!(after[7].record.id, wanted);
        assert!(after[7].score < after[6].score);
        assert!(after[7].reason.contains("focused complete symbol"));
        let (packet, shown) = store.render_packet_traced(&after, 1000);
        assert!(shown.contains(&wanted));
        assert!(packet.contains("Use a lock to protect shared state."));
        assert!(verified_code_memory::retrieval::estimate_tokens(&packet) <= 1000);
    }
}

#[test]
fn collection_narrative_file_and_failure_requests_keep_their_full_observation_pool() {
    let (_, index, wanted) = fixture();
    let queries = [
        RetrievalQuery::new(
            "a",
            "Keep every crowding observation while reviewing SharedMemoryStore.",
        ),
        RetrievalQuery::new(
            "a",
            "Preserve all crowding facts while reviewing store_memory_shared.",
        ),
        RetrievalQuery::new("a", "Review SharedMemoryStore crowding"),
        RetrievalQuery::new("a", "SharedMemoryStore crowding*"),
        RetrievalQuery::new("a", "SharedMemoryStore crowding?"),
        RetrievalQuery::new("a", "SharedMemoryStore all_crowding"),
        RetrievalQuery::new("a", "SharedMemoryStore all-crowding"),
        RetrievalQuery::new("a", "SharedMemoryStore crowding")
            .with_failure("Retain every observation."),
        RetrievalQuery::new("a", "SharedMemoryStore crowding").with_files(["src/context.rs"]),
    ];
    for query in queries {
        let before = index
            .retrieve(
                &query
                    .clone()
                    .with_failure(query.failure.clone().unwrap_or_default()),
                &FocusModel,
                0.8,
            )
            .unwrap();
        let after = index.retrieve(&query, &FocusModel, 0.8).unwrap();
        assert_eq!(ids(&after), ids(&before), "{}", query.task);
        assert_eq!(after.len(), 8);
        assert!(
            after.iter().all(|hit| hit.record.id != wanted),
            "{}",
            query.task
        );
    }
}

#[test]
fn a_single_result_existing_symbol_and_partial_names_keep_their_original_results() {
    let (_, index, wanted) = fixture();
    let query = RetrievalQuery::new("a", "SharedMemoryStore crowding").with_limit(1);
    let before = index
        .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
        .unwrap();
    assert_eq!(
        ids(&index.retrieve(&query, &FocusModel, 0.8).unwrap()),
        ids(&before)
    );
    let represented = index
        .retrieve(
            &RetrievalQuery::new("a", "SharedMemoryStore"),
            &FocusModel,
            0.8,
        )
        .unwrap();
    assert_eq!(
        represented
            .iter()
            .filter(|hit| hit.record.id == wanted)
            .count(),
        1
    );
    // This numeric context has no ordinary crowding match, so the existing
    // lexical fallback already supplies the symbol. Preserve that route too.
    let numeric = RetrievalQuery::new("a", "SharedMemoryStore crowding2");
    let before = index
        .retrieve(&numeric.clone().with_failure(""), &FocusModel, 0.8)
        .unwrap();
    assert!(before.iter().any(|hit| hit.record.id == wanted));
    assert_eq!(
        ids(&index.retrieve(&numeric, &FocusModel, 0.8).unwrap()),
        ids(&before)
    );
    for task in [
        "SharedMemoryEncryptor crowding",
        "fix_the_tests crowding",
        "SharedMemoryStore2 crowding",
    ] {
        let query = RetrievalQuery::new("a", task);
        let before = index
            .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
            .unwrap();
        assert_eq!(
            ids(&index.retrieve(&query, &FocusModel, 0.8).unwrap()),
            ids(&before)
        );
    }
}

#[test]
fn focused_symbol_candidates_obey_scope_trust_tags_and_caller_visibility_before_limits() {
    let (store, index, wanted) = fixture();
    for n in 0..90 {
        let hidden = store
            .remember(
                NewMemory::procedure(
                    "a",
                    format!("Shared memory store private {n}"),
                    "Use a lock.",
                )
                .with_tags(["private"]),
            )
            .unwrap();
        store.verify(hidden.id, "source evidence").unwrap();
        store
            .remember(NewMemory::procedure(
                "a",
                format!("Shared memory store provisional {n}"),
                "Use a lock.",
            ))
            .unwrap();
    }
    let foreign = store
        .remember(NewMemory::procedure(
            "b",
            "Shared memory store",
            "Use a lock.",
        ))
        .unwrap();
    store.verify(foreign.id, "foreign source evidence").unwrap();
    index.backfill("a", &FocusModel).unwrap();
    index.backfill("b", &FocusModel).unwrap();
    let query = RetrievalQuery::new("a", "SharedMemoryStore crowding")
        .with_verifications([Verification::Verified])
        .without_tags(["private"]);
    let hits = index.retrieve(&query, &FocusModel, 0.8).unwrap();
    assert_eq!(hits.last().unwrap().record.id, wanted);
    let denied = index
        .retrieve_filtered(&query, &FocusModel, 0.8, |r| r.id != wanted)
        .unwrap();
    assert_eq!(denied.len(), 8);
    assert!(denied.iter().all(|hit| hit.record.id != wanted));
    store.deprecate(wanted, "obsolete").unwrap();
    let obsolete = index.retrieve(&query, &FocusModel, 0.8).unwrap();
    assert_eq!(ids(&obsolete), ids(&denied));
}

#[test]
fn a_known_complete_match_beyond_the_supplementary_pool_is_not_replaced() {
    struct DenseModel;
    impl MemoryEmbedder for DenseModel {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            Ok(if text.contains("anchor") {
                vec![1.0, 0.0]
            } else if text.starts_with("Crowding") {
                vec![0.9, 0.4358899]
            } else if text.contains("alternative") {
                vec![0.0, 1.0]
            } else {
                vec![1.0, 0.0]
            })
        }
    }
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store",
            format!("anchor {}", "padding ".repeat(100)),
        ))
        .unwrap();
    for n in 0..100 {
        store
            .remember(NewMemory::procedure(
                "a",
                format!("Shared memory store alternative {n}"),
                "alternative",
            ))
            .unwrap();
    }
    for n in 0..8 {
        store
            .remember(NewMemory::procedure(
                "a",
                format!("Crowding {n}"),
                "crowding",
            ))
            .unwrap();
    }
    let index = SemanticIndex::new(store, "dense-fixture", 2).unwrap();
    index.backfill("a", &DenseModel).unwrap();
    let query = RetrievalQuery::new("a", "SharedMemoryStore crowding");
    let before = index
        .retrieve(&query.clone().with_failure(""), &DenseModel, 0.8)
        .unwrap();
    assert_eq!(before.len(), 8);
    assert!(before.iter().any(|hit| hit.record.id == wanted.id));
    let after = index.retrieve(&query, &DenseModel, 0.8).unwrap();
    assert_eq!(ids(&after), ids(&before));
}

#[test]
fn multiple_named_facts_are_recovered_atomically_without_moving_existing_named_facts() {
    let (store, index, first) = fixture();
    let second = store
        .remember(NewMemory::procedure(
            "a",
            "Retrieval query",
            "Use a lock for query state.",
        ))
        .unwrap();
    store.verify(second.id, "separate source").unwrap();
    index.backfill("a", &FocusModel).unwrap();
    for task in [
        "SharedMemoryStore RetrievalQuery crowding",
        "shared_memory_store RetrievalQuery crowding",
        "SharedMemoryStore,RetrievalQuery crowding",
        "SharedMemoryStore;RetrievalQuery crowding",
        "crate::SharedMemoryStore,crate::RetrievalQuery crowding",
        "SharedMemoryStore,SharedMemoryStore,RetrievalQuery crowding",
    ] {
        let query = RetrievalQuery::new("a", task);
        let before = index
            .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
            .unwrap();
        let after = index.retrieve(&query, &FocusModel, 0.8).unwrap();
        assert_eq!(after.len(), 8);
        assert_eq!(after[0].record.id, before[0].record.id);
        assert!(after.iter().any(|h| h.record.id == first));
        assert!(after.iter().any(|h| h.record.id == second.id));
        for id in [first, second.id] {
            if let Some(rank) = before.iter().position(|h| h.record.id == id) {
                assert_eq!(after.iter().position(|h| h.record.id == id), Some(rank));
            }
        }
        let (packet, shown) = store.render_packet_traced(&after, 1000);
        assert!(shown.contains(&second.id));
        assert!(packet.contains(&second.content));
    }
    for task in [
        "SharedMemoryStore UnknownMemory crowding",
        "all SharedMemoryStore RetrievalQuery crowding",
        "Please review SharedMemoryStore RetrievalQuery crowding",
        "SharedMemoryStore,UnknownMemory crowding",
        "all SharedMemoryStore,RetrievalQuery crowding",
        "SharedMemoryStore;RetrievalQuery all_crowding",
        "SharedMemoryStore,RetrievalQuery crowding 2",
        "SharedMemoryStore,RetrievalQuery crowding*",
        "Please review SharedMemoryStore,RetrievalQuery crowding",
    ] {
        let query = RetrievalQuery::new("a", task);
        let before = index
            .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
            .unwrap();
        assert_eq!(
            ids(&index.retrieve(&query, &FocusModel, 0.8).unwrap()),
            ids(&before)
        );
    }
}

#[test]
fn an_explicit_context_title_is_protected_alongside_compound_names() {
    let (store, index, first) = fixture();
    let second = store
        .remember(NewMemory::procedure(
            "a",
            "Retrieval query",
            "Use a lock for query state.",
        ))
        .unwrap();
    store.verify(second.id, "source").unwrap();
    let context = store
        .remember(NewMemory::procedure(
            "a",
            "crowding",
            format!(
                "{}Use a lock for the contextual fact.",
                "crowding ".repeat(8)
            ),
        ))
        .unwrap();
    store.verify(context.id, "context source").unwrap();
    index.backfill("a", &FocusModel).unwrap();
    for task in [
        "SharedMemoryStore RetrievalQuery crowding",
        "SharedMemoryStore,RetrievalQuery,crowding",
        "SharedMemoryStore;RetrievalQuery;crowding",
    ] {
        let query = RetrievalQuery::new("a", task);
        let before = index
            .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
            .unwrap();
        let rank = before
            .iter()
            .position(|h| h.record.id == context.id)
            .unwrap();
        assert_eq!(rank, 7);
        let after = index.retrieve(&query, &FocusModel, 0.8).unwrap();
        assert_eq!(after[rank].record.id, context.id);
        assert!(after.iter().any(|h| h.record.id == first));
        assert!(after.iter().any(|h| h.record.id == second.id));
    }
}
