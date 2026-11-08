use verified_code_memory::{
    semantic::SemanticIndex, MemoryEmbedder, NewMemory, RetrievalQuery, SharedMemoryStore,
    Verification,
};

struct Embed;
impl MemoryEmbedder for Embed {
    fn dimensions(&self) -> usize {
        2
    }
    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        Ok(if text.contains("lock") || text.contains("serialize") {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        })
    }
}

#[test]
fn completion_indexes_past_three_and_resumes_without_reembedding_current_claims() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Count(AtomicUsize);
    impl MemoryEmbedder for Count {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Embed.embed(text)
        }
    }
    let store = SharedMemoryStore::open_in_memory().unwrap();
    for n in 0..12 {
        store
            .remember(NewMemory::procedure(
                "a",
                format!("lock-{n}"),
                format!("lock procedure {n}"),
            ))
            .unwrap();
    }
    store
        .remember(NewMemory::procedure("b", "foreign", "lock"))
        .unwrap();
    let old = store
        .remember(NewMemory::procedure("a", "obsolete", "lock"))
        .unwrap();
    store.deprecate(old.id, "replaced").unwrap();
    let index = SemanticIndex::new(store, "v1", 2).unwrap();
    let embedder = Count(AtomicUsize::new(0));
    assert_eq!(index.backfill_limited("a", &embedder, 3).unwrap(), 3);
    assert_eq!(index.backfill_while("a", &embedder, || true).unwrap(), 9);
    assert_eq!(index.backfill_while("a", &embedder, || true).unwrap(), 0);
    assert_eq!(embedder.0.load(Ordering::SeqCst), 12);
    let query = RetrievalQuery::new("a", "serialize").with_limit(32);
    assert_eq!(index.search(&query, &[1.0, 0.0], 0.8).unwrap().len(), 12);
}

#[test]
fn cancellation_rejects_inflight_vectors_and_allows_a_later_resume() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Count(AtomicUsize);
    impl MemoryEmbedder for Count {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Embed.embed(text)
        }
    }
    let store = SharedMemoryStore::open_in_memory().unwrap();
    for n in 0..12 {
        store
            .remember(NewMemory::procedure(
                "a",
                format!("lock-{n}"),
                format!("lock procedure {n}"),
            ))
            .unwrap();
    }
    let index = SemanticIndex::new(store.clone(), "v1", 2).unwrap();
    let embedder = Count(AtomicUsize::new(0));
    assert_eq!(index.backfill_while("a", &embedder, || false).unwrap(), 0);
    assert_eq!(embedder.0.load(Ordering::SeqCst), 0);
    assert_eq!(
        index
            .backfill_while("a", &embedder, || {
                // Reading the store in the predicate must not deadlock either.
                assert_eq!(store.list("a", "").unwrap().len(), 12);
                embedder.0.load(Ordering::SeqCst) < 4
            })
            .unwrap(),
        3
    );
    let query = RetrievalQuery::new("a", "serialize").with_limit(32);
    assert_eq!(index.search(&query, &[1.0, 0.0], 0.8).unwrap().len(), 3);
    assert_eq!(index.backfill_while("a", &embedder, || true).unwrap(), 9);
    assert_eq!(embedder.0.load(Ordering::SeqCst), 13);
}

#[test]
fn short_query_calibration_recovers_topic_queries_without_relaxing_long_or_generic_requests() {
    struct Topic;
    impl MemoryEmbedder for Topic {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            Ok(if text.contains("migration") {
                vec![0.28, 0.96]
            } else {
                vec![1.0, 0.0]
            })
        }
    }
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure(
            "a",
            "migration",
            "Apply migrations in sequence within a transaction.",
        ))
        .unwrap();
    let index = SemanticIndex::new(store, "fixture", 2).unwrap();
    index.backfill("a", &Topic).unwrap();
    let query = RetrievalQuery::new("a", "database upgrade");
    assert!(index.retrieve(&query, &Topic, 0.3).unwrap().is_empty());
    let calibrated = index
        .clone()
        .with_short_query_scale(2, 11.0 / 12.0)
        .unwrap();
    let hits = calibrated.retrieve(&query, &Topic, 0.3).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.id, wanted.id);
    assert!(calibrated
        .search(&query, &[1.0, 0.0], 0.3)
        .unwrap()
        .is_empty());
    for task in ["database upgrade safely", "do this"] {
        assert!(calibrated
            .retrieve(&RetrievalQuery::new("a", task), &Topic, 0.3)
            .unwrap()
            .is_empty());
    }
    assert!(index
        .with_short_query_scale(2, 1.0)
        .unwrap()
        .retrieve(&query, &Topic, 0.3)
        .unwrap()
        .is_empty());
}

#[test]
fn invalid_short_query_calibration_is_rejected() {
    let index =
        SemanticIndex::new(SharedMemoryStore::open_in_memory().unwrap(), "fixture", 2).unwrap();
    for (words, scale) in [(0, 0.9), (17, 0.9), (2, -0.1), (2, 1.1), (2, f32::NAN)] {
        assert!(index.clone().with_short_query_scale(words, scale).is_err());
    }
    let calibrated = index.with_short_query_scale(2, 0.5).unwrap();
    assert!(calibrated
        .retrieve(&RetrievalQuery::new("a", "serialize"), &Embed, 1.1)
        .is_err());
}

#[test]
fn semantic_candidates_find_paraphrases_without_crossing_scope_or_filters() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure("a", "lock", "lock"))
        .unwrap();
    store.verify(wanted.id, "regression").unwrap();
    store
        .remember(NewMemory::episode("b", "lock", "lock"))
        .unwrap();
    store
        .remember(NewMemory::episode("a", "lock", "lock").with_tags(["hidden"]))
        .unwrap();
    let index = SemanticIndex::new(store.clone(), "fixture-v1", 2).unwrap();
    assert_eq!(index.backfill("a", &Embed).unwrap(), 2);
    index.backfill("b", &Embed).unwrap();
    let query = RetrievalQuery::new("a", "serialize")
        .with_verifications([Verification::Verified])
        .without_tags(["hidden"]);
    assert!(store.retrieve(&query).unwrap().is_empty());
    let found = index.retrieve(&query, &Embed, 0.8).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].record.id, wanted.id);
    assert!(index.search(&query, &[0.0, 1.0], 0.8).unwrap().is_empty());
}

#[test]
fn ineligible_candidates_cannot_consume_a_context_slot() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::episode("a", "lock", "lock"))
        .unwrap();
    for _ in 0..8 {
        let hidden = store
            .remember(NewMemory::procedure("a", "lock", "lock"))
            .unwrap();
        store.verify(hidden.id, "fixture").unwrap();
    }
    let query = RetrievalQuery::new("a", "lock").with_limit(1);
    assert_ne!(store.retrieve(&query).unwrap()[0].record.id, wanted.id);
    let found = store
        .retrieve_filtered(&query, |record| record.id == wanted.id)
        .unwrap();
    assert_eq!(found[0].record.id, wanted.id);
    let index = SemanticIndex::new(store, "fixture-v1", 2).unwrap();
    index.backfill("a", &Embed).unwrap();
    let found = index
        .retrieve_filtered(&query, &Embed, 0.8, |record| record.id == wanted.id)
        .unwrap();
    assert_eq!(found[0].record.id, wanted.id);
}

#[test]
fn edited_deleted_and_wrong_model_vectors_cannot_be_retrieved() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let record = store
        .remember(NewMemory::episode("a", "lock", "lock"))
        .unwrap();
    let index = SemanticIndex::new(store.clone(), "v1", 2).unwrap();
    index.backfill("a", &Embed).unwrap();
    assert_eq!(index.backfill("a", &Embed).unwrap(), 0);
    let q = RetrievalQuery::new("a", "serialize");
    assert_eq!(index.search(&q, &[1.0, 0.0], 0.8).unwrap().len(), 1);
    assert!(SemanticIndex::new(store.clone(), "v2", 2)
        .unwrap()
        .search(&q, &[1.0, 0.0], 0.8)
        .unwrap()
        .is_empty());
    store.edit(record.id, "mail", "mail").unwrap();
    assert!(index.search(&q, &[1.0, 0.0], 0.8).unwrap().is_empty());
    index.backfill("a", &Embed).unwrap();
    assert!(index.search(&q, &[1.0, 0.0], 0.8).unwrap().is_empty());
    store.delete(record.id).unwrap();
    assert!(index.search(&q, &[0.0, 1.0], 0.8).unwrap().is_empty());
}

#[test]
fn a_late_embedding_cannot_undo_clear_and_does_not_hold_the_store_lock() {
    struct Clearing(SharedMemoryStore);
    impl MemoryEmbedder for Clearing {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, _: &str) -> verified_code_memory::Result<Vec<f32>> {
            self.0.clear("a")?;
            Ok(vec![1.0, 0.0])
        }
    }
    let store = SharedMemoryStore::open_in_memory().unwrap();
    store
        .remember(NewMemory::episode("a", "lock", "lock"))
        .unwrap();
    let index = SemanticIndex::new(store.clone(), "v1", 2).unwrap();
    assert_eq!(index.backfill("a", &Clearing(store.clone())).unwrap(), 0);
    assert!(index
        .search(&RetrievalQuery::new("a", "lock"), &[1.0, 0.0], 0.8)
        .unwrap()
        .is_empty());
}

#[test]
fn invalid_vectors_and_thresholds_fail_instead_of_poisoning_rankings() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let index = SemanticIndex::new(store, "v1", 2).unwrap();
    let q = RetrievalQuery::new("a", "lock");
    for bad in [&[f32::NAN, 1.0][..], &[0.0, 0.0], &[1.0]] {
        assert!(index.search(&q, bad, 0.5).is_err());
    }
    assert!(index.search(&q, &[1.0, 0.0], f32::NAN).is_err());
}

#[test]
fn manager_pages_have_no_duplicates_and_cursors_cannot_cross_projects() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    for n in 0..101 {
        store
            .remember(NewMemory::episode("a", format!("record {n}"), "content"))
            .unwrap();
    }
    let first = store.list_page("a", "", None).unwrap();
    assert_eq!(first.len(), 50);
    let cursor = first.last().unwrap().page_cursor();
    store
        .record_outcome(first.last().unwrap().id, true)
        .unwrap();
    let second = store.list_page("a", "", Some(&cursor)).unwrap();
    assert_eq!(second.len(), 50);
    assert!(second.iter().all(|r| first.iter().all(|f| f.id != r.id)));
    let third = store
        .list_page("a", "", Some(&second.last().unwrap().page_cursor()))
        .unwrap();
    assert_eq!(third.len(), 1);
    assert!(matches!(
        store.list_page("b", "", Some(&cursor)),
        Err(verified_code_memory::VcmError::InvalidCursor)
    ));
}

#[test]
fn a_future_schema_is_refused_without_changing_its_version_or_contents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA user_version=999; CREATE TABLE future_data(value TEXT); INSERT INTO future_data VALUES('preserve');").unwrap();
    assert!(matches!(
        SharedMemoryStore::open(&path),
        Err(verified_code_memory::VcmError::UnsupportedSchema { actual: 999, .. })
    ));
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        999
    );
    assert_eq!(
        connection
            .query_row("SELECT value FROM future_data", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
}

#[test]
fn successful_run_associations_do_not_manufacture_retrieval_trust() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let record = store
        .remember(NewMemory::episode("a", "lock", "lock"))
        .unwrap();
    let query = RetrievalQuery::new("a", "lock");
    let before = store.retrieve(&query).unwrap()[0].score;
    for _ in 0..20 {
        store.record_outcome(record.id, true).unwrap();
    }
    let after = store.retrieve(&query).unwrap();
    assert_eq!(after[0].score, before);
    assert_eq!(after[0].components.outcome, 0.0);
}
