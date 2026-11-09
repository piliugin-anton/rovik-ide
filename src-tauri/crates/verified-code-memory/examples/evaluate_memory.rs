//! Reproducible retrieval baseline over incident-separated synthetic cases.
use serde_json::{json, Value};
use verified_code_memory::{MemoryEmbedder, NewMemory, RetrievalQuery, SharedMemoryStore};

fn fact(
    repo: &str,
    title: &str,
    content: &str,
    keys: &Value,
) -> Result<NewMemory, Box<dyn std::error::Error>> {
    let record = NewMemory::project_fact(repo, title, content);
    let entry = &keys["entries"][title];
    if entry.is_null() {
        return Ok(record);
    }
    if entry["claim"].as_str() != Some(content) {
        return Err(format!("search context does not describe the current claim: {title}").into());
    }
    let terms = entry["terms"]
        .as_array()
        .ok_or("search terms must be an array")?
        .iter()
        .map(|value| value.as_str().ok_or("search terms must be strings"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(record.with_tags(terms))
}

/// An experiment can replay real, externally computed embeddings through the
/// production SQLite index. Missing inputs fail instead of inventing vectors.
struct RecordedEmbeddings {
    dimensions: usize,
    input_character_limit: Option<usize>,
    vectors: std::collections::BTreeMap<String, Vec<f32>>,
}

impl MemoryEmbedder for RecordedEmbeddings {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn input_character_limit(&self) -> Option<usize> {
        self.input_character_limit
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

/// Keeps repeated fixture documents from running the native encoder again in
/// each temporary SQLite store. This cache exists only for one evaluation run.
struct MemoizedEmbeddings {
    inner: Box<dyn MemoryEmbedder>,
    vectors: std::sync::Mutex<std::collections::BTreeMap<String, Vec<f32>>>,
}

impl MemoryEmbedder for MemoizedEmbeddings {
    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
    fn input_character_limit(&self) -> Option<usize> {
        self.inner.input_character_limit()
    }
    fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
        // The encoder cannot observe a tail beyond its declared character
        // window. Reuse exactly identical effective inputs across verbose
        // fixture alternatives without changing their stored claims.
        let key = match self
            .inner
            .input_character_limit()
            .filter(|limit| *limit > 0)
        {
            Some(limit) => text.chars().take(limit).collect::<String>(),
            None => text.to_owned(),
        };
        if let Some(vector) = self
            .vectors
            .lock()
            .map_err(|_| verified_code_memory::VcmError::LockPoisoned)?
            .get(&key)
        {
            return Ok(vector.clone());
        }
        let vector = self.inner.embed(text)?;
        self.vectors
            .lock()
            .map_err(|_| verified_code_memory::VcmError::LockPoisoned)?
            .insert(key, vector.clone());
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

fn dataset_input(config: &Value) -> std::io::Result<String> {
    match config.get("dataset_file") {
        Some(Value::String(path)) => std::fs::read_to_string(path),
        None => Ok(include_str!("../../../../scripts/eval/memory-cases.json").to_owned()),
        Some(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "dataset_file must be a path string",
        )),
    }
}

fn fixture_strings(value: &Value) -> Result<Vec<&str>, Box<dyn std::error::Error>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    value
        .as_array()
        .ok_or("fixture list must be an array")?
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| "fixture list entries must be strings".into())
        })
        .collect()
}

fn fixture_query(
    case: &Value,
    config: &Value,
) -> Result<RetrievalQuery, Box<dyn std::error::Error>> {
    let mut query = RetrievalQuery::new("project", case["task"].as_str().ok_or("missing task")?);
    if let Some(failure) = case["failure"].as_str() {
        query = query.with_failure(failure);
    }
    if let Some(file) = case["file"]
        .as_str()
        .filter(|_| config["query_mode"] != "intent_only")
    {
        query = query.with_files([file]);
    }
    if config["query_mode"] != "intent_only" {
        query.files.extend(
            fixture_strings(&case["files"])?
                .into_iter()
                .map(str::to_owned),
        );
    }
    query.limit = config["candidate_limit"].as_u64().unwrap_or(8) as usize;
    query = query.with_packet_budget(config["memory_budget"].as_u64().unwrap_or(1000) as usize);
    if !case["verifications"].is_null() {
        query.verifications = serde_json::from_value(case["verifications"].clone())?;
    }
    query.excluded_tags = fixture_strings(&case["excluded_tags"])?
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok(query)
}

fn populate_corpus(
    store: &SharedMemoryStore,
    corpus: &Value,
    primary_title: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if corpus.is_null() {
        return Ok(());
    }
    for entry in corpus.as_array().ok_or("corpus must be an array")? {
        let title = entry["title"].as_str().ok_or("missing corpus title")?;
        let repo = if entry["repo"].is_null() {
            "project"
        } else {
            entry["repo"].as_str().ok_or("invalid corpus repository")?
        };
        if repo == "project" && title == primary_title {
            continue;
        }
        let content = entry["content"].as_str().ok_or("missing corpus content")?;
        let mut input = NewMemory::project_fact(repo, title, content);
        input = input
            .with_files(fixture_strings(&entry["files"])?)
            .with_tags(fixture_strings(&entry["tags"])?);
        let record = store.remember(input)?;
        if serde_json::from_value::<Option<bool>>(entry["verified"].clone())?.unwrap_or(false) {
            store.verify(record.id, "fixture corpus evidence")?;
        }
        if serde_json::from_value::<Option<bool>>(entry["deprecated"].clone())?.unwrap_or(false) {
            store.deprecate(record.id, "fixture obsolete claim")?;
        }
    }
    Ok(())
}

#[derive(Default)]
struct RequiredEvidence {
    count: usize,
    delivered: usize,
    ranks: serde_json::Map<String, Value>,
    facts: Vec<Value>,
}

fn required_evidence(
    required: &Value,
    live: &[verified_code_memory::MemoryRecord],
    found: &[verified_code_memory::RetrievedMemory],
    packet: &str,
    shown: &[uuid::Uuid],
) -> Result<RequiredEvidence, Box<dyn std::error::Error>> {
    let mut evidence = RequiredEvidence::default();
    if required.is_null() {
        return Ok(evidence);
    }
    for fact in required.as_array().ok_or("required must be an array")? {
        let title = fact["title"].as_str().ok_or("missing required title")?;
        let content = fact["content"].as_str().ok_or("missing required content")?;
        let record = live
            .iter()
            .find(|r| r.title == title && r.content == content)
            .ok_or("required fact absent from corpus")?;
        evidence.count += 1;
        evidence.delivered += usize::from(shown.contains(&record.id) && packet.contains(content));
        evidence.ranks.insert(
            title.to_owned(),
            json!(found
                .iter()
                .position(|h| h.record.id == record.id)
                .map(|i| i + 1)),
        );
        evidence.facts.push(json!({
            "id":record.id,"title":title,"claim_hash":record.claim_hash,
            "rank":found.iter().position(|hit| hit.record.id==record.id).map(|i|i+1),
            "delivered":shown.contains(&record.id)&&packet.contains(content)
        }));
    }
    Ok(evidence)
}

fn fixture_claim_time(now: chrono::DateTime<chrono::Utc>) -> String {
    // Keep recency at one whole day with twelve hours of margin. A fixed
    // calendar date changed floating-point ranking ties as the benchmark aged.
    (now - chrono::Duration::hours(36)).to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Fixes fixture IDs and claim ages before indexing: random UUIDs or a changing
/// recency bonus otherwise change rank ties between identical replays.
fn stabilize_fixture(
    uri: &str,
    expected: uuid::Uuid,
) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
    let mut connection = rusqlite::Connection::open(uri)?;
    connection.pragma_update(None, "foreign_keys", false)?;
    let transaction = connection.transaction()?;
    let claim_time = fixture_claim_time(chrono::Utc::now());
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
        transaction.execute("UPDATE memories SET id=?1,created_at=?3,updated_at=?3,content_updated_at=?3 WHERE id=?2", rusqlite::params![id.to_string(),old,&claim_time])?;
    }
    transaction.commit()?;
    new_expected.ok_or_else(|| "expected fixture record disappeared".into())
}

fn rerank_lexical(
    index: &verified_code_memory::semantic::SemanticIndex,
    query: &RetrievalQuery,
    embedder: &dyn MemoryEmbedder,
    threshold: f32,
    baseline: &[verified_code_memory::RetrievedMemory],
) -> verified_code_memory::Result<Vec<verified_code_memory::RetrievedMemory>> {
    embedder
        .embed(&format!("{}\n", query.task))
        .and_then(|vector| {
            let semantic = index.search(query, &vector, threshold)?;
            let mut ranked: Vec<_> = baseline
                .iter()
                .filter_map(|hit| {
                    let protected = query.task.trim().eq_ignore_ascii_case(&hit.record.title)
                        || query.files.iter().any(|file| {
                            hit.record.files.iter().any(|stored| {
                                verified_code_memory::retrieval::path_overlap(file, stored)
                            })
                        });
                    let similarity = semantic
                        .iter()
                        .find(|(record, _)| record.id == hit.record.id)
                        .map(|(_, score)| *score);
                    if !protected && similarity.is_none() {
                        return None;
                    }
                    let mut hit = hit.clone();
                    hit.score = similarity.unwrap_or(1.0);
                    hit.reason.push_str(", bi-encoder relevance gate");
                    Some(hit)
                })
                .collect();
            ranked.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then(a.record.id.cmp(&b.record.id))
            });
            Ok(ranked)
        })
}

// Rotate capture age independently of title/UUID, so an indexing budget cannot
// accidentally benefit from the expected claim always being among the newest.
fn capture_order(uri: &str, offset: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = rusqlite::Connection::open(uri)?;
    let transaction = connection.transaction()?;
    let mut ids = {
        let mut statement =
            transaction.prepare("SELECT id FROM memories WHERE repo='project' ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if !ids.is_empty() {
        let length = ids.len();
        ids.rotate_left(offset % length);
    }
    for (age, id) in ids.iter().enumerate() {
        transaction.execute(
            "UPDATE memories SET updated_at=strftime('%Y-%m-%dT%H:%M:%S+00:00', '2026-11-07', ?2 || ' seconds') WHERE id=?1",
            rusqlite::params![id, i64::try_from(age)?],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = experiment_config(&std::env::args().skip(1).collect::<Vec<_>>())?;
    let input = dataset_input(&config)?;
    let dataset: Value = serde_json::from_str(&input)?;
    let keys = match config["context_keys_file"].as_str() {
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
        None => json!({}),
    };
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
                input_character_limit: recorded["input_character_limit"]
                    .as_u64()
                    .map(|n| n as usize),
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
                let dimensions: usize = std::env::var("ROVIK_MEMORY_EVAL_DIMENSIONS")?.parse()?;
                let revision = std::env::var("ROVIK_MEMORY_EVAL_REVISION")?;
                let threshold: f32 = std::env::var("ROVIK_MEMORY_EVAL_THRESHOLD")?.parse()?;
                Some((
                    Box::new(verified_code_memory::embedding::OnnxMiniLm::new(
                        std::path::Path::new(&model),
                    )?),
                    revision,
                    dimensions,
                    threshold,
                ))
            }
            Err(_) => None,
        }
    };
    let backend = backend.map(|(inner, revision, dimensions, threshold)| {
        (
            MemoizedEmbeddings {
                inner,
                vectors: Default::default(),
            },
            revision,
            dimensions,
            threshold,
        )
    });
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
        let record =
            store.remember(fact("project", title, text("content")?, &keys)?.with_files([file]))?;
        store.verify(record.id, "synthetic fixture evidence")?;
        let expected = if case["supersede"] == true {
            store.deprecate(record.id, "explicit fixture update")?;
            let replacement = store.remember(fact("project", title, text("content")?, &keys)?)?;
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
            store.remember(fact(
                "project",
                other["title"].as_str().ok_or("missing title")?,
                other["content"].as_str().ok_or("missing content")?,
                &keys,
            )?)?;
        }
        populate_corpus(&store, &dataset["corpus"], title)?;
        populate_corpus(&store, &case["corpus"], title)?;
        // An identical record in another project must never leak.
        store.remember(fact("other", title, text("content")?, &keys)?)?;
        let expected = if deterministic {
            stabilize_fixture(fixture_uri, expected)?
        } else {
            expected
        };
        if let Some(offset) = config["capture_offset"].as_u64() {
            if !deterministic {
                return Err("capture_offset requires deterministic fixtures".into());
            }
            capture_order(fixture_uri, offset as usize)?;
        }
        let query = fixture_query(case, &config)?;
        let excluded: Vec<String> = fixture_strings(&case["exclude_titles"])?
            .into_iter()
            .map(str::to_owned)
            .collect();
        let accept =
            |record: &verified_code_memory::MemoryRecord| !excluded.contains(&record.title);
        let started = std::time::Instant::now();
        let (baseline, telemetry) = store.retrieve_filtered_with_telemetry(&query, accept)?;
        let mut embedding_fallback = false;
        let mut found = if let Some((embedder, revision, dimensions, threshold)) = &backend {
            let index = verified_code_memory::semantic::SemanticIndex::new(
                store.clone(),
                revision,
                *dimensions,
            )?;
            let index = if let Some(words) = config["short_query_words"].as_u64() {
                index.with_short_query_scale(
                    words as usize,
                    config["short_query_scale"]
                        .as_f64()
                        .ok_or("missing short-query scale")? as f32,
                )?
            } else {
                index
            };
            index.backfill_limited(
                "project",
                embedder,
                config["backfill_limit"]
                    .as_u64()
                    .map_or(usize::MAX, |n| n as usize),
            )?;
            // Give the identical prohibited record a vector too: a semantic
            // scope check must not pass just because other projects lack an index.
            index.backfill("other", embedder)?;
            let retrieved = if config["rerank_lexical"] == true {
                rerank_lexical(&index, &query, embedder, *threshold, &baseline)
            } else if config["semantic_only"] == true {
                embedder
                    .embed(&format!("{}\n", query.task))
                    .and_then(|vector| {
                        index
                            .search_filtered(&query, &vector, *threshold, accept)
                            .map(|hits| {
                                hits.into_iter()
                                    .map(|(record, score)| verified_code_memory::RetrievedMemory {
                                        record,
                                        score,
                                        reason: "semantic-only ablation".into(),
                                        components: Default::default(),
                                    })
                                    .collect()
                            })
                    })
            } else {
                index.retrieve_filtered(&query, embedder, *threshold, accept)
            };
            match retrieved {
                Ok(found) => found,
                Err(verified_code_memory::VcmError::EmbeddingUnavailable(
                    "no vocabulary vector",
                )) => {
                    embedding_fallback = true;
                    if config["semantic_only"] == true {
                        Vec::new()
                    } else {
                        baseline
                    }
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            baseline
        };
        let candidate_rank = found
            .iter()
            .position(|hit| hit.record.id == expected)
            .map(|at| at + 1);
        found.truncate(8);
        let memory_budget = config["memory_budget"].as_u64().unwrap_or(1000) as usize;
        let evidence = store.render_packet_with_evidence(&found, memory_budget);
        let (packet, shown, complete) =
            (evidence.text, evidence.rendered_ids, evidence.complete_ids);
        let primary_delivered = complete.contains(&expected) && packet.contains(text("content")?);
        let live = store.list("project", "")?;
        let additional = required_evidence(&case["required"], &live, &found, &packet, &complete)?;
        let required_count = 1 + additional.count;
        let required_delivered = usize::from(primary_delivered) + additional.delivered;
        let required_ranks = additional.ranks;
        let required_facts = additional.facts;
        let answer_delivered = required_delivered == required_count;
        let rank = found
            .iter()
            .position(|hit| hit.record.id == expected)
            .map(|at| at + 1);
        let prohibited = found.iter().any(|hit| {
            hit.record.repo != "project"
                || !accept(&hit.record)
                || hit.record.verification == verified_code_memory::Verification::Deprecated
                || (!query.verifications.is_empty()
                    && !query.verifications.contains(&hit.record.verification))
                || hit
                    .record
                    .tags
                    .iter()
                    .any(|tag| query.excluded_tags.contains(tag))
        });
        let full_text_ids = complete;
        let primary_claim_hash = store.get(expected)?.claim_hash;
        rows.push(json!({"id":case["id"],"incident":case["incident"],"split":case["split"],"category":case["category"],"answerable":case["answerable"],"primary_id":expected,"primary_claim_hash":primary_claim_hash,"rank":rank,"candidate_rank":candidate_rank,"answer_delivered":answer_delivered,"primary_delivered":primary_delivered,"required_delivered":required_delivered,"required_count":required_count,"required_ranks":required_ranks,"required_facts":required_facts,"rendered_ids":shown,"full_text_ids":full_text_ids,"packet_tokens":verified_code_memory::retrieval::estimate_tokens(&packet),"returned":found.len(),"returned_ids":found.iter().map(|hit|hit.record.id.to_string()).collect::<Vec<_>>(),"prohibited":prohibited,"embedding_fallback":embedding_fallback,"latency_us":started.elapsed().as_micros(),"telemetry":telemetry}));
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
        groups.insert(split.into(), json!({"cases":subset.len(),"recall_at_8":recall,"candidate_recall":answerable.iter().filter(|r|r["candidate_rank"].is_number()).count() as f64 / answerable.len() as f64,"delivered_recall":answerable.iter().filter(|r|r["answer_delivered"]==true).count() as f64 / answerable.len() as f64,"mrr":mrr,"ndcg_at_8":ndcg,"no_match_injection_rate":negatives.iter().filter(|r|r["returned"]!=0).count() as f64 / negatives.len() as f64}));
    }
    if let Some(path) = config["save_vectors_file"].as_str() {
        let (backend, revision, dimensions, _) = backend
            .as_ref()
            .ok_or("saving vectors requires an embedding backend")?;
        let vectors = backend
            .vectors
            .lock()
            .map_err(|_| verified_code_memory::VcmError::LockPoisoned)?;
        std::fs::write(
            path,
            serde_json::to_vec(
                &json!({"revision":revision,"dimensions":dimensions,"input_character_limit":backend.input_character_limit(),"vectors":&*vectors}),
            )?,
        )?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"dataset_version":dataset["version"],"dataset_fingerprint":verified_code_memory::claim_hash("memory-eval-v1",&input),"experiment_config":config,"policy":if config["rerank_lexical"]==true {"bi-encoder-gated"} else if config["semantic_only"]==true {"semantic-only"} else if backend.is_some() {"hybrid"} else {"lexical-file"},"memory_off_recall":0,"model":backend.as_ref().map(|(_,revision,dimensions,threshold)| json!({"revision":revision,"dimensions":dimensions,"threshold":threshold})),"answer_quality_evaluated":false,"groups":groups,"cases":rows})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memoized_embeddings_share_only_identical_effective_character_inputs() {
        use std::sync::{atomic::AtomicUsize, atomic::Ordering, Arc};
        struct Limited {
            calls: Arc<AtomicUsize>,
        }
        impl MemoryEmbedder for Limited {
            fn dimensions(&self) -> usize {
                1
            }
            fn input_character_limit(&self) -> Option<usize> {
                Some(3)
            }
            fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(vec![
                    text.chars().take(3).map(|c| c as u32).sum::<u32>() as f32
                ])
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let cached = MemoizedEmbeddings {
            inner: Box::new(Limited {
                calls: calls.clone(),
            }),
            vectors: Default::default(),
        };
        let first = cached.embed("é猫a first archive").unwrap();
        assert_eq!(first, cached.embed("é猫a second archive").unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_ne!(first, cached.embed("é猫b third archive").unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn deterministic_claim_age_stays_one_day_across_calendar_boundaries() {
        for text in [
            "2026-11-08T00:00:00+00:00",
            "2026-11-09T23:59:59+00:00",
            "2027-01-01T00:00:00+00:00",
        ] {
            let now = chrono::DateTime::parse_from_rfc3339(text)
                .unwrap()
                .with_timezone(&chrono::Utc);
            let stamp = fixture_claim_time(now);
            let claim = chrono::DateTime::parse_from_rfc3339(&stamp)
                .unwrap()
                .with_timezone(&chrono::Utc);
            assert_eq!((now - claim).num_days(), 1);
            assert_eq!(stamp.len(), 25);
        }
    }

    #[test]
    fn multiple_fixture_files_reach_lookup_and_intent_only_omits_both_file_fields() {
        let case = json!({"task":"lookup","file":"src/a.rs","files":["src/b.rs"]});
        assert_eq!(
            fixture_query(&case, &json!({})).unwrap().files,
            ["src/a.rs", "src/b.rs"]
        );
        assert!(fixture_query(&case, &json!({"query_mode":"intent_only"}))
            .unwrap()
            .files
            .is_empty());
        assert!(fixture_query(&json!({"task":"lookup","files":[false]}), &json!({})).is_err());
    }

    #[test]
    fn malformed_fixture_filters_fail_instead_of_silently_removing_protections() {
        for value in [json!("hidden"), json!([true])] {
            assert!(fixture_strings(&value).is_err());
        }
        assert!(fixture_query(
            &json!({"task":"lookup","verifications":"verified"}),
            &json!({})
        )
        .is_err());
        let store = SharedMemoryStore::open_in_memory().unwrap();
        assert!(populate_corpus(
            &store,
            &json!([{"title":"bad","content":"claim","verified":"yes"}]),
            "none"
        )
        .is_err());
    }

    #[test]
    fn composed_answers_require_each_claims_full_text_in_the_rendered_packet() {
        let store = SharedMemoryStore::open_in_memory().unwrap();
        let corpus = json!([
            {"title":"alpha","content":"Alpha constraint.","verified":true},
            {"title":"beta","content":"Beta constraint.","verified":true}
        ]);
        populate_corpus(&store, &corpus, "no-primary").unwrap();
        let query = RetrievalQuery::new("project", "alpha beta");
        let hits = store.retrieve(&query).unwrap();
        let (packet, shown) = store.render_packet_traced(&hits, 1000);
        let required = json!([{"title":"alpha","content":"Alpha constraint."},{"title":"beta","content":"Beta constraint."}]);
        let live = store.list("project", "").unwrap();
        let full = required_evidence(&required, &live, &hits, &packet, &shown).unwrap();
        assert_eq!((full.delivered, full.count), (2, 2));
        let missing_text =
            required_evidence(&required, &live, &hits, "Alpha constraint.", &shown).unwrap();
        assert_eq!((missing_text.delivered, missing_text.count), (1, 2));
        assert!(required_evidence(
            &json!([{"title":"missing","content":"absent"}]),
            &live,
            &hits,
            &packet,
            &shown
        )
        .is_err());
    }

    #[test]
    fn same_title_requirements_keep_separate_packet_delivery_evidence() {
        let store = SharedMemoryStore::open_in_memory().unwrap();
        let corpus = json!([
            {"title":"Context","content":"First constraint.","verified":true},
            {"title":"Context","content":"Second constraint.","verified":true}
        ]);
        populate_corpus(&store, &corpus, "none").unwrap();
        let hits = store
            .retrieve(&RetrievalQuery::new("project", "Context"))
            .unwrap();
        let (_, shown) = store.render_packet_traced(&hits, 1000);
        let evidence = required_evidence(
            &corpus,
            &store.list("project", "").unwrap(),
            &hits,
            "First constraint.",
            &shown,
        )
        .unwrap();
        assert_eq!((evidence.count, evidence.delivered), (2, 1));
        assert_eq!(evidence.facts.len(), 2);
        assert_ne!(evidence.facts[0]["id"], evidence.facts[1]["id"]);
        assert_eq!(evidence.facts[0]["delivered"], true);
        assert_eq!(evidence.facts[1]["delivered"], false);
        assert!(evidence.facts.iter().all(|fact| fact["rank"].is_number()));
    }

    #[test]
    fn fixture_corpus_trust_tags_and_caller_filters_reach_the_real_lookup() {
        let store = SharedMemoryStore::open_in_memory().unwrap();
        populate_corpus(
            &store,
            &json!([
                {"title":"wanted","content":"Constraint.","verified":true},
                {"title":"private","content":"Constraint.","verified":true,"tags":["hidden"]},
                {"title":"denied","content":"Constraint.","verified":true},
                {"title":"provisional","content":"Constraint."}
            ]),
            "no-primary",
        )
        .unwrap();
        let query = fixture_query(
            &json!({"task":"constraint","verifications":["verified"],"excluded_tags":["hidden"]}),
            &json!({}),
        )
        .unwrap();
        let hits = store
            .retrieve_filtered(&query, |r| r.title != "denied")
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.title, "wanted");
    }

    #[test]
    fn fixture_failure_text_reaches_the_real_retriever() {
        let store = SharedMemoryStore::open_in_memory().unwrap();
        let wanted = store
            .remember(NewMemory::procedure(
                "project",
                "sqlite",
                "Serialize locked database writers.",
            ))
            .unwrap();
        let case = json!({"task":"Fix the tests", "failure":"database is locked"});
        assert!(store
            .retrieve(&RetrievalQuery::new("project", "Fix the tests"))
            .unwrap()
            .is_empty());
        let hits = store
            .retrieve(&fixture_query(&case, &json!({})).unwrap())
            .unwrap();
        assert_eq!(hits[0].record.id, wanted.id);
    }

    #[test]
    fn capture_offsets_change_which_claims_fit_an_indexing_budget_without_changing_claims() {
        let uri = "file:capture-order-test?mode=memory&cache=shared";
        let store = SharedMemoryStore::open(uri).unwrap();
        for n in 0..12 {
            store
                .remember(NewMemory::project_fact(
                    "project",
                    format!("claim-{n}"),
                    format!("body {n}"),
                ))
                .unwrap();
        }
        capture_order(uri, 0).unwrap();
        let first = store.list("project", "").unwrap();
        assert!(first
            .windows(2)
            .all(|pair| pair[0].updated_at > pair[1].updated_at));
        capture_order(uri, 7).unwrap();
        let second = store.list("project", "").unwrap();
        assert_ne!(first[0].id, second[0].id);
        assert_eq!(first.len(), second.len());
        for record in &first {
            assert_eq!(
                record.claim_hash,
                second
                    .iter()
                    .find(|r| r.id == record.id)
                    .unwrap()
                    .claim_hash
            );
        }
        capture_order(uri, 7 + 12).unwrap();
        assert_eq!(
            second.iter().map(|r| r.id).collect::<Vec<_>>(),
            store
                .list("project", "")
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn evaluation_caches_each_successful_input_without_caching_provider_failures() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        struct Count(Arc<AtomicUsize>);
        impl MemoryEmbedder for Count {
            fn dimensions(&self) -> usize {
                2
            }
            fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                if text == "missing" {
                    return Err(verified_code_memory::VcmError::EmbeddingUnavailable(
                        "fixture failure",
                    ));
                }
                Ok(vec![text.len() as f32, 1.0])
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let model = MemoizedEmbeddings {
            inner: Box::new(Count(calls.clone())),
            vectors: Default::default(),
        };
        assert_eq!(model.embed("alpha").unwrap(), model.embed("alpha").unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_ne!(model.embed("alpha").unwrap(), model.embed("beta").unwrap());
        for _ in 0..2 {
            assert!(model.embed("missing").is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_recorded_model_never_fabricates_an_unrecorded_embedding() {
        let model = RecordedEmbeddings {
            dimensions: 2,
            input_character_limit: None,
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
    fn fresh_datasets_are_loaded_explicitly_and_missing_inputs_fail() {
        assert_eq!(
            dataset_input(&json!({})).unwrap(),
            include_str!("../../../../scripts/eval/memory-cases.json")
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fresh.json");
        let fixture = "{\"version\":2,\"cases\":[]}";
        std::fs::write(&path, fixture).unwrap();
        assert_eq!(
            dataset_input(&json!({"dataset_file":path})).unwrap(),
            fixture
        );
        assert!(
            dataset_input(&json!({"dataset_file":directory.path().join("missing.json")})).is_err()
        );
        assert!(dataset_input(&json!({"dataset_file":true})).is_err());
    }

    #[test]
    fn index_context_cannot_be_attached_to_an_edited_claim() {
        let keys =
            json!({"entries":{"cache":{"claim":"invalidate entries","terms":["stale results"]}}});
        let record = fact("p", "cache", "invalidate entries", &keys).unwrap();
        assert_eq!(record.content, "invalidate entries");
        assert_eq!(record.tags, vec!["stale results"]);
        assert!(fact("p", "cache", "retain entries", &keys).is_err());
    }

    #[test]
    fn the_relevance_gate_rejects_incidental_words_and_preserves_explicit_files() {
        let store = SharedMemoryStore::open_in_memory().unwrap();
        let relevant = store
            .remember(NewMemory::project_fact(
                "p",
                "retry",
                "racing retries require a lock",
            ))
            .unwrap();
        let incidental = store
            .remember(NewMemory::project_fact(
                "p",
                "simulator",
                "racing game statistics",
            ))
            .unwrap();
        let file = store
            .remember(
                NewMemory::project_fact("p", "constraint", "special convention")
                    .with_files(["src/required.rs"]),
            )
            .unwrap();
        let model = RecordedEmbeddings {
            dimensions: 2,
            input_character_limit: None,
            vectors: [
                ("racing\n".into(), vec![1.0, 0.0]),
                (
                    format!("{}\n{}", relevant.title, relevant.content),
                    vec![1.0, 0.0],
                ),
                (
                    format!("{}\n{}", incidental.title, incidental.content),
                    vec![0.0, 1.0],
                ),
                (format!("{}\n{}", file.title, file.content), vec![0.0, 1.0]),
            ]
            .into(),
        };
        let index =
            verified_code_memory::semantic::SemanticIndex::new(store.clone(), "test", 2).unwrap();
        index.backfill("p", &model).unwrap();
        let query = RetrievalQuery::new("p", "racing").with_files(["src/required.rs"]);
        let before = store.retrieve(&query).unwrap();
        assert_eq!(before.len(), 3);
        let found = rerank_lexical(&index, &query, &model, 0.8, &before).unwrap();
        let ids: Vec<_> = found.iter().map(|hit| hit.record.id).collect();
        assert!(ids.contains(&relevant.id));
        assert!(ids.contains(&file.id));
        assert!(!ids.contains(&incidental.id));
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
