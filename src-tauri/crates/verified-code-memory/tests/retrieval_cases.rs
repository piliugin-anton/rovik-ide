//! The replayable retrieval cases: a corpus, a query and what the store must
//! answer, written as data in `fixtures/retrieval_cases.json` so a case can be
//! read without reading Rust.
//!
//! A case may say `titles` (the whole answer, in order), `includes` (each of
//! these is in the answer), `prohibited` (none of these is) and `count` (how
//! many memories come back). Every case is asked twice, because the same query
//! must answer in the same order.

use serde_json::Value;
use verified_code_memory::{MemoryStore, NewMemory, RetrievalQuery, Verification};

const CASES: &str = include_str!("fixtures/retrieval_cases.json");

fn string(entry: &Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{key:?} is a string in {entry}"))
        .to_owned()
}

fn maybe_string(entry: &Value, key: &str) -> Option<String> {
    entry.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn strings(entry: &Value, key: &str) -> Vec<String> {
    entry
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .unwrap_or_else(|| panic!("{key:?} holds strings in {entry}"))
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn verification(name: &str) -> Verification {
    match name {
        "provisional" => Verification::Provisional,
        "verified" => Verification::Verified,
        "deprecated" => Verification::Deprecated,
        other => panic!("unknown verification {other:?}"),
    }
}

fn memory(repo: &str, entry: &Value) -> NewMemory {
    let title = string(entry, "title");
    let content = string(entry, "content");
    let kind = maybe_string(entry, "kind").unwrap_or_else(|| "episode".to_owned());
    let built = match kind.as_str() {
        "episode" => NewMemory::episode(repo, title, content),
        "project_fact" => NewMemory::project_fact(repo, title, content),
        "procedure" => NewMemory::procedure(repo, title, content),
        other => panic!("unknown kind {other:?}"),
    };
    built
        .with_files(strings(entry, "files"))
        .with_tags(strings(entry, "tags"))
}

fn remember_corpus(store: &MemoryStore, repo: &str, corpus: &Value) {
    let entries = corpus
        .as_array()
        .unwrap_or_else(|| panic!("the corpus is a list of memories: {corpus}"));
    for entry in entries {
        let record = store.remember(memory(repo, entry)).unwrap();
        match string(entry, "verification").as_str() {
            "provisional" => {}
            "verified" => {
                store
                    .verify(record.id, format!("fixture: {}", record.title))
                    .unwrap();
            }
            "deprecated" => {
                store
                    .deprecate(record.id, "the fixture says it no longer holds")
                    .unwrap();
            }
            other => panic!("unknown verification {other:?}"),
        }
    }
}

fn query(repo: &str, spec: &Value) -> RetrievalQuery {
    let mut query = RetrievalQuery::new(repo, string(spec, "task"));
    let files = strings(spec, "files");
    if !files.is_empty() {
        query = query.with_files(files);
    }
    if let Some(failure) = maybe_string(spec, "failure") {
        query = query.with_failure(failure);
    }
    if let Some(limit) = spec.get("limit").and_then(Value::as_u64) {
        query = query.with_limit(limit as usize);
    }
    let verifications: Vec<Verification> = strings(spec, "verifications")
        .iter()
        .map(|name| verification(name))
        .collect();
    if !verifications.is_empty() {
        query = query.with_verifications(verifications);
    }
    let excluded = strings(spec, "excluded_tags");
    if !excluded.is_empty() {
        query = query.without_tags(excluded);
    }
    query
}

fn titles(store: &MemoryStore, query: &RetrievalQuery) -> Vec<String> {
    let found = store.retrieve(query).unwrap();
    found.into_iter().map(|hit| hit.record.title).collect()
}

#[test]
fn the_fixtures_cases_are_answered_the_way_they_expect() {
    let root: Value = serde_json::from_str(CASES).expect("the cases are JSON");
    let repo = string(&root, "repo");
    let cases = root
        .get("cases")
        .and_then(Value::as_array)
        .expect("the fixture holds a list of cases");
    assert!(
        cases.len() >= 7,
        "the fixture holds every behaviour it replays, not just a few: {}",
        cases.len()
    );

    for case in cases {
        let name = string(case, "name");
        let store = MemoryStore::open_in_memory().unwrap();
        remember_corpus(
            &store,
            &repo,
            case.get("corpus").expect("a case has a corpus"),
        );
        let asked = query(&repo, case.get("query").expect("a case has a query"));

        let answered = titles(&store, &asked);
        let again = titles(&store, &asked);
        assert_eq!(
            answered, again,
            "{name}: the same query is answered in the same order twice"
        );

        let expect = case.get("expect").expect("a case says what to expect");
        if let Some(count) = expect.get("count").and_then(Value::as_u64) {
            assert_eq!(
                answered.len() as u64,
                count,
                "{name}: how many memories come back: {answered:?}"
            );
        }
        if expect.get("titles").is_some() {
            assert_eq!(
                answered,
                strings(expect, "titles"),
                "{name}: the whole answer, in order"
            );
        }
        for title in strings(expect, "includes") {
            assert!(
                answered.contains(&title),
                "{name}: {title:?} is recalled, {answered:?}"
            );
        }
        for title in strings(expect, "prohibited") {
            assert!(
                !answered.contains(&title),
                "{name}: {title:?} is not recalled, {answered:?}"
            );
        }
    }
}
