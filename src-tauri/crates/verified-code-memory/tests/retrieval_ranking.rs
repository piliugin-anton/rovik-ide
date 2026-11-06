//! What the store recalls, and what it refuses to recall.
//!
//! A memory is only a candidate when it is about the task it is asked for:
//! it shares a word that names something with the task, or it names one of
//! the query's files, or it shares such a word with the failure. Trust,
//! kind, recency and success only order the memories that are about the
//! task; they never make an unrelated memory a candidate. A query nothing
//! matches is answered with nothing.

use verified_code_memory::retrieval::path_overlap;
use verified_code_memory::{MemoryStore, NewMemory, RetrievalQuery, Verification};

const REPO: &str = "payments-service";

/// A provisional episode about `files`.
fn episode(title: &str, content: &str, files: &[&str]) -> NewMemory {
    NewMemory::episode(REPO, title, content).with_files(files.iter().copied())
}

fn titles(store: &MemoryStore, query: &RetrievalQuery) -> Vec<String> {
    let found = store.retrieve(query).unwrap();
    found.into_iter().map(|hit| hit.record.title).collect()
}

fn ids(store: &MemoryStore, query: &RetrievalQuery) -> Vec<String> {
    let found = store.retrieve(query).unwrap();
    found
        .into_iter()
        .map(|hit| hit.record.id.to_string())
        .collect()
}

#[test]
fn the_memory_sharing_more_of_the_tasks_words_is_recalled_first() {
    let store = MemoryStore::open_in_memory().unwrap();
    // One trust state (`provisional`) and one kind (episode) for both, so
    // their words are the only thing that separates them: the first shares
    // queue, overflow, drops and jobs with the task, the second only queue
    // and overflow. They are remembered back to back, and the weaker match is
    // the newer of the two, so recency pulls against the order asserted here.
    let closer = store
        .remember(episode(
            "Queue overflow drops jobs",
            "When the worker queue overflow drops jobs, raise the buffer bound.",
            &[],
        ))
        .unwrap();
    let further = store
        .remember(episode(
            "Queue overflow",
            "Queue overflow is reported by the worker.",
            &[],
        ))
        .unwrap();

    let found = titles(
        &store,
        &RetrievalQuery::new(REPO, "queue overflow drops jobs"),
    );

    assert_eq!(
        found.first(),
        Some(&closer.title),
        "the memory that shares more of the task's words comes first: {found:?}"
    );
    assert!(
        found.contains(&further.title),
        "the other memory about the task is recalled too: {found:?}"
    );
}

#[test]
fn a_query_the_project_knows_nothing_about_recalls_nothing() {
    let store = MemoryStore::open_in_memory().unwrap();
    // A recent, verified procedure of the project: exactly the sort of
    // memory a "most recent records" fallback handed to every query.
    let procedure = store
        .remember(
            NewMemory::procedure(
                REPO,
                "Release the app",
                "Run cargo xtask release, then tag the commit.",
            )
            .with_files(["xtask/src/main.rs"]),
        )
        .unwrap();
    store.verify(procedure.id, "the release went out").unwrap();

    let (found, telemetry) = store
        .retrieve_with_telemetry(&RetrievalQuery::new(
            REPO,
            "weather forecast for Lisbon tomorrow",
        ))
        .unwrap();

    assert!(
        found.is_empty(),
        "a query that shares no word and no file with any memory is answered with nothing: {:?}",
        found
            .iter()
            .map(|memory| memory.record.title.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(telemetry.returned, 0, "nothing was returned");
}

#[test]
fn a_memory_about_one_of_the_tasks_files_is_eligible_although_others_match_the_words_better() {
    let store = MemoryStore::open_in_memory().unwrap();
    // Only its file ties this memory to the task: none of its words are the
    // task's words.
    let about_the_file = store
        .remember(episode(
            "The worker serializes its writes",
            "Writes go through one lock so nothing interleaves.",
            &["src/queue.rs"],
        ))
        .unwrap();
    // Newer memories that match the task's words: a limit that fits every
    // candidate, so the answer can't turn on how a file match is weighed
    // against a word match.
    for i in 0..3 {
        store
            .remember(episode(
                &format!("Checkout backlog note {i}"),
                "The checkout backlog is trimmed by hand each week.",
                &[],
            ))
            .unwrap();
    }

    let found = titles(
        &store,
        &RetrievalQuery::new(REPO, "trim the checkout backlog")
            .with_files(["src/queue.rs"])
            .with_limit(6),
    );

    assert!(
        found.contains(&about_the_file.title),
        "a memory that names one of the query's files is a candidate, \
         whatever the other memories say: {found:?}"
    );
}

#[test]
fn a_memory_about_one_of_the_tasks_files_is_recalled_from_under_hundreds_of_newer_memories() {
    let store = MemoryStore::open_in_memory().unwrap();
    let about_the_file = store
        .remember(episode(
            "The worker serializes its writes",
            "Writes go through one lock so nothing interleaves.",
            &["src/queue.rs"],
        ))
        .unwrap();
    // 210 memories remembered after it, none of them about this task: the
    // file match is the only reason to recall anything here.
    for i in 0..210 {
        store
            .remember(episode(
                &format!("Weekly maintenance {i}"),
                "Handled in passing.",
                &[],
            ))
            .unwrap();
    }

    let found = titles(
        &store,
        &RetrievalQuery::new(REPO, "trim the checkout backlog")
            .with_files(["src/queue.rs"])
            .with_limit(8),
    );

    assert!(
        found.contains(&about_the_file.title),
        "the older memory about the query's file is recalled, not the newer memories \
         the task says nothing about: {found:?}"
    );
}

#[test]
fn memories_excluded_by_tag_do_not_use_up_the_limit() {
    let store = MemoryStore::open_in_memory().unwrap();
    let mut permitted = Vec::new();
    for i in 0..2 {
        permitted.push(
            store
                .remember(episode(
                    &format!("Retry policy {i}"),
                    "The retry policy for the queue backs off twice.",
                    &[],
                ))
                .unwrap(),
        );
    }
    // Higher-scoring memories of another chat: verified, so all three would
    // take a slot before any provisional one.
    for i in 0..3 {
        let excluded = store
            .remember(
                episode(
                    &format!("Retry policy of another chat {i}"),
                    "The retry policy for the queue backs off twice.",
                    &[],
                )
                .with_tags(["chat-elsewhere"]),
            )
            .unwrap();
        store.verify(excluded.id, "tests passed").unwrap();
    }

    let found = titles(
        &store,
        &RetrievalQuery::new(REPO, "queue retry policy")
            .with_limit(2)
            .without_tags(["chat-elsewhere"]),
    );

    let mut sorted = found.clone();
    sorted.sort();
    let mut expected: Vec<String> = permitted
        .iter()
        .map(|memory| memory.title.clone())
        .collect();
    expected.sort();
    assert_eq!(
        sorted, expected,
        "the limit is filled with permitted memories, not used up by excluded ones: {found:?}"
    );
}

#[test]
fn asking_for_verified_memories_returns_them_even_when_provisional_ones_could_fill_the_limit() {
    let store = MemoryStore::open_in_memory().unwrap();
    let verified = store
        .remember(episode(
            "Retry policy is proven",
            "The retry policy for the queue backs off twice.",
            &[],
        ))
        .unwrap();
    store.verify(verified.id, "npm test passed").unwrap();
    for i in 0..8 {
        store
            .remember(episode(
                &format!("Retry policy guess {i}"),
                "The retry policy for the queue backs off twice.",
                &[],
            ))
            .unwrap();
    }

    let found = titles(
        &store,
        &RetrievalQuery::new(REPO, "queue retry policy")
            .with_limit(8)
            .with_verifications([Verification::Verified]),
    );

    assert_eq!(
        found,
        vec![verified.title.clone()],
        "only the proven memory is answered, although eight provisional ones could fill the limit"
    );
}

#[test]
fn the_same_query_answers_in_the_same_order_twice() {
    let store = MemoryStore::open_in_memory().unwrap();
    for (title, content) in [
        (
            "Webhook signature check",
            "Compare the HMAC in constant time.",
        ),
        (
            "Webhook signature check of the retries",
            "Compare the HMAC of every retried webhook.",
        ),
        ("Retry the webhook once", "One retry per webhook, no more."),
    ] {
        store.remember(episode(title, content, &[])).unwrap();
    }

    let query = RetrievalQuery::new(REPO, "webhook signature check");
    let first = ids(&store, &query);
    let second = ids(&store, &query);

    assert!(!first.is_empty(), "the query recalls the webhook memories");
    assert_eq!(
        first, second,
        "the same query is answered in the same order every time"
    );
}

#[test]
fn of_two_equally_relevant_memories_the_newer_one_is_recalled_first() {
    let store = MemoryStore::open_in_memory().unwrap();
    let older = store
        .remember(episode(
            "Webhook signature check",
            "Compare the HMAC in constant time.",
            &[],
        ))
        .unwrap();
    // The two memories are the same text, so only their age tells them
    // apart.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let newer = store
        .remember(episode(
            "Webhook signature check",
            "Compare the HMAC in constant time.",
            &[],
        ))
        .unwrap();

    let found = ids(
        &store,
        &RetrievalQuery::new(REPO, "webhook signature check"),
    );

    assert_eq!(
        found,
        vec![newer.id.to_string(), older.id.to_string()],
        "equally relevant memories are ordered newest first"
    );
}

#[test]
fn a_deprecated_memory_is_never_recalled_even_when_its_state_is_the_only_one_asked_for() {
    let store = MemoryStore::open_in_memory().unwrap();
    let superseded = store
        .remember(episode(
            "The old retry loop",
            "The retry loop backed off twice after every failure.",
            &[],
        ))
        .unwrap();
    store
        .deprecate(superseded.id, "a newer retry queue replaced it")
        .unwrap();

    let by_default = titles(&store, &RetrievalQuery::new(REPO, "retry loop backoff"));
    assert!(
        by_default.is_empty(),
        "a discontinued memory is not recalled: {by_default:?}"
    );

    let asked_only_for_deprecated = titles(
        &store,
        &RetrievalQuery::new(REPO, "retry loop backoff")
            .with_verifications([Verification::Deprecated]),
    );
    assert!(
        asked_only_for_deprecated.is_empty(),
        "asking only for deprecated memories still recalls none: {asked_only_for_deprecated:?}"
    );
    assert_eq!(
        store.get(superseded.id).unwrap().verification,
        Verification::Deprecated,
        "the memory itself is still there, discontinued"
    );
}

#[test]
fn a_file_is_matched_by_whole_path_segments() {
    // A suffix of the path that is not a whole segment is not the file.
    assert!(
        !path_overlap("src/notfoo.rs", "foo.rs"),
        "src/notfoo.rs is not foo.rs"
    );
    assert!(
        path_overlap("src/queue.rs", "queue.rs"),
        "the last segment of src/queue.rs is queue.rs"
    );
    assert!(
        path_overlap("src\\queue.rs", "queue.rs"),
        "a Windows separator is a separator"
    );
    assert!(
        path_overlap("./src/queue.rs", "queue.rs"),
        "a leading ./ is not a directory"
    );
    assert!(
        path_overlap("./src/queue.rs", "src/queue.rs"),
        "the same file written two ways"
    );
    assert!(
        path_overlap("src/queue.rs", "src/queue.rs"),
        "the same path is the same file"
    );
    assert!(
        !path_overlap("src/queue.rs", "src/queue/mod.rs"),
        "a directory is not the file of the same name"
    );
}
