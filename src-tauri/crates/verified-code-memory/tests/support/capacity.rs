//! Constructed capacity replays, shared by regression tests and the evaluator.
use serde::Deserialize;
use serde_json::{json, Value};
use verified_code_memory::{retrieval::estimate_tokens, MemoryStore, NewMemory, RetrievedMemory};

#[derive(Deserialize)]
struct Record {
    title: String,
    content: String,
    #[serde(default = "one")]
    repeat: usize,
}

fn one() -> usize {
    1
}

#[derive(Deserialize)]
struct Case {
    name: String,
    budget: usize,
    corpus: Vec<Record>,
    required: Vec<String>,
}

/// Replays the frozen capacity fixtures against the production packet renderer.
pub fn evaluate() -> Value {
    let cases: Vec<Case> = serde_json::from_str(include_str!("../fixtures/capacity_cases.json"))
        .expect("capacity fixture JSON");
    let rows: Vec<_> = cases
        .into_iter()
        .map(|case| {
            let store = MemoryStore::open_in_memory().unwrap();
            // Packet fixtures deliberately supply an ordered, already admitted pool.
            // They measure delivery independently of candidate generation/ranking.
            let hits: Vec<_> = case
                .corpus
                .into_iter()
                .map(|record| RetrievedMemory {
                    record: store
                        .remember(NewMemory::project_fact(
                            "project",
                            record.title,
                            record.content.repeat(record.repeat),
                        ))
                        .unwrap(),
                    score: 1.0,
                    reason: "capacity replay".into(),
                    components: Default::default(),
                })
                .collect();
            let (packet, shown) = store.render_packet_traced(&hits, case.budget);
            let delivered: Vec<_> = hits
                .iter()
                .filter(|hit| shown.contains(&hit.record.id) && packet.contains(&hit.record.content))
                .map(|hit| hit.record.title.clone())
                .collect();
            let recalled = case.required.iter().filter(|title| delivered.contains(title)).count();
            json!({"name":case.name,"budget":case.budget,"tokens":estimate_tokens(&packet),
                "required":case.required.len(),"delivered":recalled,"titles":delivered,"expected_titles":case.required,
                "traced_ids_visible":shown.iter().all(|id| packet.contains(&id.to_string()))})
        })
        .collect();
    let required: u64 = rows
        .iter()
        .map(|row| row["required"].as_u64().unwrap())
        .sum();
    let delivered: u64 = rows
        .iter()
        .map(|row| row["delivered"].as_u64().unwrap())
        .sum();
    json!({"description":"Constructed packet capacity mechanisms; not a blind recall or agent-quality evaluation",
        "required":required,"delivered":delivered,"delivered_recall":delivered as f64 / required as f64,"cases":rows})
}
