mod support {
    pub mod capacity;
}

#[test]
fn packet_capacity_replays_deliver_later_claims_without_exceeding_the_allowance() {
    let report = support::capacity::evaluate();
    for case in report["cases"].as_array().unwrap() {
        assert_eq!(case["delivered"], case["required"], "{case}");
        assert_eq!(case["titles"], case["expected_titles"], "{case}");
        assert!(
            case["tokens"].as_u64().unwrap() <= case["budget"].as_u64().unwrap(),
            "{case}"
        );
        assert_eq!(case["traced_ids_visible"], true, "{case}");
    }
}

#[test]
fn every_allowance_bounds_unicode_claims_and_fetch_hints() {
    use verified_code_memory::{
        retrieval::estimate_tokens, MemoryStore, NewMemory, RetrievedMemory,
    };
    let store = MemoryStore::open_in_memory().unwrap();
    let hits: Vec<_> = ["観測を再検証。".repeat(400), "Bound retries.".into()]
        .into_iter()
        .enumerate()
        .map(|(offset, content)| RetrievedMemory {
            record: store
                .remember(NewMemory::project_fact(
                    "project",
                    format!("Claim {offset}"),
                    content,
                ))
                .unwrap(),
            score: 1.0,
            reason: "fixture".into(),
            components: Default::default(),
        })
        .collect();
    let full = store.render_context(&hits);
    assert!(full.contains(&hits[0].record.content));
    assert!(full.contains(&hits[1].record.content));
    for budget in 0..300 {
        let (packet, shown) = store.render_packet_traced(&hits, budget);
        assert!(estimate_tokens(&packet) <= budget, "{budget}: {packet}");
        assert!(shown.iter().all(|id| packet.contains(&id.to_string())));
    }
    let (packet, shown) = store.render_packet_traced(&hits, 180);
    assert!(packet.contains(&format!("`memory_get` {}", hits[0].record.id)));
    assert!(shown.contains(&hits[0].record.id));
    assert!(packet.contains(&hits[1].record.content));
}
