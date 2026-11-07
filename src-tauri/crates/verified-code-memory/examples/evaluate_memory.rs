//! Reproducible retrieval baseline over incident-separated synthetic cases.
use serde_json::{json, Value};
use verified_code_memory::{MemoryEmbedder, NewMemory, RetrievalQuery, SharedMemoryStore};

/// An experiment can replay real, externally computed embeddings through the
/// production SQLite index. Missing inputs fail instead of inventing vectors.
struct RecordedEmbeddings {
    dimensions: usize,
    vectors: std::collections::BTreeMap<String, Vec<f32>>,
}

impl MemoryEmbedder for RecordedEmbeddings {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        let vector = self.vectors.get(text).cloned().ok_or(
            verified_code_memory::VcmError::EmbeddingUnavailable(
                "input absent from recorded model output",
            ),
        )?;
        if vector.iter().all(|value| *value == 0.0) {
            return Err(verified_code_memory::VcmError::EmbeddingUnavailable(
                "no vocabulary vector",
            ));
        }
        Ok(vector)
    }
}

fn experiment_config(args: &[String]) -> Result<Value, Box<dyn std::error::Error>> {
    match args {
        [] => Ok(json!({})),
        [flag, path] if flag == "--experiment-config" => {
            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
        }
        _ => Err("usage: evaluate_memory [--experiment-config config.json]".into()),
    }
}

/// Fixes fixture IDs and claim times before indexing: randomly generated UUIDs
/// otherwise change reciprocal-rank ties between identical experiment replays.
fn stabilize_fixture(
    uri: &str,
    expected: uuid::Uuid,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let mut connection = rusqlite::Connection::open(uri)?;
    connection.pragma_update(None, "foreign_keys", false)?;
    let transaction = connection.transaction()?;
    let ids = {
        let mut statement = transaction
            .prepare("SELECT id FROM memories ORDER BY repo,title,verification,content")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut new_expected = None;
    for (offset, old) in ids.iter().enumerate() {
        let id = uuid::Uuid::from_u128(offset as u128 + 1);
        if old == &expected.to_string() {
            new_expected = Some(id);
        }
        transaction.execute(
            "UPDATE memory_files SET memory_id=?1 WHERE memory_id=?2",
            rusqlite::params![id.to_string(), old],
        )?;
        transaction.execute("UPDATE memories SET id=?1,created_at=?3,updated_at=?3,content_updated_at=?3 WHERE id=?2", rusqlite::params![id.to_string(),old,"2026-11-07T00:00:00+00:00"])?;
    }
    transaction.commit()?;
    new_expected.ok_or_else(|| "expected fixture record disappeared".into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = include_str!("../../../../scripts/eval/memory-cases.json");
    let dataset: Value = serde_json::from_str(input)?;
    let config = experiment_config(&std::env::args().skip(1).collect::<Vec<_>>())?;
    let split = config.get("split").and_then(Value::as_str);
    if split.is_some_and(|split| !matches!(split, "development" | "heldout")) {
        return Err("split must be development or heldout".into());
    }
    type Backend = (Box<dyn MemoryEmbedder>, String, usize, f32);
    let backend: Option<Backend> = if let Some(path) = config["vectors_file"].as_str() {
        let recorded: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let dimensions = recorded["dimensions"]
            .as_u64()
            .ok_or("missing dimensions")? as usize;
        Some((
            Box::new(RecordedEmbeddings {
                dimensions,
                vectors: serde_json::from_value(recorded["vectors"].clone())?,
            }),
            recorded["revision"]
                .as_str()
                .ok_or("missing revision")?
                .to_owned(),
            dimensions,
            config["threshold"].as_f64().ok_or("missing threshold")? as f32,
        ))
    } else {
        match std::env::var("ROVIK_MEMORY_EVAL_MODEL") {
            Ok(model) => {
                let python = std::env::var("ROVIK_MEMORY_EVAL_PYTHON")?;
                let dimensions: usize = std::env::var("ROVIK_MEMORY_EVAL_DIMENSIONS")?.parse()?;
                let revision = std::env::var("ROVIK_MEMORY_EVAL_REVISION")?;
                let threshold: f32 = std::env::var("ROVIK_MEMORY_EVAL_THRESHOLD")?.parse()?;
                Some((
                    Box::new(verified_code_memory::embedding::LocalSentenceEmbedder::new(
                        python.into(),
                        model.into(),
                        dimensions,
                    )?),
                    revision,
                    dimensions,
                    threshold,
                ))
            }
            Err(_) => None,
        }
    };
    let mut cases = dataset["cases"].as_array().ok_or("missing cases")?.clone();
    if let Some(path) = config["controls_file"].as_str() {
        let controls: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        for control in controls["cases"].as_array().ok_or("missing controls")? {
            let mut case = cases[0].clone();
            for key in ["id", "incident", "split", "task"] {
                case[key] = control.get(key).ok_or("missing control field")?.clone();
            }
            case["category"] = json!("hard-negative");
            case["answerable"] = json!(false);
            cases.push(case);
        }
    }
    let mut rows = Vec::new();
    for case in &cases {
        if split.is_some_and(|split| case["split"] != split) {
            continue;
        }
        let text = |key: &str| case[key].as_str().ok_or("missing text");
        let title = text("title")?;
        let fixture_uri = "file:memory-evaluation?mode=memory&cache=shared";
        let deterministic = config["deterministic"].as_bool().unwrap_or(false);
        let store = if deterministic {
            SharedMemoryStore::open(fixture_uri)?
        } else {
            SharedMemoryStore::open_in_memory()?
        };
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
        let expected = if deterministic {
            stabilize_fixture(fixture_uri, expected)?
        } else {
            expected
        };
        let mut query = RetrievalQuery::new("project", text("task")?);
        if let Some(file) = case["file"].as_str() {
            query = query.with_files([file]);
        }
        let started = std::time::Instant::now();
        let (baseline, telemetry) = store.retrieve_with_telemetry(&query)?;
        let mut embedding_fallback = false;
        let found = if let Some((embedder, revision, dimensions, threshold)) = &backend {
            let index = verified_code_memory::semantic::SemanticIndex::new(
                store.clone(),
                revision,
                *dimensions,
            )?;
            index.backfill("project", embedder.as_ref())?;
            // Give the identical prohibited record a vector too: a semantic
            // scope check must not pass just because other projects lack an index.
            index.backfill("other", embedder.as_ref())?;
            match index.retrieve(&query, embedder.as_ref(), *threshold) {
                Ok(found) => found,
                Err(verified_code_memory::VcmError::EmbeddingUnavailable(
                    "no vocabulary vector",
                )) => {
                    embedding_fallback = true;
                    baseline
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            baseline
        };
        let rank = found
            .iter()
            .position(|hit| hit.record.id == expected)
            .map(|at| at + 1);
        let prohibited = found.iter().any(|hit| hit.record.repo != "project");
        rows.push(json!({"id":case["id"],"incident":case["incident"],"split":case["split"],"category":case["category"],"answerable":case["answerable"],"rank":rank,"returned":found.len(),"prohibited":prohibited,"embedding_fallback":embedding_fallback,"latency_us":started.elapsed().as_micros(),"telemetry":telemetry}));
    }
    let mut groups = serde_json::Map::new();
    for split in ["development", "heldout"] {
        let subset: Vec<_> = rows.iter().filter(|r| r["split"] == split).collect();
        if subset.is_empty() {
            continue;
        }
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
            &json!({"dataset_version":dataset["version"],"dataset_fingerprint":verified_code_memory::claim_hash("memory-eval-v1",input),"experiment_config":config,"policy":if backend.is_some() {"hybrid"} else {"lexical-file"},"memory_off_recall":0,"model":backend.as_ref().map(|(_,revision,dimensions,threshold)| json!({"revision":revision,"dimensions":dimensions,"threshold":threshold})),"answer_quality_evaluated":false,"groups":groups,"cases":rows})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_model_never_fabricates_an_unrecorded_embedding() {
        let model = RecordedEmbeddings {
            dimensions: 2,
            vectors: [("known".into(), vec![1.0, 0.0])].into(),
        };
        assert_eq!(model.embed("known").unwrap(), vec![1.0, 0.0]);
        assert!(model.embed("unknown").is_err());
    }

    #[test]
    fn an_invalid_configuration_invocation_is_rejected() {
        assert!(experiment_config(&["--wrong".into()]).is_err());
        assert_eq!(experiment_config(&[]).unwrap(), json!({}));
    }

    #[test]
    fn stable_fixture_ids_keep_the_file_index_connected() {
        let uri = "file:stable-fixture-test?mode=memory&cache=shared";
        let store = SharedMemoryStore::open(uri).unwrap();
        let record = store
            .remember(
                NewMemory::project_fact("p", "lock", "serialize access").with_files(["src/a.rs"]),
            )
            .unwrap();
        let id = stabilize_fixture(uri, record.id).unwrap();
        assert_eq!(id, uuid::Uuid::from_u128(1));
        assert_eq!(
            store
                .retrieve(&RetrievalQuery::new("p", "unrelated").with_files(["src/a.rs"]))
                .unwrap()[0]
                .record
                .id,
            id
        );
    }
}
