use uuid::Uuid;
use verified_code_memory::{
    semantic::SemanticIndex, MemoryEmbedder, MemoryStore, NewMemory, RetrievalQuery,
    RetrievedMemory, SharedMemoryStore, Verification,
};

struct Model;
impl MemoryEmbedder for Model {
    fn dimensions(&self) -> usize {
        2
    }
    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        Ok(if text.contains("lock") || text.contains("Archive") {
            vec![0.0, 1.0]
        } else if text.starts_with("crowding\n") {
            vec![0.95, 0.3122499]
        } else if text.starts_with("Crowding observation") {
            vec![0.9, 0.4358899]
        } else {
            vec![1.0, 0.0]
        })
    }
}
fn ids(hits: &[RetrievedMemory]) -> Vec<Uuid> {
    hits.iter().map(|h| h.record.id).collect()
}

struct Fixture {
    store: SharedMemoryStore,
    index: SemanticIndex,
    query: RetrievalQuery,
    compact: Vec<Uuid>,
    archives: Vec<Uuid>,
    denied: Uuid,
}
fn fixture(missing: bool) -> Fixture {
    let store = SharedMemoryStore::new(MemoryStore::open_in_memory().unwrap());
    let keep = |memory: NewMemory| {
        let record = store.remember(memory).unwrap();
        store.verify(record.id, "source evidence").unwrap();
        record.id
    };
    let mut compact = Vec::new();
    let mut archives = Vec::new();
    for (i, title) in [
        "Alpha memory store",
        "Bravo memory query",
        "Charlie memory packet",
        "Delta memory index",
    ]
    .into_iter()
    .enumerate()
    {
        if !missing || i != 3 {
            compact.push(keep(NewMemory::procedure(
                "p",
                title,
                "Use a lock to guard durable state. ".repeat(12),
            )));
        }
        for n in 0..if i == 0 { 90 } else { 1 } {
            archives.push(keep(NewMemory::procedure(
                "p",
                title.to_uppercase(),
                format!(
                    "Archive: {} Copy {n}.",
                    format!("{title} references; ").repeat(600)
                ),
            )));
        }
    }
    for n in 0..9 {
        keep(NewMemory::procedure(
            "p",
            format!("Crowding observation {n}"),
            format!("Independent crowding observation {n}."),
        ));
    }
    keep(NewMemory::procedure(
        "p",
        "crowding",
        "crowding ".repeat(20),
    ));
    // Keep two independent protected positions: a strict leading result and
    // the exact literal context. Equal semantic scores can otherwise omit the
    // literal or make it lead, changing the legacy optional-title capacity.
    keep(NewMemory::procedure(
        "p",
        "Crowding leading observation",
        "crowding ".repeat(120),
    ));
    let denied = keep(NewMemory::procedure(
        "p",
        "ALPHA MEMORY STORE",
        format!("{} lock", "Alpha memory store ".repeat(8)),
    ));
    store
        .remember(NewMemory::procedure(
            "p",
            "ALPHA MEMORY STORE",
            format!("{} lock", "Alpha memory store ".repeat(8)),
        ))
        .unwrap();
    keep(
        NewMemory::procedure(
            "p",
            "ALPHA MEMORY STORE",
            format!("{} lock tagged", "Alpha memory store ".repeat(8)),
        )
        .with_tags(["blocked"]),
    );
    keep(NewMemory::procedure(
        "foreign",
        "ALPHA MEMORY STORE",
        format!("{} lock foreign", "Alpha memory store ".repeat(8)),
    ));
    let index = SemanticIndex::new(store.clone(), "name-fit-fixture", 2).unwrap();
    index.backfill("p", &Model).unwrap();
    index.backfill("foreign", &Model).unwrap();
    Fixture {
        store,
        index,
        query: RetrievalQuery::new(
            "p",
            "AlphaMemoryStore,BravoMemoryQuery,CharlieMemoryPacket,DeltaMemoryIndex crowding",
        )
        .with_verifications([Verification::Verified])
        .without_tags(["blocked"]),
        compact,
        archives,
        denied,
    }
}
fn retrieve(f: &Fixture, budget: Option<usize>) -> Vec<RetrievedMemory> {
    let mut query = f.query.clone();
    query.packet_budget = budget;
    f.index
        .retrieve_filtered(&query, &Model, 0.8, |r| r.id != f.denied)
        .unwrap()
}

#[test]
fn fitting_new_name_additions_replace_only_oversized_planned_claims_and_render_in_full() {
    let f = fixture(false);
    let before = retrieve(&f, None);
    assert_eq!(before[0].record.title, "Crowding leading observation");
    assert!(before
        .iter()
        .skip(1)
        .any(|hit| hit.record.title == "crowding"));
    assert_eq!(before.len(), 8);
    assert_eq!(
        before
            .iter()
            .filter(|h| f.archives.contains(&h.record.id))
            .count(),
        4
    );
    assert!(!f.compact.iter().any(|id| ids(&before).contains(id)));
    let after = retrieve(&f, Some(2000));
    let packet = f.store.render_packet_with_evidence(&after, 2000);
    assert!(f.compact.iter().all(|id| packet.complete_ids.contains(id)));
    for (old, new) in before.iter().zip(&after) {
        if !f.archives.contains(&old.record.id) {
            assert_eq!(old.record.id, new.record.id);
        }
        assert_eq!(old.score, new.score);
    }
    assert!(after.iter().all(|h| h.record.repo == "p"
        && h.record.verification == Verification::Verified
        && !h.record.tags.iter().any(|t| t == "blocked")
        && h.record.id != f.denied));
    let budget = (100..2000)
        .find(|n| {
            let packet = f.store.render_packet_with_evidence(&after, *n);
            f.compact.iter().any(|id| packet.complete_ids.contains(id))
                && !f.compact.iter().all(|id| packet.complete_ids.contains(id))
        })
        .expect("fixture needs a partial-render boundary");
    assert_eq!(ids(&retrieve(&f, Some(budget))), ids(&before));
}

#[test]
fn a_missing_fitting_name_keeps_the_exact_legacy_plan_instead_of_partial_replacement() {
    let f = fixture(true);
    let before = retrieve(&f, None);
    let after = retrieve(&f, Some(2000));
    assert_eq!(ids(&before), ids(&after));
}
