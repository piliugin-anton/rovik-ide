//! The project's long-term memory: one store per project, and the project's own switch.
//!
//! These use only what `rovik_core::memory` makes public: `load`, `store_path`,
//! `MemoryState`, `stats`, the `MemoryEntry` a page reads, and `list`, `edit`, `remove`
//! and `clear`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use rovik_core::core::Paths;
use rovik_core::memory::{self, MemoryState};
use serde_json::json;
use verified_code_memory::{NewMemory, RetrievalQuery, SharedMemoryStore};

/// A folder of its own, removed when the test ends.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    static N: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "rovik-memory-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

fn paths_of(dir: &Path) -> (Paths, PathBuf) {
    let data = dir.join("data");
    (Paths::new(data.clone(), dir.join("config")), data)
}

fn project(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn the_memory_of_two_projects_is_in_two_files() {
    let dir = temp_dir();
    let (paths, data) = paths_of(&dir.0);
    let first = project(&dir.0, "first");
    let second = project(&dir.0, "second");

    let first_path = memory::store_path(&paths, &first);
    let second_path = memory::store_path(&paths, &second);
    assert_eq!(
        first_path,
        memory::store_path(&paths, &first),
        "one project, one path"
    );
    assert_ne!(first_path, second_path, "two projects don't share a file");
    assert_eq!(
        first_path.parent(),
        Some(data.join("memory").as_path()),
        "the file lives in the app's data folder, not in the project"
    );
    assert_eq!(second_path.parent(), Some(data.join("memory").as_path()));
    assert_eq!(first_path.extension().and_then(|e| e.to_str()), Some("db"));
    assert!(
        !first_path.starts_with(&first),
        "nothing is written into the user's repository"
    );

    let state = MemoryState::default();
    let one = state.open(&paths, &first).expect("the first project opens");
    let memory = one
        .remember(
            NewMemory::episode(
                "first-project",
                "Alpha panics on an empty list",
                "Guard the empty list before indexing.",
            )
            .with_files(["src/a.rs"]),
        )
        .unwrap();

    let same = state
        .open(&paths, &first)
        .expect("the cached store returns");
    let found = same
        .retrieve(&RetrievalQuery::new(
            "first-project",
            "Alpha panics on an empty list",
        ))
        .unwrap();
    assert!(
        found.iter().any(|m| m.record.id == memory.id),
        "the store opened twice for one project is the same file"
    );

    let other = state
        .open(&paths, &second)
        .expect("the second project opens");
    let elsewhere = other
        .retrieve(&RetrievalQuery::new(
            "first-project",
            "Alpha panics on an empty list",
        ))
        .unwrap();
    assert!(
        elsewhere.is_empty(),
        "one project's memory is not in another project's file"
    );
    assert_eq!(
        memory::stats(&other, "first-project").unwrap().total,
        0,
        "the second project's store counts none of the first's memories"
    );
    assert_eq!(
        memory::stats(&one, "first-project").unwrap().total,
        1,
        "the first project's store counts what was remembered in it"
    );
}

#[test]
fn a_project_that_never_chose_keeps_memory_on() {
    let dir = temp_dir();
    let config = dir.0.join("config");
    let root = project(&dir.0, "project");
    let settings_root = root.to_string_lossy().into_owned();

    assert!(
        memory::load(&config, &settings_root).enabled,
        "a project with no saved settings has memory on"
    );

    rovik_core::project_profile::save_project_settings(
        &config,
        settings_root.clone(),
        json!({ "memory": true }),
    )
    .unwrap();
    assert!(
        memory::load(&config, &settings_root).enabled,
        "a project that chose memory on keeps it on"
    );

    rovik_core::project_profile::save_project_settings(
        &config,
        settings_root.clone(),
        json!({ "memory": false }),
    )
    .unwrap();
    assert!(
        !memory::load(&config, &settings_root).enabled,
        "a project that switched memory off is off"
    );

    rovik_core::project_profile::save_project_settings(&config, settings_root.clone(), json!({}))
        .unwrap();
    assert!(
        memory::load(&config, &settings_root).enabled,
        "settings saved without the key fall back to on"
    );
}

#[test]
fn a_store_that_cannot_be_opened_yields_nothing() {
    let dir = temp_dir();
    let data = dir.0.join("data");
    std::fs::write(&data, b"a file where the data folder should be").unwrap();
    let paths = Paths::new(data, dir.0.join("config"));
    let project = project(&dir.0, "project");

    let state = MemoryState::default();
    assert!(
        state.open(&paths, &project).is_none(),
        "a store that can't be opened yields nothing instead of stopping the run"
    );
}

// Reading a project's memories back, correcting one, dropping one, clearing them.

fn entry_ids(entries: &[memory::MemoryEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.id.clone()).collect()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn an_entry_names_its_kind_and_state_as_the_chats_panel_does() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let before = now_millis();
    let episode = store
        .remember(NewMemory::episode(
            "payments-service",
            "Checkout retry race condition",
            "Serialize retries by order ID.",
        ))
        .unwrap();
    store.verify(episode.id, "12/12 tests passed").unwrap();
    let fact = store
        .remember(NewMemory::project_fact(
            "payments-service",
            "Retries run in the worker",
            "Retries are queued, not done inline.",
        ))
        .unwrap();
    let after = now_millis();

    let entries = memory::list(&store, "payments-service", "").unwrap();
    assert_eq!(entries.len(), 2, "both memories are listed");
    let verified = entries
        .iter()
        .find(|entry| entry.id == episode.id.to_string())
        .expect("the verified episode is listed");
    let provisional = entries
        .iter()
        .find(|entry| entry.id == fact.id.to_string())
        .expect("the project fact is listed");

    assert_eq!(verified.kind, "episode", "the kind reads as a word");
    assert_eq!(
        provisional.kind, "project fact",
        "the kind reads as a word"
    );
    assert_eq!(verified.verification, "verified");
    assert_eq!(provisional.verification, "provisional");
    assert_eq!(
        verified.evidence.as_deref(),
        Some("12/12 tests passed"),
        "the evidence the agent gave travels with the entry"
    );
    assert!(
        (before..=after).contains(&verified.updated_at),
        "updatedAt is milliseconds since the Unix epoch, as the protocol's stamps are: {}",
        verified.updated_at
    );
    assert!((before..=after).contains(&provisional.updated_at));

    let json = serde_json::to_value(verified).unwrap();
    assert!(
        json.get("updatedAt").is_some(),
        "the page reads camelCase keys: {json}"
    );
    assert!(json.get("updated_at").is_none());
    assert_eq!(
        json.get("verification").and_then(|value| value.as_str()),
        Some("verified")
    );
    assert_eq!(
        json.get("kind").and_then(|value| value.as_str()),
        Some("episode")
    );
}

#[test]
fn the_list_is_this_projects_memories_newest_first() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let first = store
        .remember(NewMemory::episode(
            "payments-service",
            "Serialize retries",
            "Serialize retries by order ID.",
        ))
        .unwrap();
    let second = store
        .remember(NewMemory::episode(
            "payments-service",
            "Retry tests",
            "The retries are covered twice.",
        ))
        .unwrap();
    let elsewhere = store
        .remember(NewMemory::episode(
            "billing-service",
            "Billing retry",
            "The billing worker retries at night.",
        ))
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(5));
    memory::edit(
        &store,
        &first.id.to_string(),
        "Serialize retries",
        "Serialize every retry by order ID.",
    )
    .unwrap();

    let listed = memory::list(&store, "payments-service", "").unwrap();
    assert_eq!(
        entry_ids(&listed),
        vec![first.id.to_string(), second.id.to_string()],
        "the memory edited last is the newest, so it is listed first"
    );
    assert!(
        listed[0].updated_at >= listed[1].updated_at,
        "the newest memory has the latest timestamp"
    );
    assert_eq!(listed.len(), 2, "another project's memory is left out");
    assert!(
        listed.iter().all(|entry| entry.title != "Billing retry"),
        "every listed entry belongs to the project asked about"
    );

    let other = memory::list(&store, "billing-service", "").unwrap();
    assert_eq!(entry_ids(&other), vec![elsewhere.id.to_string()]);
    assert!(
        memory::list(&store, "no-such-project", "").unwrap().is_empty(),
        "a project that remembers nothing lists nothing"
    );
}

#[test]
fn a_search_matches_part_of_a_title_the_text_or_a_file() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let retry = store
        .remember(
            NewMemory::episode(
                "payments-service",
                "Checkout retry race condition",
                "Serialize retries by order ID; the count did not fix the race.",
            )
            .with_files(["src/checkout/retry.py", "tests/test_retry.py"])
            .with_tags(["checkout"]),
        )
        .unwrap();
    let billing = store
        .remember(
            NewMemory::episode(
                "payments-service",
                "Nightly billing run",
                "The billing worker runs at night.",
            )
            .with_files(["src/billing/worker.py"])
            .with_tags(["after-hours"]),
        )
        .unwrap();

    let found = |query: &str| entry_ids(&memory::list(&store, "payments-service", query).unwrap());

    assert_eq!(
        found("retry race"),
        vec![retry.id.to_string()],
        "part of a title finds it"
    );
    assert_eq!(
        found("did not fix"),
        vec![retry.id.to_string()],
        "part of the text finds it"
    );
    assert_eq!(
        found("billing/worker.py"),
        vec![billing.id.to_string()],
        "a file the memory names finds it"
    );
    assert_eq!(
        found("after-hours"),
        vec![billing.id.to_string()],
        "a tag finds it"
    );
    assert_eq!(found("").len(), 2, "an empty search asks for every memory");
    assert!(found("nothing mentions this").is_empty());
}

#[test]
fn an_edited_memory_is_listed_with_its_new_title() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let memory = store
        .remember(NewMemory::episode(
            "payments-service",
            "Retry race",
            "Increasing the retry count did not fix it.",
        ))
        .unwrap();
    store.verify(memory.id, "12/12 tests passed").unwrap();

    memory::edit(
        &store,
        &memory.id.to_string(),
        "  Retry race, serialized  ",
        "  Serialize every retry by order ID.  ",
    )
    .unwrap();

    let listed = memory::list(&store, "payments-service", "").unwrap();
    assert_eq!(listed.len(), 1, "the same memory, corrected");
    assert_eq!(listed[0].id, memory.id.to_string());
    assert_eq!(
        listed[0].title, "Retry race, serialized",
        "the title is kept, trimmed"
    );
    assert_eq!(
        listed[0].content, "Serialize every retry by order ID.",
        "the text is kept, trimmed"
    );
    assert_eq!(
        listed[0].verification, "verified",
        "the trust state stays as the agent left it"
    );
    assert_eq!(
        entry_ids(&memory::list(&store, "payments-service", "serialized").unwrap()),
        vec![memory.id.to_string()],
        "the new title can be searched for"
    );
}

#[test]
fn an_edit_that_would_empty_the_title_or_the_text_is_refused() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let memory = store
        .remember(NewMemory::episode(
            "payments-service",
            "Retry race",
            "Increasing the retry count did not fix it.",
        ))
        .unwrap();

    assert!(
        memory::edit(&store, &memory.id.to_string(), "   ", "Some text").is_err(),
        "an edit with no title is refused"
    );
    assert!(
        memory::edit(&store, &memory.id.to_string(), "A title", "  \n ").is_err(),
        "an edit with no text is refused"
    );

    let listed = memory::list(&store, "payments-service", "").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Retry race", "the memory is unchanged");
    assert_eq!(
        listed[0].content, "Increasing the retry count did not fix it.",
        "the memory is unchanged"
    );
}

#[test]
fn a_memory_id_that_is_not_one_is_an_error() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let memory = store
        .remember(NewMemory::episode(
            "payments-service",
            "Retry race",
            "Increasing the retry count did not fix it.",
        ))
        .unwrap();

    let refused = memory::remove(&store, "not-a-uuid").unwrap_err();
    assert!(
        refused.contains("not-a-uuid"),
        "the error names the id it refused: {refused}"
    );
    let refused = memory::edit(&store, "still-not-a-uuid", "A title", "Some text").unwrap_err();
    assert!(
        refused.contains("still-not-a-uuid"),
        "the error names the id it refused: {refused}"
    );
    assert!(
        memory::remove(&store, "00000000-0000-4000-8000-000000000000").is_err(),
        "an id no memory has is an error too"
    );

    let listed = memory::list(&store, "payments-service", "").unwrap();
    assert_eq!(listed.len(), 1, "nothing was removed");
    assert_eq!(listed[0].title, memory.title, "nothing was changed");
}

#[test]
fn removing_a_memory_takes_it_out_of_the_list() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let kept = store
        .remember(NewMemory::episode(
            "payments-service",
            "Serialize retries",
            "Serialize retries by order ID.",
        ))
        .unwrap();
    let dropped = store
        .remember(NewMemory::episode(
            "payments-service",
            "Retry tests",
            "The retries are covered twice.",
        ))
        .unwrap();

    memory::remove(&store, &dropped.id.to_string()).unwrap();

    assert_eq!(
        entry_ids(&memory::list(&store, "payments-service", "").unwrap()),
        vec![kept.id.to_string()],
        "only the memory that was kept is listed"
    );
    assert!(
        memory::remove(&store, &dropped.id.to_string()).is_err(),
        "removing a memory twice is an error"
    );
}

#[test]
fn clearing_says_how_many_memories_went() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    store
        .remember(NewMemory::episode(
            "payments-service",
            "Serialize retries",
            "Serialize retries by order ID.",
        ))
        .unwrap();
    store
        .remember(NewMemory::episode(
            "payments-service",
            "Retry tests",
            "The retries are covered twice.",
        ))
        .unwrap();
    store
        .remember(NewMemory::episode(
            "billing-service",
            "Billing retry",
            "The billing worker retries at night.",
        ))
        .unwrap();

    assert_eq!(
        memory::clear(&store, "payments-service").unwrap(),
        2,
        "the count says how many memories went"
    );
    assert!(
        memory::list(&store, "payments-service", "").unwrap().is_empty(),
        "the project has no memories left"
    );
    assert_eq!(
        memory::list(&store, "billing-service", "").unwrap().len(),
        1,
        "another project's memory stays"
    );
    assert_eq!(
        memory::clear(&store, "payments-service").unwrap(),
        0,
        "clearing a project that remembers nothing clears nothing"
    );
}
