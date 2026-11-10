use uuid::Uuid;
use verified_code_memory::{
    semantic::SemanticIndex, MemoryEmbedder, MemoryStore, NewMemory, RetrievalQuery,
    RetrievalWeights, RetrievedMemory, SharedMemoryStore,
};

struct SubjectModel;
impl MemoryEmbedder for SubjectModel {
    fn dimensions(&self) -> usize {
        2
    }
    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        Ok(
            if text.contains("lock") || text.starts_with("File context") {
                vec![0.0, 1.0]
            } else if text.starts_with("crowding") || text.starts_with("SHARED MEMORY STORE") {
                vec![0.95, 0.3122499]
            } else if text.starts_with("Cross references") {
                vec![0.9, 0.4358899]
            } else if text.starts_with("Crowding") {
                vec![0.8, 0.6]
            } else {
                vec![1.0, 0.0]
            },
        )
    }
}

fn ids(hits: &[RetrievedMemory]) -> Vec<Uuid> {
    hits.iter().map(|hit| hit.record.id).collect()
}

struct Fixture {
    store: SharedMemoryStore,
    index: SemanticIndex,
    query: RetrievalQuery,
    leader: Uuid,
    compact: Vec<Uuid>,
    context: Vec<Uuid>,
}

#[derive(Clone, Copy)]
enum Anchor {
    None,
    File,
    Name,
    Literal,
    Canonical,
}

fn fixture(anchor: Anchor) -> Fixture {
    let store = SharedMemoryStore::new(MemoryStore::open_in_memory().unwrap().with_weights(
        RetrievalWeights {
            file_match: 10.0,
            ..Default::default()
        },
    ));
    let keep = |memory: NewMemory| {
        let row = store.remember(memory).unwrap();
        store.verify(row.id, "source evidence").unwrap();
        row.id
    };
    let compact = ["Shared memory store", "Retrieval query", "Memory packet"]
        .into_iter()
        .map(|title| {
            keep(NewMemory::procedure(
                "a",
                title,
                "Use a lock to protect durable state. ".repeat(5),
            ))
        })
        .collect();
    let reference = keep(NewMemory::procedure(
        "a",
        "Cross references",
        format!(
            "Shared memory store, retrieval query and memory packet navigation. {}",
            "crowding ".repeat(20)
        ),
    ));
    let literal = keep(NewMemory::procedure(
        "a",
        "crowding",
        "crowding ".repeat(20),
    ));
    let mut context = vec![reference, literal];
    for path in ["src/a.rs", "src/b.rs", "src/c.rs"] {
        context.push(keep(
            NewMemory::procedure("a", "File context", format!("Constraint for {path}."))
                .with_files([path]),
        ));
    }
    for n in 0..4 {
        keep(NewMemory::procedure(
            "a",
            format!("Crowding observation {n}"),
            format!("Independent crowding metric {n}."),
        ));
    }
    let leader = keep(
        NewMemory::procedure(
            "a",
            if matches!(anchor, Anchor::Literal) {
                "CROWDING"
            } else if matches!(anchor, Anchor::Canonical) {
                "SharedMemoryStore"
            } else {
                "Leading observation"
            },
            format!(
                "{}{}",
                if matches!(anchor, Anchor::Name) {
                    "Shared memory store. "
                } else {
                    ""
                },
                "crowding ".repeat(120)
            ),
        )
        .with_files(if matches!(anchor, Anchor::File) {
            vec!["src/a.rs"]
        } else {
            Vec::new()
        }),
    );
    if matches!(anchor, Anchor::Canonical) {
        keep(NewMemory::procedure(
            "a",
            "SHARED MEMORY STORE",
            format!("{}A different assertion.", "crowding ".repeat(30)),
        ));
    }
    let index = SemanticIndex::new(store.clone(), "full-subject-fixture", 2).unwrap();
    index.backfill("a", &SubjectModel).unwrap();
    Fixture {
        store,
        index,
        query: RetrievalQuery::new(
            "a",
            if matches!(anchor, Anchor::Canonical) {
                "crate::SharedMemoryStore,RetrievalQuery,MemoryPacket,CrossReferences crowding"
            } else {
                "SharedMemoryStore,RetrievalQuery,MemoryPacket,CrossReferences crowding"
            },
        )
        .with_files(["src/a.rs", "src/b.rs", "src/c.rs"]),
        leader,
        compact,
        context,
    }
}

#[test]
fn eight_explicit_subjects_can_replace_only_an_unanchored_leader_with_full_delivery() {
    let f = fixture(Anchor::None);
    let before = f.index.retrieve(&f.query, &SubjectModel, 0.7).unwrap();
    assert_eq!(before.len(), 8);
    assert_eq!(before[0].record.id, f.leader);
    assert!(f.context.iter().all(|id| ids(&before).contains(id)));
    assert!(!f.compact.iter().any(|id| ids(&before).contains(id)));
    let after = f
        .index
        .retrieve(
            &f.query.clone().with_packet_budget(2000),
            &SubjectModel,
            0.7,
        )
        .unwrap();
    let packet = f.store.render_packet_with_evidence(&after, 2000);
    let wanted: Vec<_> = f.compact.iter().chain(&f.context).copied().collect();
    assert_eq!(after.len(), 8);
    assert_eq!(packet.complete_ids.len(), 8);
    assert!(wanted.iter().all(|id| packet.complete_ids.contains(id)));
    assert!(!ids(&after).contains(&f.leader));
    for id in &f.context {
        assert_eq!(
            ids(&after).iter().position(|found| found == id),
            ids(&before).iter().position(|found| found == id)
        );
    }
    let pressure = (100..2000)
        .find(|budget| {
            let old = f.store.render_packet_with_evidence(&before, *budget);
            let trial = f.store.render_packet_with_evidence(&after, *budget);
            f.compact.iter().all(|id| trial.complete_ids.contains(id))
                && f.context
                    .iter()
                    .all(|id| !old.complete_ids.contains(id) || trial.complete_ids.contains(id))
                && !wanted.iter().all(|id| trial.complete_ids.contains(id))
        })
        .expect("fixture must fit titles while omitting another requested subject");
    let rejected = f
        .index
        .retrieve(&f.query.with_packet_budget(pressure), &SubjectModel, 0.7)
        .unwrap();
    assert_eq!(ids(&rejected), ids(&before));
}

#[test]
fn independent_name_file_and_literal_anchors_keep_the_leader_under_slot_pressure() {
    for anchor in [
        Anchor::File,
        Anchor::Name,
        Anchor::Literal,
        Anchor::Canonical,
    ] {
        let f = fixture(anchor);
        let before = f.index.retrieve(&f.query, &SubjectModel, 0.7).unwrap();
        assert_eq!(before[0].record.id, f.leader);
        let after = f
            .index
            .retrieve(&f.query.with_packet_budget(2000), &SubjectModel, 0.7)
            .unwrap();
        assert_eq!(after[0].record.id, f.leader);
    }
}

fn assert_aliases_cannot_release_a_leader(f: &Fixture, query: RetrievalQuery, wanted: &[Uuid]) {
    assert_eq!(wanted.len(), 7);
    let before = f.index.retrieve(&query, &SubjectModel, 0.7).unwrap();
    assert_eq!(before[0].record.id, f.leader);
    let normal = f
        .index
        .retrieve(&query.clone().with_packet_budget(2000), &SubjectModel, 0.7)
        .unwrap();
    assert_eq!(normal[0].record.id, f.leader);
    let added: Vec<_> = wanted
        .iter()
        .filter(|id| !ids(&before).contains(id))
        .copied()
        .collect();
    let slots = std::iter::once(0).chain(
        (1..before.len())
            .rev()
            .filter(|slot| !wanted.contains(&before[*slot].record.id)),
    );
    let mut alternative = before.clone();
    for (slot, id) in slots.zip(&added) {
        alternative[slot] = normal
            .iter()
            .find(|hit| hit.record.id == *id)
            .unwrap()
            .clone();
    }
    let pressure = (100..2000)
        .find(|budget| {
            let old = f.store.render_packet_with_evidence(&before, *budget);
            let normal = f.store.render_packet_with_evidence(&normal, *budget);
            let alternative = f.store.render_packet_with_evidence(&alternative, *budget);
            wanted
                .iter()
                .all(|id| alternative.complete_ids.contains(id))
                && (!added.iter().all(|id| normal.complete_ids.contains(id))
                    || before.iter().any(|hit| {
                        (hit.record.id == f.leader || wanted.contains(&hit.record.id))
                            && old.complete_ids.contains(&hit.record.id)
                            && !normal.complete_ids.contains(&hit.record.id)
                    }))
        })
        .expect("a hypothetical leader replacement must help the seven-subject packet");
    let actual = f
        .index
        .retrieve(&query.with_packet_budget(pressure), &SubjectModel, 0.7)
        .unwrap();
    assert_eq!(ids(&actual), ids(&before));
}

#[test]
fn duplicate_identifier_and_file_aliases_cannot_inflate_seven_subjects_into_eight() {
    let f = fixture(Anchor::None);
    let wanted: Vec<_> = f.compact[..2].iter().chain(&f.context).copied().collect();
    assert_aliases_cannot_release_a_leader(
        &f,
        RetrievalQuery::new(
            "a",
            "SharedMemoryStore,crate::SharedMemoryStore,RetrievalQuery,CrossReferences crowding",
        )
        .with_files(["src/a.rs", "src/b.rs", "src/c.rs"]),
        &wanted,
    );
    let wanted: Vec<_> = f.compact.iter().chain(&f.context[..4]).copied().collect();
    assert_aliases_cannot_release_a_leader(
        &f,
        RetrievalQuery::new("a", &f.query.task).with_files(["src/a.rs", "src/b.rs", "./src\\a.rs"]),
        &wanted,
    );
}
