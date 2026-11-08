//! Synthetic candidate-pool pressure, independent of semantic similarity.
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use verified_code_memory::{MemoryStore, NewMemory, RetrievalQuery, Verification};

#[derive(Deserialize)]
struct Record {
    title: String,
    content: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    verification: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default = "one")]
    copies: usize,
}

fn one() -> usize {
    1
}

#[derive(Deserialize)]
struct Case {
    name: String,
    task: String,
    files: Vec<String>,
    #[serde(default)]
    excluded_tags: Vec<String>,
    corpus: Vec<Record>,
    required: Vec<String>,
}

/// Measures recall and delivery under duplicate and visibility pressure.
pub fn evaluate() -> Value {
    let cases: Vec<Case> = serde_json::from_str(include_str!("../fixtures/pool_cases.json"))
        .expect("candidate pool fixture JSON");
    let rows: Vec<_> = cases.into_iter().map(|case| {
        let store = MemoryStore::open_in_memory().unwrap();
        let mut hidden = HashSet::new();
        let mut seeded = Vec::new();
        for spec in &case.corpus {
            for _ in 0..spec.copies {
                let record = store.remember(NewMemory::project_fact(
                    spec.repo.as_deref().unwrap_or("project"), &spec.title, &spec.content,
                ).with_files(spec.files.iter().cloned()).with_tags(spec.tags.iter().cloned())).unwrap();
                if spec.verification.as_deref() == Some("deprecated") {
                    store.deprecate(record.id, "fixture supersession").unwrap();
                }
                if spec.hidden { hidden.insert(record.id); }
                seeded.push(store.get(record.id).unwrap());
            }
        }
        let query = RetrievalQuery::new("project", &case.task)
            .with_files(case.files).without_tags(case.excluded_tags.clone()).with_limit(8);
        let started = std::time::Instant::now();
        let (found, telemetry) = store.retrieve_filtered_with_telemetry(&query, |r| !hidden.contains(&r.id)).unwrap();
        let elapsed_us = started.elapsed().as_micros();
        let again = store.retrieve_filtered_with_telemetry(&query, |r| !hidden.contains(&r.id)).unwrap().0;
        let (packet, shown) = store.render_packet_traced(&found, 1000);
        let titles: Vec<_> = found.iter().map(|h| h.record.title.clone()).collect();
        let recalled = case.required.iter().filter(|t| titles.contains(t)).count();
        let delivered = case.required.iter().filter(|title| found.iter().any(|h|
            &h.record.title == *title && shown.contains(&h.record.id) && packet.contains(&h.record.content))).count();
        let safe = found.iter().all(|h| h.record.repo == "project" && !hidden.contains(&h.record.id)
            && h.record.verification != Verification::Deprecated
            && h.record.tags.iter().all(|tag| !case.excluded_tags.contains(tag)));
        json!({"name":case.name,"required":case.required.len(),"recalled":recalled,"delivered":delivered,
            "titles":titles,"returned":found.len(),"safe":safe,"elapsed_us":elapsed_us,"telemetry":telemetry,
            "stable":found.iter().map(|h|h.record.id).eq(again.iter().map(|h|h.record.id)),
            "history_unchanged":seeded.iter().all(|r| store.get(r.id).unwrap() == *r),
            "packet_tokens":verified_code_memory::retrieval::estimate_tokens(&packet)})
    }).collect();
    let total = |field: &str| rows.iter().map(|r| r[field].as_u64().unwrap()).sum::<u64>();
    json!({"description":"Constructed candidate-capacity cases, not semantic generalization or agent task success",
        "required":total("required"),"recalled":total("recalled"),"delivered":total("delivered"),"cases":rows})
}
