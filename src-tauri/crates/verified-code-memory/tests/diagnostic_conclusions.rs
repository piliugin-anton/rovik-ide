use std::sync::atomic::{AtomicUsize, Ordering};
use verified_code_memory::{
    semantic::SemanticIndex, MemoryEmbedder, NewMemory, RetrievalQuery, SharedMemoryStore,
    Verification,
};

const WINDOW: usize = 64;

#[derive(Default)]
struct CrossWindow {
    calls: AtomicUsize,
    fail_diagnostic: bool,
}

impl MemoryEmbedder for CrossWindow {
    fn dimensions(&self) -> usize {
        3
    }

    fn input_character_limit(&self) -> Option<usize> {
        Some(WINDOW)
    }

    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if text == "recover" && self.fail_diagnostic {
            return Err(verified_code_memory::VcmError::EmbeddingUnavailable(
                "fixture diagnostic unavailable",
            ));
        }
        let visible: String = text.chars().take(WINDOW).collect();
        let visible = visible.to_lowercase();
        Ok(
            if visible.contains("recover") || visible.contains("use a lock") {
                vec![0.0, 1.0, 0.0]
            } else if visible.starts_with("wanted")
                || visible.starts_with("ordinary")
                || visible.starts_with("denied")
            {
                vec![0.0, 0.0, 1.0]
            } else {
                vec![1.0, 0.0, 0.0]
            },
        )
    }
}

fn note(conclusion: &str) -> String {
    format!("{}\n\n{conclusion}", "Preliminary material. ".repeat(8))
}

fn query() -> RetrievalQuery {
    RetrievalQuery::new("a", "界".repeat(WINDOW)).with_failure("recover")
}

#[test]
fn independent_diagnostics_search_hidden_conclusions_after_all_incumbent_views() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let context = store
        .remember(NewMemory::procedure(
            "a",
            "Context",
            "An existing observation.",
        ))
        .unwrap();
    let ordinary = store
        .remember(NewMemory::procedure(
            "a",
            "Ordinary",
            note("Original conclusion."),
        ))
        .unwrap();
    let wanted = store
        .remember(NewMemory::procedure("a", "Wanted", note("Use a lock.")))
        .unwrap();
    let model = CrossWindow::default();
    let index = SemanticIndex::new(store.clone(), "cross-v1", 3).unwrap();
    index.backfill("a", &model).unwrap();
    assert!(index
        .search(&query(), &[0.0, 1.0, 0.0], 0.8)
        .unwrap()
        .is_empty());
    model.calls.store(0, Ordering::SeqCst);
    let hits = index.retrieve(&query(), &model, 0.8).unwrap();
    assert_eq!(
        hits.iter().map(|h| h.record.id).collect::<Vec<_>>(),
        [context.id, ordinary.id, wanted.id]
    );
    assert!(hits[2].reason.contains("diagnostic conclusion cosine"));
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        2,
        "reuse the diagnostic inference"
    );
    assert!(hits.windows(2).all(|pair| pair[0].score > pair[1].score));
    let (packet, shown) = store.render_packet_traced(&hits, 1000);
    assert!(shown.contains(&wanted.id));
    assert!(packet.contains(&wanted.content));
    // The original conclusion has priority when only its slot remains.
    let full = index.retrieve(&query().with_limit(2), &model, 0.8).unwrap();
    assert_eq!(
        full.iter().map(|h| h.record.id).collect::<Vec<_>>(),
        [context.id, ordinary.id]
    );
    model.calls.store(0, Ordering::SeqCst);
    let full = index.retrieve(&query().with_limit(1), &model, 0.8).unwrap();
    assert_eq!(full[0].record.id, context.id);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn diagnostic_conclusions_filter_visibility_scope_trust_and_tags_before_their_limit() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure("a", "Wanted", note("Use a lock.")))
        .unwrap();
    store.verify(wanted.id, "evidence").unwrap();
    let denied = store
        .remember(NewMemory::procedure("a", "Denied", note("Use a lock.")))
        .unwrap();
    store.verify(denied.id, "evidence").unwrap();
    for n in 0..40 {
        let hidden = store
            .remember(
                NewMemory::procedure("a", format!("Wanted private {n}"), note("Use a lock."))
                    .with_tags(["private"]),
            )
            .unwrap();
        store.verify(hidden.id, "evidence").unwrap();
    }
    store
        .remember(NewMemory::procedure(
            "a",
            "Wanted unverified",
            note("Use a lock."),
        ))
        .unwrap();
    store
        .remember(NewMemory::procedure(
            "b",
            "Wanted foreign",
            note("Use a lock."),
        ))
        .unwrap();
    let model = CrossWindow::default();
    let index = SemanticIndex::new(store, "cross-v1", 3).unwrap();
    index.backfill("a", &model).unwrap();
    index.backfill("b", &model).unwrap();
    let hits = index
        .retrieve_filtered(
            &query()
                .with_limit(1)
                .with_verifications([Verification::Verified])
                .without_tags(["private"]),
            &model,
            0.8,
            |r| r.id != denied.id,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.id, wanted.id);
}

#[test]
fn unavailable_diagnostics_preserve_successful_primary_and_conclusion_results() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let context = store
        .remember(NewMemory::procedure(
            "a",
            "Context",
            "An existing observation.",
        ))
        .unwrap();
    let ordinary = store
        .remember(NewMemory::procedure(
            "a",
            "Ordinary",
            note("Original conclusion."),
        ))
        .unwrap();
    store
        .remember(NewMemory::procedure("a", "Wanted", note("Use a lock.")))
        .unwrap();
    let index = SemanticIndex::new(store, "cross-v1", 3).unwrap();
    let model = CrossWindow::default();
    index.backfill("a", &model).unwrap();
    let unavailable = CrossWindow {
        fail_diagnostic: true,
        ..Default::default()
    };
    let hits = index.retrieve(&query(), &unavailable, 0.8).unwrap();
    assert_eq!(
        hits.iter().map(|h| h.record.id).collect::<Vec<_>>(),
        [context.id, ordinary.id]
    );
}
