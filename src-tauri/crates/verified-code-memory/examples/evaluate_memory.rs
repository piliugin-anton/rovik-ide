//! Reproducible retrieval baseline over incident-separated synthetic cases.
use serde_json::{json, Value};
use verified_code_memory::{NewMemory, RetrievalQuery, SharedMemoryStore};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = include_str!("../../../../scripts/eval/memory-cases.json");
    let dataset: Value = serde_json::from_str(input)?;
    let backend = match std::env::var("ROVIK_MEMORY_EVAL_MODEL") {
        Ok(model) => {
            let python = std::env::var("ROVIK_MEMORY_EVAL_PYTHON")?;
            let dimensions: usize = std::env::var("ROVIK_MEMORY_EVAL_DIMENSIONS")?.parse()?;
            let revision = std::env::var("ROVIK_MEMORY_EVAL_REVISION")?;
            let threshold: f32 = std::env::var("ROVIK_MEMORY_EVAL_THRESHOLD")?.parse()?;
            Some((
                verified_code_memory::embedding::LocalSentenceEmbedder::new(
                    python.into(),
                    model.into(),
                    dimensions,
                )?,
                revision,
                dimensions,
                threshold,
            ))
        }
        Err(_) => None,
    };
    let mut rows = Vec::new();
    for case in dataset["cases"].as_array().ok_or("missing cases")? {
        let text = |key: &str| case[key].as_str().ok_or("missing text");
        let title = text("title")?;
        let store = SharedMemoryStore::open_in_memory()?;
        let file = format!("src/{title}.rs");
        let record = store.remember(
            NewMemory::project_fact("project", title, text("content")?).with_files([file]),
        )?;
        store.verify(record.id, "synthetic fixture evidence")?;
        let expected = if case["supersede"] == true {
            store.deprecate(record.id, "explicit fixture update")?;
            let replacement =
                store.remember(NewMemory::project_fact("project", title, text("content")?))?;
            store.verify(replacement.id, "replacement evidence")?;
            replacement.id
        } else {
            record.id
        };
        // Distractors from independent incidents share the same repository.
        for other in dataset["cases"]
            .as_array()
            .ok_or("missing cases")?
            .iter()
            .filter(|other| other["category"] == "identifier" && other["title"] != case["title"])
        {
            store.remember(NewMemory::project_fact(
                "project",
                other["title"].as_str().ok_or("missing title")?,
                other["content"].as_str().ok_or("missing content")?,
            ))?;
        }
        // An identical record in another project must never leak.
        store.remember(NewMemory::project_fact("other", title, text("content")?))?;
        let mut query = RetrievalQuery::new("project", text("task")?);
        if let Some(file) = case["file"].as_str() {
            query = query.with_files([file]);
        }
        let started = std::time::Instant::now();
        let (baseline, telemetry) = store.retrieve_with_telemetry(&query)?;
        let found = if let Some((embedder, revision, dimensions, threshold)) = &backend {
            let index = verified_code_memory::semantic::SemanticIndex::new(
                store.clone(),
                revision,
                *dimensions,
            )?;
            index.backfill("project", embedder)?;
            index.retrieve(&query, embedder, *threshold)?
        } else {
            baseline
        };
        let rank = found
            .iter()
            .position(|hit| hit.record.id == expected)
            .map(|at| at + 1);
        let prohibited = found.iter().any(|hit| hit.record.repo != "project");
        rows.push(json!({"id":case["id"],"incident":case["incident"],"split":case["split"],"category":case["category"],"answerable":case["answerable"],"rank":rank,"returned":found.len(),"prohibited":prohibited,"latency_us":started.elapsed().as_micros(),"telemetry":telemetry}));
    }
    let mut groups = serde_json::Map::new();
    for split in ["development", "heldout"] {
        let subset: Vec<_> = rows.iter().filter(|r| r["split"] == split).collect();
        let answerable: Vec<_> = subset.iter().filter(|r| r["answerable"] == true).collect();
        let negatives: Vec<_> = subset.iter().filter(|r| r["answerable"] == false).collect();
        let recall = answerable.iter().filter(|r| r["rank"].is_number()).count() as f64
            / answerable.len() as f64;
        let mrr = answerable
            .iter()
            .filter_map(|r| r["rank"].as_u64())
            .map(|rank| 1.0 / rank as f64)
            .sum::<f64>()
            / answerable.len() as f64;
        let ndcg = answerable
            .iter()
            .filter_map(|r| r["rank"].as_u64())
            .map(|rank| 1.0 / (rank as f64 + 1.0).log2())
            .sum::<f64>()
            / answerable.len() as f64;
        groups.insert(split.into(), json!({"cases":subset.len(),"recall_at_8":recall,"mrr":mrr,"ndcg_at_8":ndcg,"no_match_injection_rate":negatives.iter().filter(|r|r["returned"]!=0).count() as f64 / negatives.len() as f64}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"dataset_version":dataset["version"],"dataset_fingerprint":verified_code_memory::claim_hash("memory-eval-v1",input),"policy":if backend.is_some() {"hybrid"} else {"lexical-file"},"memory_off_recall":0,"model":backend.as_ref().map(|(_,revision,dimensions,_)| json!({"revision":revision,"dimensions":dimensions})),"answer_quality_evaluated":false,"groups":groups,"cases":rows})
        )?
    );
    Ok(())
}
