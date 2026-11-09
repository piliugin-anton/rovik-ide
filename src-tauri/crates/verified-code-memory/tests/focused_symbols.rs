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

fn reserve_context_slot(store: &SharedMemoryStore) {
    // Seven crowding records plus the context still fill all eight slots.
    // A ninth equally ranked record can randomly exclude the context before
    // the policy under test even runs, depending on generated UUID ties.
    let spare = store.list("a", "Crowding observation 7").unwrap();
    assert_eq!(spare.len(), 1);
    store.delete(spare[0].id).unwrap();
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
    let file_query = query.clone().with_files(["src/noise.rs"]);
    let file_hits = index.retrieve(&file_query, &FocusModel, 0.8).unwrap();
    assert_eq!(file_hits.last().unwrap().record.id, wanted);
    let file_denied = index
        .retrieve_filtered(&file_query, &FocusModel, 0.8, |r| r.id != wanted)
        .unwrap();
    assert_eq!(ids(&file_denied), ids(&denied));
    store.deprecate(wanted, "obsolete").unwrap();
    let obsolete = index.retrieve(&query, &FocusModel, 0.8).unwrap();
    assert_eq!(ids(&obsolete), ids(&denied));
    let file_obsolete = index.retrieve(&file_query, &FocusModel, 0.8).unwrap();
    assert_eq!(ids(&file_obsolete), ids(&file_denied));
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
    // Seven crowding records plus the wanted record fill the semantic pool.
    // An eighth crowding record could lead the lexical pool but fall outside
    // the semantic limit, tying the wanted record's single-pool RRF score.
    // UUID ordering then decided whether this test reached its precondition.
    for n in 0..7 {
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
    let semantic = index.search(&query, &[1.0, 0.0], 0.8).unwrap();
    assert_eq!(semantic.len(), 8);
    assert_eq!(semantic[0].0.id, wanted.id);
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
    reserve_context_slot(&store);
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

#[test]
fn focused_file_requests_recover_named_facts_without_displacing_the_tail_file_match() {
    let (store, index, first) = fixture();
    reserve_context_slot(&store);
    let second = store
        .remember(NewMemory::procedure(
            "a",
            "Retrieval query",
            "Use a lock for query state.",
        ))
        .unwrap();
    store.verify(second.id, "source").unwrap();
    let context = store
        .remember(
            NewMemory::procedure(
                "a",
                "File observation",
                format!("{}Use a lock for file state.", "crowding ".repeat(8)),
            )
            .with_files(["src/noise.rs"]),
        )
        .unwrap();
    store.verify(context.id, "file evidence").unwrap();
    index.backfill("a", &FocusModel).unwrap();
    for task in [
        "SharedMemoryStore crowding",
        "SharedMemoryStore,RetrievalQuery crowding",
        "crate::SharedMemoryStore;crate::RetrievalQuery crowding",
    ] {
        let query = RetrievalQuery::new("a", task).with_files(["./src\\noise.rs"]);
        let before = index
            .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
            .unwrap();
        assert_eq!(before.len(), 8);
        assert_eq!(before[7].record.id, context.id);
        assert!(before.iter().all(|hit| hit.record.id != first));
        let after = index.retrieve(&query, &FocusModel, 0.8).unwrap();
        assert_eq!(after.len(), 8);
        assert_eq!(after[0].record.id, before[0].record.id);
        assert_eq!(after[7].record.id, context.id);
        assert!(after.iter().any(|hit| hit.record.id == first));
        if task.contains("RetrievalQuery") {
            assert!(after.iter().any(|hit| hit.record.id == second.id));
        }
        for incumbent in before.iter().filter(|hit| {
            hit.record.files.iter().any(|file| {
                query
                    .files
                    .iter()
                    .any(|supplied| verified_code_memory::retrieval::path_overlap(file, supplied))
            })
        }) {
            assert_eq!(
                after
                    .iter()
                    .position(|hit| hit.record.id == incumbent.record.id),
                before
                    .iter()
                    .position(|hit| hit.record.id == incumbent.record.id)
            );
        }
    }
    let denied =
        RetrievalQuery::new("a", "SharedMemoryStore crowding").with_files(["src/noise.rs"]);
    let before = index
        .retrieve(&denied.clone().with_failure(""), &FocusModel, 0.8)
        .unwrap();
    assert_eq!(
        ids(&index
            .retrieve_filtered(&denied, &FocusModel, 0.8, |record| record.id != first)
            .unwrap()),
        ids(&before)
    );
}

#[test]
fn a_file_pool_with_insufficient_unprotected_slots_aborts_all_named_additions() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    for n in 0..8 {
        let memory = NewMemory::procedure(
            "a",
            format!("Crowding observation {n}"),
            format!("Crowding sample {n} records a distinct observation."),
        );
        let memory = if n < 7 {
            memory.with_files(["src/context.rs"])
        } else {
            memory
        };
        let record = store.remember(memory).unwrap();
        store.verify(record.id, "file evidence").unwrap();
    }
    let first = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store",
            "Use a lock for shared state.",
        ))
        .unwrap();
    store.verify(first.id, "source").unwrap();
    let second = store
        .remember(NewMemory::procedure(
            "a",
            "Retrieval query",
            "Use a lock for query state.",
        ))
        .unwrap();
    store.verify(second.id, "source").unwrap();
    let index = SemanticIndex::new(store, "file-fixture", 2).unwrap();
    index.backfill("a", &FocusModel).unwrap();
    let query = RetrievalQuery::new("a", "SharedMemoryStore,RetrievalQuery crowding")
        .with_files(["src/context.rs"]);
    let before = index
        .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
        .unwrap();
    assert_eq!(before.len(), 8);
    assert_eq!(
        before
            .iter()
            .filter(|hit| !hit.record.files.is_empty())
            .count(),
        7
    );
    let after = index.retrieve(&query, &FocusModel, 0.8).unwrap();
    assert_eq!(ids(&after), ids(&before));
    assert!(after
        .iter()
        .all(|hit| hit.record.id != first.id && hit.record.id != second.id));
}

#[test]
fn insufficient_file_coverage_capacity_keeps_the_successful_name_only_plan() {
    struct CapacityModel;
    impl MemoryEmbedder for CapacityModel {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            if text.starts_with("Crowding observation")
                && !text.starts_with("Crowding observation 7")
            {
                // Give the unprotected record a strict single-pool lead over
                // the file claim, while seven file matches win both pools.
                Ok(vec![0.9, 0.4358899])
            } else {
                FocusModel.embed(text)
            }
        }
    }
    let store = SharedMemoryStore::open_in_memory().unwrap();
    for n in 0..8 {
        let input = NewMemory::procedure(
            "a",
            format!("Crowding observation {n}"),
            format!("Crowding sample {n} is a separate observation."),
        );
        let input = if n < 7 {
            input.with_files(["src/context.rs"])
        } else {
            input
        };
        let record = store.remember(input).unwrap();
        store.verify(record.id, "source").unwrap();
    }
    let wanted = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store",
            "Use a lock for shared state.",
        ))
        .unwrap();
    store.verify(wanted.id, "source").unwrap();
    let file = store
        .remember(
            NewMemory::procedure("a", "Other file", "Use a lock for other state.")
                .with_files(["src/other.rs"]),
        )
        .unwrap();
    store.verify(file.id, "source").unwrap();
    let index = SemanticIndex::new(store, "file-capacity-fixture", 2).unwrap();
    index.backfill("a", &CapacityModel).unwrap();
    let query = RetrievalQuery::new("a", "SharedMemoryStore crowding")
        .with_files(["src/context.rs", "src/other.rs"]);
    let initial = index
        .retrieve(&query.clone().with_failure(""), &CapacityModel, 0.8)
        .unwrap();
    assert_eq!(initial.len(), 8);
    assert!(initial[7].record.files.is_empty());
    assert!(initial.iter().all(|hit| hit.record.id != file.id));
    let after = index.retrieve(&query, &CapacityModel, 0.8).unwrap();
    assert_eq!(&ids(&after)[..7], &ids(&initial)[..7]);
    assert_eq!(after[7].record.id, wanted.id);
}

#[test]
fn focused_file_coverage_preserves_named_ranks_and_filters_before_its_file_pool_bound() {
    let (store, index, first) = fixture();
    let second = store
        .remember(NewMemory::procedure(
            "a",
            "Retrieval query",
            "Use a lock for queries.",
        ))
        .unwrap();
    store.verify(second.id, "source").unwrap();
    let mut files = Vec::new();
    for path in ["src/noise.rs", "src/dispatch.rs"] {
        let record = store
            .remember(
                NewMemory::procedure(
                    "a",
                    format!("Context for {path}"),
                    "Use a lock for file state.",
                )
                .with_files([path]),
            )
            .unwrap();
        store.verify(record.id, "source").unwrap();
        files.push(record.id);
    }
    for n in 0..45 {
        for (repo, verified, tags) in [
            ("a", true, vec!["hidden"]),
            ("a", false, vec![]),
            ("other", true, vec![]),
        ] {
            let record = store
                .remember(
                    NewMemory::procedure(repo, format!("Private file {n}"), "Use a lock.")
                        .with_files(["src/noise.rs", "src/dispatch.rs"])
                        .with_tags(tags),
                )
                .unwrap();
            if verified {
                store.verify(record.id, "source").unwrap();
            }
        }
    }
    let collision = store
        .remember(
            NewMemory::procedure("a", "Suffix collision", "Use a lock.")
                .with_files(["src/notnoise.rs"]),
        )
        .unwrap();
    store.verify(collision.id, "source").unwrap();
    index.backfill("a", &FocusModel).unwrap();
    let query = RetrievalQuery::new("a", "SharedMemoryStore,RetrievalQuery crowding")
        .with_files(["./src\\noise.rs", "src/dispatch.rs", "src/unknown.rs"])
        .with_verifications([Verification::Verified])
        .without_tags(["hidden"]);
    let initial = index
        .retrieve(&query.clone().with_failure(""), &FocusModel, 0.8)
        .unwrap();
    assert!(initial.iter().all(|hit| !files.contains(&hit.record.id)));
    let names_only = index
        .retrieve_filtered(&query, &FocusModel, 0.8, |record| {
            !files.contains(&record.id)
        })
        .unwrap();
    let after = index.retrieve(&query, &FocusModel, 0.8).unwrap();
    assert_eq!(after.len(), 8);
    assert_eq!(after[0].record.id, names_only[0].record.id);
    for id in [first, second.id] {
        let rank = names_only
            .iter()
            .position(|hit| hit.record.id == id)
            .unwrap();
        assert_eq!(after[rank].record.id, id);
    }
    for id in &files {
        assert_eq!(after.iter().filter(|hit| hit.record.id == *id).count(), 1);
    }
    assert!(after.iter().all(|hit| hit.record.id != collision.id
        && hit.record.repo == "a"
        && hit.record.verification == Verification::Verified
        && !hit.record.tags.iter().any(|tag| tag == "hidden")));
    let denied = index
        .retrieve_filtered(&query, &FocusModel, 0.8, |record| record.id != files[0])
        .unwrap();
    assert!(denied.iter().all(|hit| hit.record.id != files[0]));
    assert!(denied.iter().any(|hit| hit.record.id == files[1]));
}

#[test]
fn one_file_fact_covers_multiple_supplied_paths_without_duplicate_slots() {
    let (store, index, wanted) = fixture();
    let file = store
        .remember(
            NewMemory::procedure("a", "Shared file context", "Use a lock for file state.")
                .with_files(["src/noise.rs", "src/dispatch.rs"]),
        )
        .unwrap();
    store.verify(file.id, "source").unwrap();
    index.backfill("a", &FocusModel).unwrap();
    let query = RetrievalQuery::new("a", "SharedMemoryStore crowding").with_files([
        "./src\\noise.rs",
        "src/dispatch.rs",
        "src/noise.rs",
    ]);
    let after = index.retrieve(&query, &FocusModel, 0.8).unwrap();
    assert_eq!(after.len(), 8);
    assert!(after.iter().any(|hit| hit.record.id == wanted));
    assert_eq!(
        after.iter().filter(|hit| hit.record.id == file.id).count(),
        1
    );
    let (_, shown) = store.render_packet_traced(&after, 1000);
    assert!(shown.contains(&wanted) && shown.contains(&file.id));
}
#[test]
fn optional_title_coverage_preserves_the_legacy_packet_when_protected_full_text_would_be_lost() {
    struct TitleModel;
    impl MemoryEmbedder for TitleModel {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            Ok(if text.starts_with("Cross references") {
                vec![1.0, 0.0]
            } else if text.starts_with("Crowding") {
                vec![0.9, 0.4358899]
            } else if text.contains("lock") {
                vec![0.0, 1.0]
            } else {
                vec![1.0, 0.0]
            })
        }
    }
    // Strict file weights keep the single-pool file result above the omitted
    // crowding result, independently of UUID and timestamp ties.
    let store = SharedMemoryStore::new(
        verified_code_memory::MemoryStore::open_in_memory()
            .unwrap()
            .with_weights(verified_code_memory::RetrievalWeights {
                file_match: 10.0,
                ..Default::default()
            }),
    );
    for n in 0..7 {
        let record = store
            .remember(NewMemory::procedure(
                "a",
                format!("Crowding observation {n}"),
                format!("Independent crowding observation {n}."),
            ))
            .unwrap();
        store.verify(record.id, "source").unwrap();
    }
    let first = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store",
            format!("Use a lock. {}", "Durable detail. ".repeat(12)),
        ))
        .unwrap()
        .id;
    store.verify(first, "source").unwrap();
    let second = store
        .remember(NewMemory::procedure(
            "a",
            "Retrieval query",
            format!("Use a lock. {}", "Separate detail. ".repeat(12)),
        ))
        .unwrap();
    store.verify(second.id, "source").unwrap();
    let reference = store
        .remember(
            NewMemory::procedure(
                "a",
                "Cross references",
                "Shared memory store and retrieval query crowding references.",
            )
            .with_files(["src/noise.rs"]),
        )
        .unwrap();
    store.verify(reference.id, "source").unwrap();
    let file =
        store
            .remember(
                NewMemory::procedure("a", "File context", "Use a lock for file state.")
                    .with_files(["src/noise.rs", "src/other.rs", "src/third.rs"]),
            )
            .unwrap();
    store.verify(file.id, "source").unwrap();
    let index = SemanticIndex::new(store.clone(), "title-fixture", 2).unwrap();
    for n in 0..90 {
        let hidden = store
            .remember(
                NewMemory::procedure(
                    "a",
                    "SHARED MEMORY STORE",
                    format!("Use a lock for private state {n}."),
                )
                .with_tags(["caller-hidden"]),
            )
            .unwrap();
        store.verify(hidden.id, "private source").unwrap();
    }
    // Visible title alternatives must not fill the bounded supplementary pool
    // with claims that cannot possibly render under the supplied allowance.
    for n in 0..90 {
        let oversized = store
            .remember(NewMemory::procedure(
                "a",
                "shared memory store",
                format!(
                    "Use a lock for archive {n}. {}",
                    "Shared memory store reference; ".repeat(400)
                ),
            ))
            .unwrap();
        store.verify(oversized.id, "archived source").unwrap();
    }
    index.backfill("a", &TitleModel).unwrap();
    let visible = |record: &verified_code_memory::MemoryRecord| {
        !record.tags.iter().any(|tag| tag == "caller-hidden")
    };
    let query = RetrievalQuery::new("a", "SharedMemoryStore,RetrievalQuery crowding").with_files([
        "src/noise.rs",
        "src/other.rs",
        "src/third.rs",
    ]);
    let legacy = index
        .retrieve_filtered(&query, &TitleModel, 0.8, visible)
        .unwrap();
    assert_eq!(legacy.len(), 8);
    assert_eq!(legacy[0].record.id, reference.id);
    assert_eq!(legacy[7].record.id, file.id);
    assert!(!ids(&legacy).contains(&first));
    let after = index
        .retrieve_filtered(
            &query.clone().with_packet_budget(1000),
            &TitleModel,
            0.8,
            visible,
        )
        .unwrap();
    assert!(ids(&after).contains(&first) && ids(&after).contains(&second.id));
    assert_eq!(after[0].record.id, reference.id);
    assert_eq!(after[7].record.id, file.id);
    let mut unsafe_trial = legacy.clone();
    for (slot, id) in [(6, first), (5, second.id)] {
        unsafe_trial[slot].record = store.get(id).unwrap();
    }
    let pressure = (100..1000)
        .find(|budget| {
            let before = store.render_packet_with_evidence(&legacy, *budget);
            let trial = store.render_packet_with_evidence(&unsafe_trial, *budget);
            before.complete_ids.contains(&file.id)
                && trial.complete_ids.contains(&first)
                && trial.complete_ids.contains(&second.id)
                && !trial.complete_ids.contains(&file.id)
        })
        .expect("fixture must expose a full-claim loss despite fitting both new titles");
    let safe = index
        .retrieve_filtered(
            &query.with_packet_budget(pressure),
            &TitleModel,
            0.8,
            visible,
        )
        .unwrap();
    assert_eq!(ids(&safe), ids(&legacy));
    assert!(store
        .render_packet_with_evidence(&safe, pressure)
        .complete_ids
        .contains(&file.id));
}
