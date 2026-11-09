//! Opt-in fixture diagnostics. Read the actual claim-bound index; never create
//! gold vectors that would conceal an incomplete backfill.
use serde_json::{json, Value};
use uuid::Uuid;
use verified_code_memory::{
    MemoryEmbedder, MemoryRecord, RetrievalQuery, RetrievedMemory, SharedMemoryStore, Verification,
};

type Error = Box<dyn std::error::Error>;
type Scored = Vec<(MemoryRecord, f32)>;

fn similarity(a: &[f32], b: &[f32]) -> Result<f32, Error> {
    if a.len() != b.len() || a.is_empty() || a.iter().chain(b).any(|v| !v.is_finite()) {
        return Err("invalid diagnostic vector".into());
    }
    // Use the production cosine's f64 accumulation and f32 result, including
    // negative scores below the search API's minimum supported cutoff.
    let norm = |v: &[f32]| v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let denominator = norm(a) * norm(b);
    if denominator == 0.0 {
        return Err("zero diagnostic vector".into());
    }
    Ok((a
        .iter()
        .zip(b)
        .map(|(a, b)| *a as f64 * *b as f64)
        .sum::<f64>()
        / denominator)
        .clamp(-1.0, 1.0) as f32)
}

fn excluded(
    record: &MemoryRecord,
    query: &RetrievalQuery,
    accept: &impl Fn(&MemoryRecord) -> bool,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if record.repo != query.repo {
        reasons.push("repository");
    }
    if !accept(record) {
        reasons.push("caller");
    }
    if record.verification == Verification::Deprecated {
        reasons.push("deprecated");
    }
    if !query.verifications.is_empty() && !query.verifications.contains(&record.verification) {
        reasons.push("verification");
    }
    if record.tags.iter().any(|t| query.excluded_tags.contains(t)) {
        reasons.push("tag");
    }
    reasons
}

fn scores(
    connection: &rusqlite::Connection,
    table: &str,
    store: &SharedMemoryStore,
    query: &RetrievalQuery,
    model: &str,
    vector: &[f32],
    accept: &impl Fn(&MemoryRecord) -> bool,
) -> Result<(Scored, Scored), Error> {
    // Table names are internal constants supplied below, never config text.
    let mut statement = connection.prepare(&format!("SELECT v.memory_id,v.vector_json FROM {table} v JOIN memories m ON m.id=v.memory_id WHERE m.repo=?1 AND v.model=?2 AND v.claim_hash=m.claim_hash"))?;
    let rows = statement.query_map(rusqlite::params![query.repo, model], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut all = Vec::new();
    for row in rows {
        let (id, data) = row?;
        let record = store.get(Uuid::parse_str(&id)?)?;
        let stored: Vec<f32> = serde_json::from_str(&data)?;
        all.push((record, similarity(vector, &stored)?));
    }
    all.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
    let mut eligible: Vec<(MemoryRecord, f32)> = Vec::new();
    for (record, score) in &all {
        if excluded(record, query, accept).is_empty()
            && !eligible.iter().any(|(r, _)| r.same_context_claim(record))
        {
            eligible.push((record.clone(), *score));
        }
    }
    Ok((all, eligible))
}

fn target_score(
    all: &[(MemoryRecord, f32)],
    eligible: &[(MemoryRecord, f32)],
    id: Uuid,
    cutoff: f32,
    limit: usize,
) -> Value {
    let score = all.iter().find(|(r, _)| r.id == id).map(|(_, s)| *s);
    let target = all.iter().find(|(r, _)| r.id == id).map(|(r, _)| r);
    let representative = target.and_then(|target| {
        eligible
            .iter()
            .enumerate()
            .find(|(_, (r, _))| r.same_context_claim(target))
    });
    let rank = representative.map(|(i, _)| i + 1);
    json!({"indexed":score.is_some(),"cosine":score,"eligible_distinct_rank":rank,"representative_id":representative.map(|(_, (r, _))|r.id),"cutoff":cutoff,"passes_cutoff":score.is_some_and(|s|s>=cutoff),"within_pool":rank.is_some_and(|r|r<=limit)&&score.is_some_and(|s|s>=cutoff)})
}

fn stage(
    selected: bool,
    full: bool,
    exclusions: bool,
    lexical: bool,
    views: &[Value],
) -> &'static str {
    if selected {
        return if full {
            "complete"
        } else {
            "selected_not_full"
        };
    }
    if exclusions {
        return "ineligible";
    }
    if lexical || views.iter().any(|v| v["within_pool"] == true) {
        return "candidate_or_supplementary_guard";
    }
    if views
        .iter()
        .any(|v| v["passes_cutoff"] == true && v["eligible_distinct_rank"].is_number())
    {
        return "semantic_pool_limit";
    }
    if views.iter().all(|v| v["indexed"] == false) {
        return "unindexed";
    }
    "below_semantic_admission"
}

fn append_preview(
    store: &SharedMemoryStore,
    query: &RetrievalQuery,
    found: &[RetrievedMemory],
    full: &[Uuid],
    record: &MemoryRecord,
    budget: usize,
) -> Value {
    if found.len() >= query.limit {
        return json!({"accepted":false,"reason":"result_limit"});
    }
    if found
        .iter()
        .any(|hit| hit.record.same_context_claim(record))
    {
        return json!({"accepted":false,"reason":"existing_context_claim"});
    }
    let mut appended = found.to_vec();
    appended.push(RetrievedMemory {
        record: record.clone(),
        score: found.last().map_or(1.0 / 61.0, |hit| hit.score) / 2.0,
        reason: "evaluation-only additive preview".into(),
        components: Default::default(),
    });
    let packet = store.render_packet_with_evidence(&appended, budget);
    if !packet.complete_ids.contains(&record.id)
        || full.iter().any(|id| !packet.complete_ids.contains(id))
    {
        return json!({"accepted":false,"reason":"packet_guard"});
    }
    json!({"accepted":true,"returned_ids":appended.iter().map(|h|h.record.id).collect::<Vec<_>>(),"rendered_ids":packet.rendered_ids,"full_text_ids":packet.complete_ids,"packet_tokens":verified_code_memory::retrieval::estimate_tokens(&packet.text)})
}

/// Reports observed fixture/index state without changing retrieval or stored records.
#[allow(clippy::too_many_arguments)]
pub(super) fn collect(
    uri: &str,
    store: &SharedMemoryStore,
    query: &RetrievalQuery,
    embedder: &dyn MemoryEmbedder,
    model: &str,
    minimum: f32,
    config: &Value,
    targets: &[Uuid],
    lexical: &[Uuid],
    found: &[RetrievedMemory],
    full: &[Uuid],
    accept: impl Fn(&MemoryRecord) -> bool,
) -> Result<Value, Error> {
    let connection = rusqlite::Connection::open(uri)?;
    let text = format!("{}\n{}", query.task, query.failure.as_deref().unwrap_or(""));
    let words = text.split_whitespace().count();
    let cutoff = match (
        config["short_query_words"].as_u64(),
        config["short_query_scale"].as_f64(),
    ) {
        (Some(limit), Some(scale))
            if words > 0
                && words <= limit as usize
                && !verified_code_memory::retrieval::build_fts_query(&[&text]).is_empty() =>
        {
            minimum * scale as f32
        }
        _ => minimum,
    };
    let mut probes = vec![("task", text)];
    if let Some(failure) = query.failure.as_deref().filter(|s| !s.trim().is_empty()) {
        probes.push(("failure_probe", failure.to_owned()));
    }
    let mut target_views: Vec<Vec<Value>> = targets.iter().map(|_| Vec::new()).collect();
    let mut views = Vec::new();
    for (name, text) in probes {
        let vector = embedder.embed(&text)?;
        for (table, kind) in [
            ("memory_vectors", "primary"),
            ("memory_vector_conclusions", "conclusion"),
        ] {
            let threshold = if name == "task" && kind == "primary" {
                cutoff
            } else {
                minimum
            };
            let (all, eligible) =
                scores(&connection, table, store, query, model, &vector, &accept)?;
            // Admission research needs a ranking independent of expected facts.
            // Keep the same eligibility and exact-copy representatives as search.
            let leaders: Vec<_> = eligible
                .iter()
                .take(3)
                .map(|(r, s)| json!({"id":r.id,"title":r.title,"claim_hash":r.claim_hash,"cosine":s}))
                .collect();
            let candidate_pool: Vec<_> = eligible
                .iter()
                .take(query.limit)
                .map(|(r, s)| json!({"id":r.id,"title":r.title,"claim_hash":r.claim_hash,"cosine":s,"append_preview":append_preview(store,query,found,full,r,config["memory_budget"].as_u64().unwrap_or(1000) as usize)}))
                .collect();
            let competitors: Vec<_> = eligible
                .iter()
                .filter(|(r, _)| !targets.contains(&r.id))
                .take(3)
                .map(|(r, s)| json!({"id":r.id,"title":r.title,"cosine":s}))
                .collect();
            let mut facts = Vec::new();
            for (i, id) in targets.iter().enumerate() {
                let mut fact = target_score(&all, &eligible, *id, threshold, query.limit);
                fact["id"] = json!(id);
                fact["margin_to_best_nonrequired"] = json!(fact["cosine"]
                    .as_f64()
                    .zip(competitors.first().and_then(|c| c["cosine"].as_f64()))
                    .map(|(s, b)| s - b));
                target_views[i].push(fact.clone());
                facts.push(fact);
            }
            views.push(json!({"query_view":name,"index_view":kind,"probe_only":true,"indexed_records":all.len(),"eligible_distinct_records":eligible.len(),"best_eligible":leaders,"candidate_pool":candidate_pool,"best_nonrequired":competitors,"targets":facts}));
        }
    }
    let mut facts = Vec::new();
    for (i, id) in targets.iter().enumerate() {
        let record = store.get(*id)?;
        let exclusions = excluded(&record, query, &accept);
        let rank = found.iter().position(|h| h.record.id == *id).map(|i| i + 1);
        facts.push(json!({"id":id,"title":record.title,"claim_hash":record.claim_hash,"exclusions":exclusions,"lexical_rank":lexical.iter().position(|r|r==id).map(|i|i+1),"selected_rank":rank,"full":full.contains(id),"stage":stage(rank.is_some(),full.contains(id),!exclusions.is_empty(),lexical.contains(id),&target_views[i])}));
    }
    let selected: Vec<_> = found.iter().enumerate().map(|(i, hit)| json!({"id":hit.record.id,"title":hit.record.title,"claim_hash":hit.record.claim_hash,"rank":i+1,"reason":hit.reason,"full":full.contains(&hit.record.id),"claim_tokens":verified_code_memory::retrieval::estimate_tokens(&hit.record.content)})).collect();
    Ok(
        json!({"facts":facts,"views":views,"selected":selected,"note":"Read-only index probes after retrieval. Supplementary scores do not prove that a guarded lookup ran; candidate_or_supplementary_guard deliberately leaves that distinction open. Focused identifier placement and packet rollback are not traced by semantic probes. Nonrequired competitors may be useful context."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use verified_code_memory::{semantic::SemanticIndex, NewMemory};

    struct Model;
    impl MemoryEmbedder for Model {
        fn dimensions(&self) -> usize {
            2
        }
        fn embed(&self, text: &str) -> verified_code_memory::Result<Vec<f32>> {
            Ok(if text.starts_with("Gold\n") {
                vec![-0.6, 0.8]
            } else {
                vec![1.0, 0.0]
            })
        }
    }

    #[test]
    fn additive_previews_preserve_incumbents_and_reject_duplicates_limits_and_partial_packets() {
        let store = SharedMemoryStore::open_in_memory().unwrap();
        let anchor = store
            .remember(NewMemory::project_fact(
                "p",
                "Anchor",
                "Keep this source assertion.",
            ))
            .unwrap();
        let addition = store
            .remember(NewMemory::project_fact(
                "p",
                "Addition",
                "A second source assertion.",
            ))
            .unwrap();
        let found = vec![RetrievedMemory {
            record: anchor.clone(),
            score: 0.25,
            reason: "original lookup".into(),
            components: Default::default(),
        }];
        let mut query = RetrievalQuery::new("p", "task");
        query.limit = 2;
        let baseline = store.render_packet_with_evidence(&found, 1000);
        assert_eq!(baseline.complete_ids, [anchor.id]);
        let preview = append_preview(
            &store,
            &query,
            &found,
            &baseline.complete_ids,
            &addition,
            1000,
        );
        assert_eq!(preview["accepted"], true);
        assert_eq!(preview["returned_ids"], json!([anchor.id, addition.id]));
        assert_eq!(preview["full_text_ids"], json!([anchor.id, addition.id]));
        assert!(preview["packet_tokens"].as_u64().unwrap() <= 1000);
        let mut copy = anchor.clone();
        copy.id = Uuid::new_v4();
        assert_eq!(
            append_preview(&store, &query, &found, &baseline.complete_ids, &copy, 1000)["reason"],
            "existing_context_claim"
        );
        query.limit = 1;
        assert_eq!(
            append_preview(
                &store,
                &query,
                &found,
                &baseline.complete_ids,
                &addition,
                1000
            )["reason"],
            "result_limit"
        );
        query.limit = 2;
        let budget = verified_code_memory::retrieval::estimate_tokens(&baseline.text);
        assert_eq!(
            store
                .render_packet_with_evidence(&found, budget)
                .complete_ids,
            [anchor.id]
        );
        assert_eq!(
            append_preview(
                &store,
                &query,
                &found,
                &baseline.complete_ids,
                &addition,
                budget
            )["reason"],
            "packet_guard"
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].record, anchor);
        assert_eq!(found[0].score, 0.25);
    }

    #[test]
    fn eligible_leaders_match_filtered_search_without_using_expected_fact_labels() {
        let uri = format!(
            "file:diagnostic-leaders-{}?mode=memory&cache=shared",
            Uuid::new_v4()
        );
        let store = SharedMemoryStore::open(&uri).unwrap();
        let mut copies = Vec::new();
        for title in [
            "Copy",
            "Copy",
            "Leader A",
            "Leader B",
            "Hidden",
            "Deprecated",
            "Leader C",
            "Leader D",
        ] {
            let record = store
                .remember(NewMemory::project_fact("p", title, "An assertion."))
                .unwrap();
            store.verify(record.id, "source evidence").unwrap();
            copies.push(record);
        }
        store.deprecate(copies[5].id, "obsolete").unwrap();
        let tagged = store
            .remember(
                NewMemory::project_fact("p", "Tagged", "An assertion.").with_tags(["blocked"]),
            )
            .unwrap();
        store.verify(tagged.id, "source evidence").unwrap();
        store
            .remember(NewMemory::project_fact("other", "Foreign", "An assertion."))
            .unwrap();
        store
            .remember(NewMemory::project_fact("p", "Unverified", "An assertion."))
            .unwrap();
        let index = SemanticIndex::new(store.clone(), "test", 2).unwrap();
        index.backfill("p", &Model).unwrap();
        index.backfill("other", &Model).unwrap();
        let mut query = RetrievalQuery::new("p", "wanted")
            .with_verifications([Verification::Verified])
            .without_tags(["blocked"]);
        query.limit = 4;
        let accept = |r: &MemoryRecord| r.id != copies[4].id;
        let production = index
            .search_filtered(&query, &[1.0, 0.0], 0.3, accept)
            .unwrap();
        assert_eq!(production.len(), 4);
        let before = store.list("p", "").unwrap();
        let probe = |targets: &[Uuid]| {
            collect(
                &uri,
                &store,
                &query,
                &Model,
                "test",
                0.3,
                &json!({}),
                targets,
                &[],
                &[],
                &[],
                accept,
            )
            .unwrap()
        };
        let first = probe(&[copies[0].id]);
        let second = probe(&[copies[2].id, copies[3].id]);
        let leaders = &first["views"][0]["best_eligible"];
        assert_eq!(leaders, &second["views"][0]["best_eligible"]);
        assert_eq!(leaders.as_array().unwrap().len(), 3);
        let pool = &first["views"][0]["candidate_pool"];
        assert_eq!(pool, &second["views"][0]["candidate_pool"]);
        assert_eq!(pool.as_array().unwrap().len(), query.limit);
        for (leader, (record, score)) in pool.as_array().unwrap().iter().zip(&production) {
            assert_eq!(leader["id"], json!(record.id));
            assert_eq!(leader["claim_hash"], json!(record.claim_hash));
            assert_eq!(leader["cosine"], json!(score));
        }
        assert_eq!(store.list("p", "").unwrap(), before);
    }

    #[test]
    fn probes_report_negative_similarity_filters_and_missing_vectors_without_changing_records() {
        let uri = format!(
            "file:diagnostics-{}?mode=memory&cache=shared",
            Uuid::new_v4()
        );
        let store = SharedMemoryStore::open(&uri).unwrap();
        let gold = store
            .remember(NewMemory::project_fact("p", "Gold", "Expected assertion."))
            .unwrap();
        let competitor = store
            .remember(NewMemory::project_fact(
                "p",
                "Competitor",
                "Another assertion.",
            ))
            .unwrap();
        let hidden = store
            .remember(NewMemory::project_fact("p", "Hidden", "Hidden assertion."))
            .unwrap();
        store
            .remember(NewMemory::project_fact(
                "other",
                "Foreign",
                "Foreign assertion.",
            ))
            .unwrap();
        store.verify(competitor.id, "source evidence").unwrap();
        store
            .remember(
                NewMemory::project_fact("p", "Tagged", "Excluded assertion.")
                    .with_tags(["blocked"]),
            )
            .unwrap();
        let index = SemanticIndex::new(store.clone(), "test", 2).unwrap();
        index.backfill("p", &Model).unwrap();
        index.backfill("other", &Model).unwrap();
        let query = RetrievalQuery::new("p", "wanted").without_tags(["blocked"]);
        let accept = |r: &MemoryRecord| r.id != hidden.id;
        let production = index
            .search_filtered(&query, &[1.0, 0.0], 0.3, accept)
            .unwrap();
        assert_eq!(
            production.iter().map(|(r, _)| r.id).collect::<Vec<_>>(),
            [competitor.id]
        );
        let before = store.list("p", "").unwrap();
        let result = collect(
            &uri,
            &store,
            &query,
            &Model,
            "test",
            0.3,
            &json!({}),
            &[gold.id],
            &[],
            &[],
            &[],
            accept,
        )
        .unwrap();
        let view = &result["views"][0];
        assert!((view["targets"][0]["cosine"].as_f64().unwrap() + 0.6).abs() < 0.000001);
        assert_eq!(view["best_nonrequired"][0]["id"], json!(competitor.id));
        assert_eq!(view["best_nonrequired"].as_array().unwrap().len(), 1);
        assert_eq!(result["facts"][0]["stage"], "below_semantic_admission");
        assert_eq!(store.list("p", "").unwrap(), before);
        let trusted = query.clone().with_verifications([Verification::Verified]);
        let trust_trace = collect(
            &uri,
            &store,
            &trusted,
            &Model,
            "test",
            0.3,
            &json!({}),
            &[gold.id],
            &[],
            &[],
            &[],
            accept,
        )
        .unwrap();
        assert_eq!(
            trust_trace["facts"][0]["exclusions"],
            json!(["verification"])
        );
        assert_eq!(trust_trace["facts"][0]["stage"], "ineligible");
        assert_eq!(
            trust_trace["views"][0]["best_nonrequired"][0]["id"],
            json!(competitor.id)
        );

        let literal = RetrievalQuery::new("p", "Gold");
        let selected = store.retrieve(&literal).unwrap();
        let packet = store.render_packet_with_evidence(&selected, 0);
        assert!(packet.complete_ids.is_empty());
        let trace = collect(
            &uri,
            &store,
            &literal,
            &Model,
            "test",
            0.3,
            &json!({}),
            &[gold.id],
            &[gold.id],
            &selected,
            &packet.complete_ids,
            accept,
        )
        .unwrap();
        assert_eq!(trace["facts"][0]["stage"], "selected_not_full");
        assert_eq!(trace["selected"][0]["id"], json!(gold.id));
        store.edit(gold.id, "Gold", "Changed assertion.").unwrap();
        let result = collect(
            &uri,
            &store,
            &query,
            &Model,
            "test",
            0.3,
            &json!({}),
            &[gold.id],
            &[],
            &[],
            &[],
            accept,
        )
        .unwrap();
        assert_eq!(result["views"][0]["targets"][0]["indexed"], false);
        assert_eq!(result["facts"][0]["stage"], "unindexed");
    }

    #[test]
    fn exact_copy_scores_name_the_same_representative_as_production_search() {
        let uri = format!(
            "file:diagnostic-copy-{}?mode=memory&cache=shared",
            Uuid::new_v4()
        );
        let store = SharedMemoryStore::open(&uri).unwrap();
        let first = store
            .remember(NewMemory::project_fact("p", "Copy", "Same assertion."))
            .unwrap();
        let second = store
            .remember(NewMemory::project_fact("p", "Copy", "Same assertion."))
            .unwrap();
        let index = SemanticIndex::new(store.clone(), "test", 2).unwrap();
        index.backfill("p", &Model).unwrap();
        let query = RetrievalQuery::new("p", "wanted");
        let production = index.search(&query, &[1.0, 0.0], 0.3).unwrap();
        assert_eq!(production.len(), 1);
        let result = collect(
            &uri,
            &store,
            &query,
            &Model,
            "test",
            0.3,
            &json!({}),
            &[first.id, second.id],
            &[],
            &[],
            &[],
            |_| true,
        )
        .unwrap();
        for target in result["views"][0]["targets"].as_array().unwrap() {
            assert_eq!(target["representative_id"], json!(production[0].0.id));
            assert_eq!(target["eligible_distinct_rank"], 1);
        }
    }

    #[test]
    fn classifications_distinguish_ineligible_crowded_selected_and_full_claims() {
        assert_eq!(stage(true, false, false, false, &[]), "selected_not_full");
        assert_eq!(stage(true, true, false, false, &[]), "complete");
        assert_eq!(stage(false, false, true, false, &[]), "ineligible");
        assert_eq!(
            stage(false, false, false, true, &[]),
            "candidate_or_supplementary_guard"
        );
        assert_eq!(
            stage(
                false,
                false,
                false,
                false,
                &[
                    json!({"indexed":true,"passes_cutoff":true,"eligible_distinct_rank":9,"within_pool":false})
                ]
            ),
            "semantic_pool_limit"
        );
        assert!(similarity(&[0.0, 0.0], &[1.0, 0.0]).is_err());
        assert!(similarity(&[f32::NAN], &[1.0]).is_err());
        assert!(similarity(&[1.0], &[1.0, 0.0]).is_err());
    }
}
