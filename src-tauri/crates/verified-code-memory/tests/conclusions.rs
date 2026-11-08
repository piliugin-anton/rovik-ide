use std::sync::atomic::{AtomicUsize, Ordering};
use verified_code_memory::{
    semantic::SemanticIndex, MemoryEmbedder, NewMemory, RetrievalQuery, SharedMemoryStore,
    Verification,
};

const WINDOW: usize = 64;

struct Windowed {
    advertised: bool,
    calls: AtomicUsize,
}

impl Windowed {
    fn new(advertised: bool) -> Self {
        Self {
            advertised,
            calls: AtomicUsize::new(0),
        }
    }
}

impl MemoryEmbedder for Windowed {
    fn dimensions(&self) -> usize {
        2
    }

    fn input_character_limit(&self) -> Option<usize> {
        self.advertised.then_some(WINDOW)
    }

    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let visible: String = text.chars().take(WINDOW).collect();
        Ok(
            if visible.contains("lock") || visible.contains("serialize") {
                vec![1.0, 0.0]
            } else {
                vec![0.0, 1.0]
            },
        )
    }
}

fn long_claim() -> String {
    format!(
        "{}\n\nUse a lock to preserve integrity.\n\n",
        "背景 material. ".repeat(12)
    )
}

#[test]
fn hidden_conclusions_fill_free_slots_without_changing_primary_search_or_incumbents() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure("a", "Conclusion", long_claim()))
        .unwrap();
    let context = store
        .remember(
            NewMemory::procedure("a", "Context", "An existing observation.")
                .with_files(["src/context.rs"]),
        )
        .unwrap();
    let index = SemanticIndex::new(store.clone(), "window-v1", 2).unwrap();
    let embedder = Windowed::new(true);
    assert_eq!(index.backfill("a", &embedder).unwrap(), 2);
    assert_eq!(index.backfill("a", &embedder).unwrap(), 0);
    let query = RetrievalQuery::new("a", "serialize").with_files(["src/context.rs"]);
    assert_eq!(store.retrieve(&query).unwrap().len(), 1);
    assert!(index.search(&query, &[1.0, 0.0], 0.8).unwrap().is_empty());
    let hits = index.retrieve(&query, &embedder, 0.8).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].record.id, context.id);
    assert_eq!(hits[1].record.id, wanted.id);
    assert!(hits[1].score < hits[0].score);
    let (packet, shown) = store.render_packet_traced(&hits, 1000);
    assert!(shown.contains(&wanted.id));
    assert!(packet.contains(&wanted.content));
    let full = index
        .retrieve(&query.with_limit(1), &embedder, 0.8)
        .unwrap();
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].record.id, context.id);
}

#[test]
fn existing_primary_vectors_gain_conclusions_without_reembedding_the_primary_view() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    store
        .remember(NewMemory::procedure("a", "Conclusion", long_claim()))
        .unwrap();
    let index = SemanticIndex::new(store, "window-v1", 2).unwrap();
    let legacy = Windowed::new(false);
    assert_eq!(index.backfill("a", &legacy).unwrap(), 1);
    let query = RetrievalQuery::new("a", "serialize");
    assert!(index.retrieve(&query, &legacy, 0.8).unwrap().is_empty());
    let upgraded = Windowed::new(true);
    assert_eq!(index.backfill_limited("a", &upgraded, 1).unwrap(), 1);
    assert_eq!(upgraded.calls.load(Ordering::SeqCst), 1);
    assert_eq!(index.backfill("a", &upgraded).unwrap(), 0);
    assert_eq!(index.retrieve(&query, &upgraded, 0.8).unwrap().len(), 1);
}

#[test]
fn conclusion_candidates_apply_scope_trust_tags_and_visibility_before_limits() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure("a", "Conclusion", long_claim()))
        .unwrap();
    store.verify(wanted.id, "evidence").unwrap();
    for n in 0..35 {
        let hidden = store
            .remember(
                NewMemory::procedure("a", format!("Private {n}"), long_claim())
                    .with_tags(["private"]),
            )
            .unwrap();
        store.verify(hidden.id, "evidence").unwrap();
    }
    let excluded = store
        .remember(NewMemory::procedure("a", "Denied", long_claim()))
        .unwrap();
    store.verify(excluded.id, "evidence").unwrap();
    let obsolete = store
        .remember(NewMemory::procedure("a", "Obsolete", long_claim()))
        .unwrap();
    store.verify(obsolete.id, "evidence").unwrap();
    store
        .remember(NewMemory::procedure("a", "Unverified", long_claim()))
        .unwrap();
    store
        .remember(NewMemory::procedure("b", "Foreign", long_claim()))
        .unwrap();
    let index = SemanticIndex::new(store.clone(), "window-v1", 2).unwrap();
    let embedder = Windowed::new(true);
    index.backfill("a", &embedder).unwrap();
    index.backfill("b", &embedder).unwrap();
    store.deprecate(obsolete.id, "replaced").unwrap();
    let query = RetrievalQuery::new("a", "serialize")
        .with_verifications([Verification::Verified])
        .without_tags(["private"])
        .with_limit(1);
    let hits = index
        .retrieve_filtered(&query, &embedder, 0.8, |r| r.id != excluded.id)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.id, wanted.id);
}

#[test]
fn edits_deletes_model_changes_and_index_clear_invalidate_conclusions() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure("a", "Conclusion", long_claim()))
        .unwrap();
    let index = SemanticIndex::new(store.clone(), "window-v1", 2).unwrap();
    let embedder = Windowed::new(true);
    index.backfill("a", &embedder).unwrap();
    let query = RetrievalQuery::new("a", "serialize");
    assert!(SemanticIndex::new(store.clone(), "other", 2)
        .unwrap()
        .retrieve(&query, &embedder, 0.8)
        .unwrap()
        .is_empty());
    assert_eq!(index.clear("a").unwrap(), 1);
    assert!(index.retrieve(&query, &embedder, 0.8).unwrap().is_empty());
    assert_eq!(index.backfill("a", &embedder).unwrap(), 1);
    store.edit(wanted.id, "Conclusion", "Send mail.").unwrap();
    assert!(index.retrieve(&query, &embedder, 0.8).unwrap().is_empty());
    assert_eq!(index.backfill("a", &embedder).unwrap(), 1);
    assert!(index.retrieve(&query, &embedder, 0.8).unwrap().is_empty());
    store.edit(wanted.id, "Conclusion", &long_claim()).unwrap();
    index.backfill("a", &embedder).unwrap();
    assert_eq!(index.retrieve(&query, &embedder, 0.8).unwrap().len(), 1);
    store.delete(wanted.id).unwrap();
    assert!(index.retrieve(&query, &embedder, 0.8).unwrap().is_empty());
}

#[test]
fn cancellation_during_conclusion_inference_rejects_both_late_views() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    store
        .remember(NewMemory::procedure("a", "Conclusion", long_claim()))
        .unwrap();
    let index = SemanticIndex::new(store, "window-v1", 2).unwrap();
    let embedder = Windowed::new(true);
    assert_eq!(
        index
            .backfill_while("a", &embedder, || embedder.calls.load(Ordering::SeqCst) < 2)
            .unwrap(),
        0
    );
    let query = RetrievalQuery::new("a", "serialize");
    assert!(index.retrieve(&query, &embedder, 0.8).unwrap().is_empty());
    assert_eq!(index.backfill("a", &embedder).unwrap(), 1);
    assert_eq!(index.retrieve(&query, &embedder, 0.8).unwrap().len(), 1);
}

#[test]
fn concurrent_edit_delete_or_clear_during_conclusion_embedding_cannot_restore_stale_data() {
    struct Mutating {
        store: SharedMemoryStore,
        record: verified_code_memory::MemoryRecord,
        action: &'static str,
    }
    impl MemoryEmbedder for Mutating {
        fn dimensions(&self) -> usize {
            2
        }
        fn input_character_limit(&self) -> Option<usize> {
            Some(WINDOW)
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            if text.chars().count() <= WINDOW && text.contains("Use a lock") {
                match self.action {
                    "edit" => self
                        .store
                        .edit(self.record.id, "Conclusion", "Send mail.")
                        .map(|_| ())?,
                    "delete" => self.store.delete(self.record.id)?,
                    "clear" => {
                        self.store.clear("a")?;
                    }
                    _ => unreachable!(),
                }
            }
            Windowed::new(true).embed(text)
        }
    }
    for action in ["edit", "delete", "clear"] {
        let store = SharedMemoryStore::open_in_memory().unwrap();
        let record = store
            .remember(NewMemory::procedure("a", "Conclusion", long_claim()))
            .unwrap();
        let index = SemanticIndex::new(store.clone(), "window-v1", 2).unwrap();
        assert_eq!(
            index
                .backfill(
                    "a",
                    &Mutating {
                        store,
                        record,
                        action
                    }
                )
                .unwrap(),
            0
        );
        let query = RetrievalQuery::new("a", "serialize");
        assert!(index
            .retrieve(&query, &Windowed::new(true), 0.8)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn supplementary_embedding_failure_keeps_the_primary_vector_and_can_be_retried() {
    struct Failing;
    impl MemoryEmbedder for Failing {
        fn dimensions(&self) -> usize {
            2
        }
        fn input_character_limit(&self) -> Option<usize> {
            Some(WINDOW)
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            if text.chars().count() <= WINDOW && text.contains("Use a lock") {
                return Err(verified_code_memory::VcmError::EmbeddingUnavailable(
                    "fixture",
                ));
            }
            Windowed::new(true).embed(text)
        }
    }
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let record = store
        .remember(NewMemory::procedure("a", "Conclusion", long_claim()))
        .unwrap();
    let index = SemanticIndex::new(store, "window-v1", 2).unwrap();
    assert_eq!(index.backfill("a", &Failing).unwrap(), 1);
    let query = RetrievalQuery::new("a", "serialize");
    assert_eq!(
        index.search(&query, &[0.0, 1.0], 0.8).unwrap()[0].0.id,
        record.id
    );
    let healthy = Windowed::new(true);
    assert_eq!(index.backfill("a", &healthy).unwrap(), 1);
    assert_eq!(healthy.calls.load(Ordering::SeqCst), 1);
    assert_eq!(index.retrieve(&query, &healthy, 0.8).unwrap().len(), 1);
}

#[test]
fn short_notes_need_no_extra_inference_and_crlf_conclusions_respect_unicode_windows() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    store
        .remember(NewMemory::procedure("a", "Brief", "Use a lock."))
        .unwrap();
    let index = SemanticIndex::new(store.clone(), "window-v1", 2).unwrap();
    let embedder = Windowed::new(true);
    index.backfill("a", &embedder).unwrap();
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 1);
    let long = long_claim().replace('\n', "\r\n");
    let wanted = store
        .remember(NewMemory::procedure("a", "Long", long))
        .unwrap();
    index.backfill("a", &embedder).unwrap();
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 3);
    let hits = index
        .retrieve(&RetrievalQuery::new("a", "serialize"), &embedder, 0.8)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[1].record.id, wanted.id);
}
