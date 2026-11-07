//! Consolidation: the same thing said twice is one record, revised.
//!
//! A record carries an identity — what it is within its project — so a later
//! record of the same identity revises the older one instead of sitting beside
//! it as a second opinion. The store offers the pieces an agent needs to do
//! that safely: an exact lookup of the records carrying an identity, a
//! supersession that deprecates the older ones in one transaction, a
//! generation that moves whenever a project's memory is cut down (so a write
//! planned before the cut is refused instead of resurrecting what was cut),
//! and a bound on how many episodes a project keeps.
//!
//! A record whose identity is empty was written before identities were kept:
//! it is the same thing as nothing else, and no identity lookup answers with
//! it.
//!
//! Each test that needs a store file writes its own in its own temporary
//! directory, so the tests neither see nor depend on each other's records.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;
use tempfile::TempDir;
use verified_code_memory::error::VcmError;
use verified_code_memory::model::{Applicability, Verification};
use verified_code_memory::{MemoryRecord, MemoryStore, NewMemory, SharedMemoryStore};

const REPO: &str = "payments-service";
const ELSEWHERE: &str = "billing-service";
const IDENTITY: &str = "checkout/retry";
const HMAC: &str = "checkout/hmac";

/// A pause long enough that a stamp written afterwards is a later one.
fn tick() {
    std::thread::sleep(Duration::from_millis(1100));
}

fn ids(memories: &[MemoryRecord]) -> Vec<String> {
    memories.iter().map(|memory| memory.id.to_string()).collect()
}

/// An episode of the project, saying nothing in particular.
fn episode(title: &str) -> NewMemory {
    NewMemory::episode(REPO, title, format!("{title}, written down."))
}

/// An episode of the project that says what it is, so a later record of the
/// same identity is the same thing said again.
fn identified(title: &str, identity: &str) -> NewMemory {
    episode(title).with_identity(identity)
}

/// What a project does, not what happened to it once.
fn procedure(title: &str) -> NewMemory {
    NewMemory::procedure(REPO, title, format!("{title}, in that order."))
}

fn project_fact(title: &str) -> NewMemory {
    NewMemory::project_fact(REPO, title, format!("{title}; it is a rule."))
}

/// How the store spells its refusal. The contract names the kinds of refusal
/// — a field that may not be empty, a record that is not there — but not the
/// shape of their fields, so the variant's debug form is what the tests read.
fn refusal(error: &VcmError) -> String {
    format!("{error:?}")
}

#[test]
fn two_records_of_different_identities_are_never_each_others_duplicate() {
    let store = MemoryStore::open_in_memory().unwrap();
    let retry = store.remember(identified("Serialize the retries", IDENTITY)).unwrap();
    let hmac = store.remember(identified("Check the signature", HMAC)).unwrap();

    assert_ne!(
        retry.identity, hmac.identity,
        "the two records say two different things"
    );
    assert_eq!(
        ids(&store.find_by_identity(REPO, IDENTITY).unwrap()),
        vec![retry.id.to_string()],
        "a lookup for the first identity answers with the first record alone"
    );
    assert_eq!(
        ids(&store.find_by_identity(REPO, HMAC).unwrap()),
        vec![hmac.id.to_string()],
        "a lookup for the second identity answers with the second record alone"
    );
    assert_eq!(
        store.stats(REPO).unwrap().total,
        2,
        "both records are kept: neither was taken for the other's older copy"
    );
    assert_eq!(
        store.get(hmac.id).unwrap().verification,
        Verification::Provisional,
        "and neither was deprecated"
    );
}

#[test]
fn an_identity_lookup_answers_with_the_records_of_that_identity_newest_first_and_no_others() {
    let store = MemoryStore::open_in_memory().unwrap();
    let older = store.remember(identified("Retry with a counter", IDENTITY)).unwrap();
    tick();
    let newer = store.remember(identified("Retry through the queue", IDENTITY)).unwrap();
    let other_identity = store.remember(identified("Check the signature", HMAC)).unwrap();
    let other_project = store
        .remember(
            NewMemory::procedure(ELSEWHERE, "Serialize the retries", "Drain, then retry.")
                .with_identity(IDENTITY),
        )
        .unwrap();

    assert_eq!(
        ids(&store.find_by_identity(REPO, IDENTITY).unwrap()),
        vec![newer.id.to_string(), older.id.to_string()],
        "the two records of that identity, newest first, and no record of another identity \
         ({}) and no record of another project ({})",
        other_identity.id,
        other_project.id
    );
}

#[test]
fn an_identity_lookup_without_a_project_or_without_an_identity_is_refused() {
    let store = MemoryStore::open_in_memory().unwrap();
    store.remember(identified("Serialize the retries", IDENTITY)).unwrap();

    let without_project = store.find_by_identity("", IDENTITY).unwrap_err();
    assert!(
        refusal(&without_project).contains("EmptyField"),
        "a lookup with no project is an empty field: {}",
        refusal(&without_project)
    );

    let without_identity = store.find_by_identity(REPO, "").unwrap_err();
    assert!(
        refusal(&without_identity).contains("EmptyField"),
        "a lookup with no identity is an empty field: {}",
        refusal(&without_identity)
    );
}

#[test]
fn superseding_deprecates_only_the_ids_given_and_says_how_many_it_deprecated() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kept = store.remember(identified("Retry through the queue", IDENTITY)).unwrap();
    store.verify(kept.id, "cargo test -p checkout passed").unwrap();
    let first = store.remember(identified("Retry with a counter", IDENTITY)).unwrap();
    let second = store.remember(identified("Retry with a sleep", IDENTITY)).unwrap();
    let not_given = store.remember(identified("Retry in the journal", IDENTITY)).unwrap();

    assert_eq!(
        store
            .supersede(kept.id, &[first.id, second.id], "the queue replaced both")
            .unwrap(),
        2,
        "both older records were deprecated by the one call"
    );

    assert_eq!(
        store.get(first.id).unwrap().verification,
        Verification::Deprecated,
        "the first older record is deprecated"
    );
    assert_eq!(
        store.get(second.id).unwrap().verification,
        Verification::Deprecated,
        "the second older record is deprecated"
    );
    assert_eq!(
        store.get(not_given.id).unwrap().verification,
        Verification::Provisional,
        "a record of the same identity that was not given is left alone"
    );

    let after = store.get(kept.id).unwrap();
    assert_eq!(
        after.verification,
        Verification::Verified,
        "the kept record's verification is untouched"
    );
    assert_eq!(
        after.evidence.as_deref(),
        Some("cargo test -p checkout passed"),
        "and so is the evidence it was verified by"
    );
    assert_eq!(
        after.applicability,
        Applicability::Current,
        "and so is what the record says about the code of today"
    );

    assert_eq!(
        store
            .supersede(kept.id, &[first.id, second.id], "the queue replaced them")
            .unwrap(),
        0,
        "a record already deprecated is not counted again"
    );
    assert_eq!(
        store.get(first.id).unwrap().verification,
        Verification::Deprecated,
        "and stays deprecated"
    );
}

#[test]
fn a_supersession_that_is_refused_deprecates_nothing() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kept = store.remember(identified("Retry through the queue", IDENTITY)).unwrap();
    let older = store.remember(identified("Retry with a counter", IDENTITY)).unwrap();

    // A record that is no longer in the store: its id names nothing.
    let gone = store.remember(identified("Retry in the router", "checkout/router")).unwrap();
    store.delete(gone.id).unwrap();

    let unknown_kept = store
        .supersede(gone.id, &[older.id], "the queue replaced it")
        .unwrap_err();
    assert!(
        refusal(&unknown_kept).contains("NotFound"),
        "superseding onto a record that is not there is not found: {}",
        refusal(&unknown_kept)
    );
    assert_eq!(
        store.get(older.id).unwrap().verification,
        Verification::Provisional,
        "the refused call deprecated nothing"
    );

    let empty_reason = store.supersede(kept.id, &[older.id], "   ").unwrap_err();
    assert!(
        refusal(&empty_reason).contains("EmptyField"),
        "a supersession with no reason is an empty field: {}",
        refusal(&empty_reason)
    );
    assert_eq!(
        store.get(older.id).unwrap().verification,
        Verification::Provisional,
        "and it deprecates nothing either"
    );
    assert_eq!(
        store.stats(REPO).unwrap().deprecated,
        0,
        "no record of the project was deprecated by either refused call"
    );
}

#[test]
fn superseding_does_not_touch_another_projects_record_of_the_same_identity() {
    let store = MemoryStore::open_in_memory().unwrap();
    let kept = store.remember(identified("Retry through the queue", IDENTITY)).unwrap();
    let older = store.remember(identified("Retry with a counter", IDENTITY)).unwrap();
    let other_project = store
        .remember(
            NewMemory::procedure(ELSEWHERE, "Serialize the retries", "Drain, then retry.")
                .with_identity(IDENTITY),
        )
        .unwrap();

    assert_eq!(
        store
            .supersede(kept.id, &[older.id], "the queue replaced the counter")
            .unwrap(),
        1,
        "one record of this project was deprecated"
    );
    assert_eq!(
        store.get(older.id).unwrap().verification,
        Verification::Deprecated,
        "the older record of this project is deprecated"
    );
    assert_eq!(
        store.get(other_project.id).unwrap().verification,
        Verification::Provisional,
        "the same identity in another project is another record"
    );
    assert_eq!(
        ids(&store.find_by_identity(ELSEWHERE, IDENTITY).unwrap()),
        vec![other_project.id.to_string()],
        "and it still carries its identity"
    );
}

#[test]
fn a_projects_generation_starts_at_zero_and_moves_only_when_its_memory_is_cut_down() {
    let store = MemoryStore::open_in_memory().unwrap();
    assert_eq!(
        store.generation(REPO).unwrap(),
        0,
        "a project nothing was ever cut from is at generation 0"
    );

    let memory = store.remember(episode("Retry one")).unwrap();
    assert_eq!(
        store.generation(REPO).unwrap(),
        0,
        "writing a record cuts nothing down"
    );

    store.delete(memory.id).unwrap();
    let after_delete = store.generation(REPO).unwrap();
    assert!(
        after_delete > 0,
        "deleting a record moves the project's generation: {after_delete}"
    );
    assert_eq!(
        store.generation(ELSEWHERE).unwrap(),
        0,
        "another project's generation is its own"
    );

    store.remember(episode("Retry two")).unwrap();
    store.clear(REPO).unwrap();
    let after_clear = store.generation(REPO).unwrap();
    assert!(
        after_clear > after_delete,
        "clearing the project moves its generation again: {after_clear} after {after_delete}"
    );
}

#[test]
fn a_write_with_the_current_generation_is_kept_and_one_with_a_stale_generation_is_refused() {
    let store = MemoryStore::open_in_memory().unwrap();
    let current = store.generation(REPO).unwrap();

    let written = store
        .remember_if_current(identified("Retry through the queue", IDENTITY), current)
        .unwrap()
        .expect("the generation still holds, so the record is written");
    assert_eq!(
        store.get(written.id).unwrap().identity,
        IDENTITY,
        "the written record carries the identity it was given"
    );
    assert_eq!(
        ids(&store.find_by_identity(REPO, IDENTITY).unwrap()),
        vec![written.id.to_string()],
        "and an identity lookup finds it"
    );

    // The project's memory is cut down, so a write planned before the cut is
    // planned against a store that is no longer there.
    let cut = store.remember(episode("Retry with a counter")).unwrap();
    store.delete(cut.id).unwrap();
    assert!(
        store.generation(REPO).unwrap() > current,
        "the cut moved the generation"
    );

    let total_before = store.stats(REPO).unwrap().total;
    let refused = store
        .remember_if_current(identified("Retry in the router", "checkout/router"), current)
        .unwrap();
    assert!(
        refused.is_none(),
        "a stale generation refuses the write: {refused:?}"
    );
    assert_eq!(
        store.stats(REPO).unwrap().total,
        total_before,
        "nothing was written"
    );
    assert!(
        store.find_by_identity(REPO, "checkout/router").unwrap().is_empty(),
        "the refused record left nothing behind to be found by its identity"
    );
}

#[test]
fn a_verification_with_the_current_generation_is_kept_and_one_with_a_stale_generation_is_refused() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store.remember(identified("Retry through the queue", IDENTITY)).unwrap();
    let current = store.generation(REPO).unwrap();

    let verified = store
        .verify_if_current(memory.id, "12/12 tests passed", current)
        .unwrap()
        .expect("the generation still holds, so the evidence is recorded");
    assert_eq!(
        verified.verification,
        Verification::Verified,
        "the record is verified by the evidence"
    );
    assert_eq!(
        verified.evidence.as_deref(),
        Some("12/12 tests passed"),
        "and the evidence is kept"
    );

    let cut = store.remember(episode("Retry with a counter")).unwrap();
    store.delete(cut.id).unwrap();

    let refused = store
        .verify_if_current(memory.id, "a later run said so", current)
        .unwrap();
    assert!(
        refused.is_none(),
        "a stale generation refuses the verification: {refused:?}"
    );

    let after = store.get(memory.id).unwrap();
    assert_eq!(
        after.evidence.as_deref(),
        Some("12/12 tests passed"),
        "the evidence that was there is still the evidence"
    );
    assert_eq!(
        after.verification,
        Verification::Verified,
        "and the refused verification changed nothing"
    );
}

#[test]
fn pruning_keeps_the_newest_episodes_and_never_procedures_or_project_facts() {
    let store = MemoryStore::open_in_memory().unwrap();
    let fact = store.remember(project_fact("Totals are in cents")).unwrap();
    let releasing = store.remember(procedure("Release the app")).unwrap();
    tick();
    let oldest = store.remember(episode("Retry one")).unwrap();
    tick();
    let second = store.remember(episode("Retry two")).unwrap();
    tick();
    let third = store.remember(episode("Retry three")).unwrap();
    tick();
    let newest = store.remember(episode("Retry four")).unwrap();

    assert_eq!(
        store.prune_episodes(REPO, 2).unwrap(),
        2,
        "the two episodes older than the newest two went"
    );

    for pruned in [&oldest, &second] {
        assert!(
            store.get(pruned.id).is_err(),
            "the older episode {:?} was pruned",
            pruned.title
        );
    }
    for kept in [&third, &newest] {
        let read = store.get(kept.id).expect("the newer episode is kept");
        assert_eq!(read.title, kept.title, "the newest two episodes stay");
    }
    for survivor in [&fact, &releasing] {
        let read = store
            .get(survivor.id)
            .expect("a record that is not an episode is never pruned");
        assert_eq!(
            read.title, survivor.title,
            "a procedure and a project fact are never pruned"
        );
    }

    assert_eq!(
        store.prune_episodes(REPO, 2).unwrap(),
        0,
        "the newest two are the only episodes left, so nothing goes"
    );
    assert_eq!(
        store.prune_episodes(REPO, 10).unwrap(),
        0,
        "a bound no episode count exceeds deletes nothing"
    );
    assert_eq!(
        store.stats(REPO).unwrap().total,
        4,
        "the fact, the procedure and the two newest episodes are left"
    );
}

#[test]
fn pruning_one_projects_episodes_leaves_another_projects_alone() {
    let store = MemoryStore::open_in_memory().unwrap();
    let other_project = store
        .remember(
            NewMemory::episode(ELSEWHERE, "Totals were doubled", "Sum the lines in cents.")
                .with_files(["src/ledger/total.py"]),
        )
        .unwrap();
    let older = store.remember(episode("Retry one")).unwrap();
    tick();
    let newer = store.remember(episode("Retry two")).unwrap();

    assert_eq!(
        store.prune_episodes(REPO, 1).unwrap(),
        1,
        "only this project's older episode went"
    );
    assert!(
        store.get(older.id).is_err(),
        "the older episode of the project is pruned"
    );
    assert!(
        store.get(newer.id).is_ok(),
        "the newest episode of the project is kept"
    );
    assert!(
        store.get(other_project.id).is_ok(),
        "another project's episode is untouched, however old it is"
    );
}

#[test]
fn a_record_written_without_an_identity_is_found_by_no_identity_lookup() {
    let store = MemoryStore::open_in_memory().unwrap();
    let anonymous = store.remember(episode("Retry one")).unwrap();
    let anonymous_too = store.remember(episode("Retry two")).unwrap();

    assert!(
        anonymous.identity.is_empty(),
        "a record written without an identity carries none: {:?}",
        anonymous.identity
    );
    assert!(
        store.find_by_identity(REPO, "").is_err(),
        "an empty identity is refused, so no lookup can answer with such a record"
    );

    let named = store.remember(identified("Retry through the queue", IDENTITY)).unwrap();
    assert_eq!(
        ids(&store.find_by_identity(REPO, IDENTITY).unwrap()),
        vec![named.id.to_string()],
        "the records without an identity are not duplicates of the one with it \
         ({}, {})",
        anonymous.id,
        anonymous_too.id
    );
    assert_eq!(
        store.stats(REPO).unwrap().total,
        3,
        "all three records are kept: none was taken for another's older copy"
    );
}

/// One test's store file, as the version before identities left it: the
/// records are in `memories`, and no column of that table could hold an
/// identity.
///
/// The records are written through the store, so they have the shape this
/// crate writes, and then every index, trigger and column that speaks of an
/// identity is dropped — exactly what the older file was missing.
fn store_before_identities(memories: Vec<NewMemory>) -> (TempDir, PathBuf, Vec<MemoryRecord>) {
    let directory = tempfile::tempdir().expect("a temporary directory of this test's own");
    let path = directory.path().join("memory.db");
    let records = {
        let store = MemoryStore::open(&path).expect("a store file is opened");
        memories
            .into_iter()
            .map(|memory| store.remember(memory).expect("the record is written"))
            .collect::<Vec<MemoryRecord>>()
    };

    let identity_columns: Vec<String> = columns_of(&path, "memories")
        .into_iter()
        .filter(|column| column.to_ascii_lowercase().contains("identity"))
        .collect();
    assert!(
        !identity_columns.is_empty(),
        "this version keeps an identity in memories, so the file of the previous shape is made \
         by dropping that column; its columns are {:?}",
        columns_of(&path, "memories")
    );

    let connection = Connection::open(&path).expect("the store file is opened to rewrite it");
    // An index or a trigger that mentions the column would block its removal.
    for kind in ["index", "trigger"] {
        let keyword = if kind == "index" { "INDEX" } else { "TRIGGER" };
        for (name, sql) in names_of(&path, kind) {
            let mentions = identity_columns.iter().any(|column| sql.contains(column.as_str()));
            if mentions {
                connection
                    .execute_batch(&format!("DROP {keyword} IF EXISTS \"{name}\""))
                    .expect("an index or trigger of the file is dropped");
            }
        }
    }
    for column in &identity_columns {
        connection
            .execute_batch(&format!("ALTER TABLE memories DROP COLUMN \"{column}\""))
            .expect("the column holding an identity is dropped");
    }
    // Whatever migration marker this crate may write, the older file predates
    // all of them.
    connection
        .execute_batch("PRAGMA user_version = 0")
        .expect("the file holds no migration marker");
    drop(connection);

    assert!(
        !columns_of(&path, "memories")
            .iter()
            .any(|column| column.to_ascii_lowercase().contains("identity")),
        "the fixture has no column for an identity: {:?}",
        columns_of(&path, "memories")
    );
    assert_eq!(records_in(&path), records.len(), "the records are kept");
    (directory, path, records)
}

/// The `name` and SQL of every object of `kind` in the store file.
fn names_of(path: &Path, kind: &str) -> Vec<(String, String)> {
    let connection = Connection::open(path).expect("the store file is opened");
    let mut statement = connection
        .prepare("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = ?1 ORDER BY name")
        .expect("sqlite_master is readable");
    statement
        .query_map(rusqlite::params![kind], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("the objects of the file are listed")
        .map(|row| row.expect("an object of the file"))
        .collect()
}

/// The column names of a table of the store file.
fn columns_of(path: &Path, table: &str) -> Vec<String> {
    let connection = Connection::open(path).expect("the store file is opened");
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .expect("the table is described");
    statement
        .query_map(rusqlite::params![], |row| row.get::<_, String>(1))
        .expect("the columns of the table are listed")
        .map(|row| row.expect("a column of the table"))
        .collect()
}

fn records_in(path: &Path) -> usize {
    let connection = Connection::open(path).expect("the store file is opened");
    connection
        .query_row("SELECT COUNT(*) FROM memories", rusqlite::params![], |row| {
            row.get::<_, i64>(0)
        })
        .expect("the records of the file are counted") as usize
}

#[test]
fn a_store_file_of_the_previous_shape_opens_keeps_its_records_and_finds_by_identity_afterwards() {
    let (_directory, path, records) = store_before_identities(vec![
        identified("Serialize the retries", IDENTITY),
        procedure("Release the app"),
    ]);
    let store = MemoryStore::open(&path).expect("the file of the previous shape is opened");

    let kept = store
        .get(records[0].id)
        .expect("the record of the previous shape is kept");
    assert_eq!(kept.title, "Serialize the retries", "the title is intact");
    assert!(
        kept.identity.is_empty(),
        "a record of that file has no identity: {:?}",
        kept.identity
    );
    assert!(
        store.get(records[1].id).is_ok(),
        "the other record of the file is kept too"
    );
    assert!(
        store.find_by_identity(REPO, IDENTITY).unwrap().is_empty(),
        "no identity lookup answers with a record that has no identity"
    );

    let written = store
        .remember(identified("Retry through the queue", IDENTITY))
        .expect("a record is written to the file of the previous shape");
    assert_eq!(
        ids(&store.find_by_identity(REPO, IDENTITY).unwrap()),
        vec![written.id.to_string()],
        "an identity lookup works on the file afterwards, and finds only the record written \
         with that identity"
    );

    let revising = store
        .remember(identified("Retry through the journal", IDENTITY))
        .expect("a later record is written to the file");
    assert_eq!(
        store
            .supersede(revising.id, &[written.id], "the journal replaced the queue")
            .expect("a supersession works on the file afterwards"),
        1,
        "one record of the project was deprecated"
    );
    assert_eq!(
        store.get(written.id).unwrap().verification,
        Verification::Deprecated,
        "the revised record is deprecated"
    );
    assert_eq!(
        store.get(revising.id).unwrap().verification,
        Verification::Provisional,
        "the revising record is left as the claim it is"
    );
    assert_eq!(records_in(&path), 4, "all four records are in the file");
}

#[test]
fn the_shared_store_consolidates_through_its_lock() {
    let store = SharedMemoryStore::open_in_memory().unwrap();
    let kept = store.remember(identified("Retry through the queue", IDENTITY)).unwrap();
    let older = store.remember(identified("Retry with a counter", IDENTITY)).unwrap();

    let mut found = ids(&store.find_by_identity(REPO, IDENTITY).unwrap());
    found.sort();
    let mut expected = vec![kept.id.to_string(), older.id.to_string()];
    expected.sort();
    assert_eq!(
        found, expected,
        "the shared store looks a project's records up by their identity"
    );

    let generation = store.generation(REPO).unwrap();
    let routed = || identified("Retry in the router", "checkout/router");
    assert!(
        store
            .remember_if_current(routed(), generation - 1)
            .unwrap()
            .is_none(),
        "the shared store refuses a write planned before its project was cut down"
    );
    assert_eq!(
        store
            .remember_if_current(routed(), generation)
            .unwrap()
            .map(|record| record.identity),
        Some("checkout/router".to_string()),
        "and keeps one planned at the generation the project is at"
    );

    assert_eq!(
        store
            .supersede(kept.id, &[older.id], "the queue replaced the counter")
            .unwrap(),
        1,
        "the shared store supersedes through its lock"
    );
    assert_eq!(
        store.get(older.id).unwrap().verification,
        Verification::Deprecated,
        "the older record is deprecated"
    );
    assert_eq!(
        store.prune_episodes(REPO, 10).unwrap(),
        0,
        "and bounds the episodes it keeps"
    );
}
