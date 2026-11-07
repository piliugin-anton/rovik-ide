use verified_code_memory::{
    deduplicate_memories, Applicability, MemoryEmbedder, MemoryStore, NewMemory, RetrievalQuery,
    RetrievedMemory, SharedMemoryStore, Verification,
};

fn claim() -> NewMemory {
    NewMemory::procedure(
        "p",
        "Retry lock",
        "Serialize retry queue access with a lock.",
    )
    .with_files(["src/retry.rs"])
    .with_evidence("source observation")
}

#[test]
fn exact_copies_free_slots_without_deleting_their_history() {
    let store = MemoryStore::open_in_memory().unwrap();
    for _ in 0..8 {
        store.remember(claim()).unwrap();
    }
    let distinct = store
        .remember(NewMemory::procedure(
            "p",
            "Retry fallback",
            "Retry failures use the fallback.",
        ))
        .unwrap();
    let found = store
        .retrieve(&RetrievalQuery::new("p", "retry lock queue").with_limit(2))
        .unwrap();
    assert_eq!(found.len(), 2);
    assert!(found.iter().any(|hit| hit.record.id == distinct.id));
    assert_eq!(store.list("p", "").unwrap().len(), 9);
}

#[test]
fn distinct_sources_scopes_and_evidence_are_not_interchangeable() {
    let store = MemoryStore::open_in_memory().unwrap();
    let original = store.remember(claim()).unwrap();
    let mut variations = Vec::new();
    let mut changed = original.clone();
    changed.evidence = Some("different evidence".into());
    variations.push(changed);
    let mut changed = original.clone();
    changed.files = vec!["src/other.rs".into()];
    variations.push(changed);
    let mut changed = original.clone();
    changed.code_version = Some("different version".into());
    variations.push(changed);
    let mut changed = original.clone();
    changed.source_episode = Some(uuid::Uuid::new_v4());
    variations.push(changed);
    let mut changed = original.clone();
    changed.tags = vec!["different-source".into()];
    variations.push(changed);
    let mut changed = original.clone();
    changed.identity = "different-conditions".into();
    variations.push(changed);
    let mut changed = original.clone();
    changed.applicability = Applicability::NeedsRevalidation;
    variations.push(changed);
    let mut changed = original.clone();
    changed.verification = Verification::Verified;
    variations.push(changed);
    let mut changed = original.clone();
    changed.content = "A different claim with a deliberately colliding hash".into();
    variations.push(changed);
    for changed in variations {
        assert!(!original.same_context_claim(&changed), "{changed:?}");
    }
    let mut copy = original.clone();
    copy.id = uuid::Uuid::new_v4();
    copy.use_count += 1;
    assert!(original.same_context_claim(&copy));
}

#[test]
fn packet_packing_deduplicates_copies_and_preserves_sibling_claims() {
    let store = MemoryStore::open_in_memory().unwrap();
    let episode = store
        .remember(NewMemory::episode(
            "p",
            "Investigation",
            "Original source observations.",
        ))
        .unwrap();
    let a = store
        .remember(claim().with_source_episode(episode.id))
        .unwrap();
    let b = store
        .remember(
            NewMemory::procedure("p", "Retry deadline", "Bound the retry deadline.")
                .with_source_episode(episode.id),
        )
        .unwrap();
    let mut copy = a.clone();
    copy.id = uuid::Uuid::new_v4();
    let hit = |record| RetrievedMemory {
        record,
        score: 1.0,
        reason: "fixture".into(),
        components: Default::default(),
    };
    let mut hits = vec![hit(a.clone()), hit(copy), hit(b.clone())];
    let (packet, shown) = store.render_packet_traced(&hits, 1000);
    assert_eq!(packet.matches(&a.content).count(), 1);
    assert!(packet.contains(&b.content));
    assert_eq!(shown, vec![a.id, b.id]);
    deduplicate_memories(&mut hits);
    assert_eq!(hits.len(), 2);
}

#[test]
fn repeated_text_does_not_discard_a_new_observation_or_scope() {
    let store = MemoryStore::open_in_memory().unwrap();
    let input = claim().with_identity("stable-claim");
    let first = store
        .remember_if_current(input.clone(), 0)
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .remember_if_current(input.clone(), 0)
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    let changed = store
        .remember_if_current(input.clone().with_evidence("another observation"), 0)
        .unwrap()
        .unwrap();
    assert_ne!(changed.id, first.id);
    let other = store
        .remember_if_current(input.with_files(["src/other.rs"]), 0)
        .unwrap()
        .unwrap();
    assert_ne!(other.id, first.id);
    assert_eq!(store.list("p", "").unwrap().len(), 3);
}

#[test]
fn semantic_and_hybrid_limits_count_distinct_claims() {
    struct Embed;
    impl MemoryEmbedder for Embed {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, _: &str) -> verified_code_memory::Result<Vec<f32>> {
            Ok(vec![1.0, 0.0])
        }
    }
    let store = SharedMemoryStore::open_in_memory().unwrap();
    for _ in 0..8 {
        store.remember(claim()).unwrap();
    }
    let other = store
        .remember(NewMemory::procedure(
            "p",
            "Retry deadline",
            "Bound the retry deadline.",
        ))
        .unwrap();
    let index =
        verified_code_memory::semantic::SemanticIndex::new(store.clone(), "fixture", 2).unwrap();
    index.backfill("p", &Embed).unwrap();
    let query = RetrievalQuery::new("p", "retry lock queue").with_limit(2);
    let semantic = index.search(&query, &[1.0, 0.0], 0.8).unwrap();
    assert_eq!(semantic.len(), 2);
    assert!(semantic.iter().any(|(record, _)| record.id == other.id));
    let hybrid = index.retrieve(&query, &Embed, 0.8).unwrap();
    assert_eq!(hybrid.len(), 2);
    assert!(hybrid.iter().any(|hit| hit.record.id == other.id));
    assert!(hybrid
        .iter()
        .any(|hit| hit.reason.contains("semantic match")));
}
