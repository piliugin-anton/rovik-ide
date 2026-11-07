//! Mechanism experiments for capture, linked context and duplicate crowding.
//! These constructed fixtures do not measure an LLM's extraction or task quality.
use serde_json::{json, Value};
use std::collections::HashSet;
use verified_code_memory::{NewMemory, RetrievalQuery, RetrievedMemory, SharedMemoryStore};

fn collapse(hits: Vec<RetrievedMemory>, limit: usize) -> Vec<RetrievedMemory> {
    let mut families = HashSet::new();
    hits.into_iter()
        .filter(|hit| families.insert(hit.record.source_episode.unwrap_or(hit.record.id)))
        .take(limit)
        .collect()
}

fn representation(
    mode: &str,
    cases: &[Value],
    keys: &Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let corpus: Vec<_> = cases
        .iter()
        .filter(|case| case["category"] == "identifier")
        .collect();
    let mut captured = 0;
    let mut retrieved = 0;
    let mut delivered = 0;
    let mut total = 0;
    let mut budget_overruns = 0;
    let mut rows = Vec::new();
    for case in cases.iter().filter(|case| case["answerable"] == true) {
        let store = SharedMemoryStore::open_in_memory()?;
        for source in &corpus {
            let title = source["title"].as_str().ok_or("missing title")?;
            let claim = source["content"].as_str().ok_or("missing content")?;
            let context = keys["entries"][title]["terms"]
                .as_array()
                .ok_or("missing descriptors")?
                .iter()
                .map(|term| term.as_str().ok_or("invalid descriptor"))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            let activity = format!("Completed work on {title}; the command exited successfully.");
            let original = if mode == "linked_context" {
                format!("{claim}\nSource context: {context}")
            } else {
                activity
            };
            let episode = store.remember(NewMemory::episode(
                "p",
                format!("Work on {title}"),
                original,
            ))?;
            if mode != "episode_only" {
                let claim_text = if mode == "flattened_context" {
                    format!("{claim}\nSource context: {context}")
                } else {
                    claim.to_owned()
                };
                store.remember(
                    NewMemory::procedure("p", title, claim_text)
                        .with_source_episode(episode.id)
                        .with_files([format!("src/{title}.rs")]),
                )?;
            }
        }
        let claim = case["content"].as_str().ok_or("missing claim")?;
        let present = store
            .list("p", "")?
            .iter()
            .any(|record| record.content.contains(claim));
        captured += usize::from(present);
        let mut query =
            RetrievalQuery::new("p", case["task"].as_str().ok_or("missing task")?).with_limit(32);
        if let Some(path) = case["file"].as_str() {
            query = query.with_files([path]);
        }
        let found = collapse(store.retrieve(&query)?, 8);
        let recalled = found.iter().any(|hit| hit.record.content.contains(claim));
        retrieved += usize::from(recalled);
        let (packet, _) = store.render_packet_traced(&found, 1000);
        let supplied = packet.contains(claim);
        delivered += usize::from(supplied);
        budget_overruns +=
            usize::from(verified_code_memory::retrieval::estimate_tokens(&packet) > 1000);
        total += 1;
        rows.push(json!({"id":case["id"],"split":case["split"],"captured":present,"retrieved":recalled,"delivered":supplied}));
    }
    Ok(
        json!({"mode":mode,"cases":total,"capture_coverage":captured as f64/total as f64,"answer_bearing_recall_at_8":retrieved as f64/total as f64,"delivered_recall":delivered as f64/total as f64,"budget_overruns":budget_overruns,"negative_controls_evaluated":false,"rows":rows}),
    )
}

fn duplicate_stress() -> Result<Value, Box<dyn std::error::Error>> {
    let store = SharedMemoryStore::open_in_memory()?;
    let episode = store.remember(NewMemory::episode(
        "p",
        "SQLite investigation",
        "Recorded source observations.",
    ))?;
    let mut candidates = Vec::new();
    for _ in 0..12 {
        let record = store.remember(
            NewMemory::procedure(
                "p",
                "Release SQLite mutex",
                "Release the SQLite mutex before model execution.",
            )
            .with_source_episode(episode.id),
        )?;
        candidates.push(RetrievedMemory {
            record,
            score: 1.0,
            reason: "admitted candidate; constructed ranking stress".into(),
            components: Default::default(),
        });
    }
    let independent = store.remember(NewMemory::procedure(
        "p",
        "Restore context references",
        "Restore active memory IDs after history compaction.",
    ))?;
    candidates.push(RetrievedMemory {
        record: independent,
        score: 0.9,
        reason: "admitted candidate; constructed ranking stress".into(),
        components: Default::default(),
    });
    let evaluate = |hits: Vec<RetrievedMemory>| {
        let (packet, ids) = store.render_packet_traced(&hits, 1000);
        let coverage =
            usize::from(packet.contains("Release the SQLite mutex before model execution."))
                + usize::from(
                    packet.contains("Restore active memory IDs after history compaction."),
                );
        json!({"covered_requirements":coverage,"required":2,"records_supplied":ids.len(),"packet_tokens":verified_code_memory::retrieval::estimate_tokens(&packet)})
    };
    let flat = evaluate(candidates.iter().take(8).cloned().collect());
    let diverse = evaluate(collapse(candidates, 8));
    Ok(
        json!({"constructed_stress":true,"candidate_count":13,"flat":flat,"incident_collapse":diverse,"generator_quality_evaluated":false}),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dataset: Value =
        serde_json::from_str(include_str!("../../../../scripts/eval/memory-cases.json"))?;
    let keys: Value = serde_json::from_str(include_str!(
        "../../../../scripts/eval/memory-context-keys.json"
    ))?;
    let cases = dataset["cases"].as_array().ok_or("missing cases")?;
    let representations = [
        "episode_only",
        "structured_claim",
        "flattened_context",
        "linked_context",
    ]
    .iter()
    .map(|mode| representation(mode, cases, &keys))
    .collect::<Result<Vec<_>, _>>()?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"representations":representations,"duplicate_stress":duplicate_stress()?,"final_limit":8,"memory_budget":1000,"extractor_accuracy_measured":false,"agent_task_success_measured":false})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_incident_cannot_fill_the_packet_and_hide_an_independent_requirement() {
        let report = duplicate_stress().unwrap();
        assert_eq!(report["flat"]["covered_requirements"], 1);
        assert_eq!(report["incident_collapse"]["covered_requirements"], 2);
        assert_eq!(report["incident_collapse"]["records_supplied"], 2);
        assert!(
            report["incident_collapse"]["packet_tokens"]
                .as_u64()
                .unwrap()
                <= 1000
        );
    }

    #[test]
    fn successful_activity_does_not_manufacture_the_missing_answer() {
        let cases = vec![
            json!({"id":"c","category":"identifier","title":"lock","content":"Release the mutex before the model call.","task":"mutex model","answerable":true,"split":"development"}),
        ];
        let keys = json!({"entries":{"lock":{"terms":["mutex ownership"]}}});
        assert_eq!(
            representation("episode_only", &cases, &keys).unwrap()["capture_coverage"],
            0.0
        );
        assert_eq!(
            representation("structured_claim", &cases, &keys).unwrap()["capture_coverage"],
            1.0
        );
    }
}
