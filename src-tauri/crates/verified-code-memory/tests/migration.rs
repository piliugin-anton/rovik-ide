//! Opening a store file that an older version of this crate wrote.
//!
//! The older version kept its records in `memories` and nothing else: it had
//! no index of the files a record names and no full-text index, so no query
//! could find any of them. Opening such a file must build both indexes from
//! the records themselves, and they must keep up with the records afterwards.
//!
//! Each test writes its own store file in its own temporary directory, so the
//! tests neither see nor depend on each other's records.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;
use verified_code_memory::model::Applicability;
use verified_code_memory::{MemoryRecord, MemoryStore, NewMemory, RetrievalQuery};

const REPO: &str = "payments-service";
const ELSEWHERE: &str = "billing-service";

/// A task that shares no word and no file with any memory written below, so a
/// query that answers anything is answering on its files alone.
const UNRELATED: &str = "weather forecast for Lisbon tomorrow";

/// The retry memory, whose file path repeats no word of its text.
fn retry_memory() -> NewMemory {
    NewMemory::episode(
        REPO,
        "Retry race in the worker",
        "Serialize every retry through the retryqueue journal.",
    )
    .with_files(["src/ledger/append.py"])
}

/// A memory naming its file by the bare file name, not by a path.
fn queue_memory() -> NewMemory {
    NewMemory::procedure(
        REPO,
        "Drain the worker backlog",
        "Drain every worker backlog before the pool closes.",
    )
    .with_files(["queue.rs"])
}

fn ids(store: &MemoryStore, query: &RetrievalQuery) -> Vec<String> {
    store
        .retrieve(query)
        .expect("the query is answered")
        .into_iter()
        .map(|memory| memory.record.id.to_string())
        .collect()
}

/// One test's store file, as an older version of this crate left it: the
/// records are in `memories`, and there is neither a file index nor a
/// full-text index next to them.
///
/// The records are written through the store, so they have the shape this
/// crate writes, and then the indexes and the triggers that keep them up to
/// date are dropped — exactly what the older file was missing.
fn store_from_an_older_version(memories: Vec<NewMemory>) -> (TempDir, PathBuf, Vec<MemoryRecord>) {
    let directory = tempfile::tempdir().expect("a temporary directory of this test's own");
    let path = directory.path().join("memory.db");
    let records = {
        let store = MemoryStore::open(&path).expect("a store file is opened");
        memories
            .into_iter()
            .map(|memory| store.remember(memory).expect("the record is written"))
            .collect::<Vec<MemoryRecord>>()
    };

    let connection = Connection::open(&path).expect("the store file is opened to rewrite it");
    // `IF EXISTS` on both: dropping an index table takes its triggers with it,
    // and dropping the full-text table takes its shadow tables with it, so a
    // later drop of one of those names finds nothing left to drop.
    for table in index_tables(&path) {
        connection
            .execute_batch(&format!("DROP TABLE IF EXISTS \"{table}\""))
            .expect("an index of the file is dropped");
    }
    for (trigger, _) in names_of(&path, "trigger") {
        connection
            .execute_batch(&format!("DROP TRIGGER IF EXISTS \"{trigger}\""))
            .expect("a trigger of the file is dropped");
    }
    // Whatever migration marker this crate may write, the older file predates
    // all of them.
    connection
        .execute_batch("PRAGMA user_version = 0")
        .expect("the file holds no migration marker");
    drop(connection);

    assert_eq!(records_in(&path), records.len(), "the records are kept");
    assert!(
        index_tables(&path).is_empty(),
        "the fixture has no file index and no full-text index: {:?}",
        index_tables(&path)
    );
    assert!(
        names_of(&path, "trigger").is_empty(),
        "the fixture has no trigger that would fill an index: {:?}",
        names_of(&path, "trigger")
    );
    (directory, path, records)
}

/// The tables that index the records: the file index and the full-text index.
fn index_tables(path: &Path) -> Vec<String> {
    names_of(path, "table")
        .into_iter()
        .filter(|(name, sql)| {
            name.as_str() == "memory_files" || sql.to_ascii_uppercase().contains("VIRTUAL TABLE")
        })
        .map(|(name, _)| name)
        .collect()
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

fn records_in(path: &Path) -> usize {
    let connection = Connection::open(path).expect("the store file is opened");
    connection
        .query_row(
            "SELECT COUNT(*) FROM memories",
            rusqlite::params![],
            |row| row.get::<_, i64>(0),
        )
        .expect("the records of the file are counted") as usize
}

#[test]
fn opening_a_store_an_older_version_wrote_finds_its_record_by_the_file_it_names() {
    let (_directory, path, records) = store_from_an_older_version(vec![retry_memory()]);
    let store = MemoryStore::open(&path).expect("the older file is opened");

    for files in [
        vec!["src/ledger/append.py"],
        vec!["append.py"],
        vec!["./src/ledger/append.py"],
        vec!["src\\ledger\\append.py"],
    ] {
        let label = files.join(",");
        assert_eq!(
            ids(
                &store,
                &RetrievalQuery::new(REPO, UNRELATED).with_files(files.clone())
            ),
            vec![records[0].id.to_string()],
            "the record is found by {label:?}, although the task shares nothing with it"
        );
    }

    assert!(
        ids(
            &store,
            &RetrievalQuery::new(REPO, UNRELATED).with_files(["src/billing/invoice.py"])
        )
        .is_empty(),
        "a file no record names finds nothing"
    );

    let read = store.get(records[0].id).expect("the record is still there");
    assert_eq!(
        read.title, "Retry race in the worker",
        "the title is intact"
    );
    assert_eq!(
        read.content, "Serialize every retry through the retryqueue journal.",
        "the text of the record is intact"
    );
    assert_eq!(read.files.len(), 1, "the record still names one file");
    assert!(
        format!("{:?}", read.files).contains("src/ledger/append.py"),
        "the file it names is intact: {:?}",
        read.files
    );
}

#[test]
fn opening_a_store_an_older_version_wrote_finds_its_record_by_the_bare_file_name_it_names() {
    let (_directory, path, records) = store_from_an_older_version(vec![queue_memory()]);
    let store = MemoryStore::open(&path).expect("the older file is opened");

    assert_eq!(
        ids(
            &store,
            &RetrievalQuery::new(REPO, UNRELATED).with_files(["src/worker/queue.rs"])
        ),
        vec![records[0].id.to_string()],
        "a query path is matched against the bare file name the record holds"
    );
}

#[test]
fn opening_a_store_an_older_version_wrote_finds_its_record_by_a_word_of_it() {
    let (_directory, path, records) = store_from_an_older_version(vec![retry_memory()]);
    let store = MemoryStore::open(&path).expect("the older file is opened");

    assert_eq!(
        ids(&store, &RetrievalQuery::new(REPO, "retryqueue journal")),
        vec![records[0].id.to_string()],
        "a word only this record holds finds it"
    );

    let (found, telemetry) = store
        .retrieve_with_telemetry(&RetrievalQuery::new(REPO, UNRELATED))
        .expect("the query is answered");
    assert!(
        found.is_empty(),
        "a query sharing no word and no file with the record finds nothing: {:?}",
        found
            .iter()
            .map(|memory| memory.record.title.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(telemetry.returned, 0, "nothing was returned");
}

#[test]
fn an_edit_after_opening_keeps_a_record_the_older_version_wrote_findable() {
    let (_directory, path, records) = store_from_an_older_version(vec![retry_memory()]);
    let store = MemoryStore::open(&path).expect("the older file is opened");

    let edited = store
        .edit(
            records[0].id,
            "Retry serialization",
            "Serialize every retry through the journalqueue journal.",
        )
        .expect("the record is edited");

    assert_eq!(
        ids(
            &store,
            &RetrievalQuery::new(REPO, UNRELATED).with_files(["src/ledger/append.py"])
        ),
        vec![edited.id.to_string()],
        "the edited record is still found by the file it names"
    );
    assert_eq!(
        ids(&store, &RetrievalQuery::new(REPO, "journalqueue")),
        vec![edited.id.to_string()],
        "the edited record is found by the word the edit wrote"
    );
}

#[test]
fn deleting_a_record_the_older_version_wrote_leaves_it_unfindable() {
    let (_directory, path, records) =
        store_from_an_older_version(vec![retry_memory(), queue_memory()]);
    let store = MemoryStore::open(&path).expect("the older file is opened");

    store.delete(records[0].id).expect("the record is deleted");

    assert!(
        store.get(records[0].id).is_err(),
        "the deleted record is gone"
    );
    assert!(
        ids(
            &store,
            &RetrievalQuery::new(REPO, UNRELATED).with_files(["src/ledger/append.py"])
        )
        .is_empty(),
        "no file row of the deleted record is left behind"
    );
    assert!(
        ids(&store, &RetrievalQuery::new(REPO, "retryqueue")).is_empty(),
        "no word of the deleted record finds it"
    );
    assert_eq!(
        ids(
            &store,
            &RetrievalQuery::new(REPO, UNRELATED).with_files(["src/worker/queue.rs"])
        ),
        vec![records[1].id.to_string()],
        "the other record of the file is still found by its file"
    );
}

#[test]
fn clearing_a_project_after_opening_leaves_its_records_unfindable() {
    let (_directory, path, records) = store_from_an_older_version(vec![
        retry_memory(),
        queue_memory(),
        NewMemory::project_fact(
            ELSEWHERE,
            "Totals are in cents",
            "Sum the lines in cents before display.",
        )
        .with_files(["src/ledger/invoice.py"]),
    ]);
    let store = MemoryStore::open(&path).expect("the older file is opened");

    assert_eq!(
        store.clear(REPO).expect("the project is cleared"),
        2,
        "both records of the project went"
    );

    assert!(
        ids(
            &store,
            &RetrievalQuery::new(REPO, UNRELATED).with_files(["src/ledger/append.py"])
        )
        .is_empty(),
        "no file row of a cleared record is left behind"
    );
    assert!(
        ids(
            &store,
            &RetrievalQuery::new(REPO, UNRELATED).with_files(["src/worker/queue.rs"])
        )
        .is_empty(),
        "the second cleared record is unfindable by its file too"
    );
    assert!(
        ids(&store, &RetrievalQuery::new(REPO, "backlog")).is_empty(),
        "no word of a cleared record finds it"
    );
    assert_eq!(
        ids(
            &store,
            &RetrievalQuery::new(ELSEWHERE, UNRELATED).with_files(["src/ledger/invoice.py"])
        ),
        vec![records[2].id.to_string()],
        "another project's record is still found by its file"
    );
}

// A store file of the shape immediately before this version: its `memories`
// table has no claim hash, no applicability, no provenance and no stamp of the
// claim's text.

/// The columns this version added to `memories`, and therefore the ones the
/// file of the previous shape does not have.
const COLUMNS_THIS_VERSION_ADDED: [&str; 4] = [
    "applicability",
    "claim_hash",
    "source_kind",
    "content_updated_at",
];

/// One test's store file, as the previous shape of this crate left it: the
/// records are in `memories` without the columns this version added, and
/// without the file index, the full-text index or the triggers that keep them
/// up to date.
fn store_of_the_previous_shape(memories: Vec<NewMemory>) -> (TempDir, PathBuf, Vec<MemoryRecord>) {
    let directory = tempfile::tempdir().expect("a temporary directory of this test's own");
    let path = directory.path().join("memory.db");
    let records = {
        let store = MemoryStore::open(&path).expect("a store file is opened");
        memories
            .into_iter()
            .map(|memory| store.remember(memory).expect("the record is written"))
            .collect::<Vec<MemoryRecord>>()
    };

    let connection = Connection::open(&path).expect("the store file is opened to rewrite it");
    for table in index_tables(&path) {
        connection
            .execute_batch(&format!("DROP TABLE IF EXISTS \"{table}\""))
            .expect("an index of the file is dropped");
    }
    for (trigger, _) in names_of(&path, "trigger") {
        connection
            .execute_batch(&format!("DROP TRIGGER IF EXISTS \"{trigger}\""))
            .expect("a trigger of the file is dropped");
    }
    let dropped: Vec<String> = columns_of(&path, "memories")
        .into_iter()
        .filter(|column| COLUMNS_THIS_VERSION_ADDED.contains(&column.as_str()))
        .collect();
    // An index that mentions one of those columns would block its removal.
    for (index, sql) in names_of(&path, "index") {
        if dropped.iter().any(|column| sql.contains(column.as_str())) {
            connection
                .execute_batch(&format!("DROP INDEX IF EXISTS \"{index}\""))
                .expect("an index of the file is dropped");
        }
    }
    for column in &dropped {
        connection
            .execute_batch(&format!("ALTER TABLE memories DROP COLUMN \"{column}\""))
            .expect("a column this version added is dropped");
    }
    connection
        .execute_batch("PRAGMA user_version = 0")
        .expect("the file holds no migration marker");
    drop(connection);

    assert!(
        !dropped.is_empty(),
        "the fixture is the previous shape: it must lack some of {COLUMNS_THIS_VERSION_ADDED:?}, \
         but its memories table holds {:?}",
        columns_of(&path, "memories")
    );
    for column in COLUMNS_THIS_VERSION_ADDED {
        assert!(
            !columns_of(&path, "memories")
                .iter()
                .any(|name| name == column),
            "the previous shape has no {column:?} column"
        );
    }
    assert_eq!(records_in(&path), records.len(), "the records are kept");
    assert!(
        index_tables(&path).is_empty(),
        "the fixture has no index to find a record by: {:?}",
        index_tables(&path)
    );
    (directory, path, records)
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

#[test]
fn a_store_file_of_the_previous_shape_keeps_its_records_and_finds_them_again() {
    let (_directory, path, records) = store_of_the_previous_shape(vec![retry_memory()]);
    let store = MemoryStore::open(&path).expect("the file of the previous shape is opened");

    let read = store
        .get(records[0].id)
        .expect("the record of the previous shape is kept");
    assert_eq!(
        read.title, "Retry race in the worker",
        "the title is intact"
    );
    assert_eq!(
        read.content, "Serialize every retry through the retryqueue journal.",
        "the text is intact"
    );
    assert_eq!(
        read.applicability,
        Applicability::Unknown,
        "a record whose file said nothing about the code of today is unknown"
    );
    assert_eq!(records_in(&path), 1, "the file holds exactly its record");

    assert_eq!(
        ids(&store, &RetrievalQuery::new(REPO, "retryqueue journal")),
        vec![records[0].id.to_string()],
        "the record of the previous shape is found by a word of it"
    );
    assert_eq!(
        ids(
            &store,
            &RetrievalQuery::new(REPO, UNRELATED).with_files(["src/ledger/append.py"])
        ),
        vec![records[0].id.to_string()],
        "and by the file it names"
    );
}

#[test]
fn a_record_of_the_previous_shape_is_given_the_hash_of_its_own_words() {
    let (_directory, path, records) = store_of_the_previous_shape(vec![retry_memory()]);
    let store = MemoryStore::open(&path).expect("the file of the previous shape is opened");

    // Re-spacing the words is not a correction, which the store can only tell
    // if the record was given the hash of the title and text it holds.
    let reworded = store
        .edit(
            records[0].id,
            "Retry race in the worker",
            "Serialize   every retry\n\tthrough the retryqueue journal.",
        )
        .expect("the record is edited");

    assert_eq!(
        reworded.applicability,
        Applicability::Unknown,
        "the same words in another spacing leave the applicability alone"
    );

    // A word changed is a correction, so the claim is no longer the one of the
    // old file.
    let corrected = store
        .edit(
            records[0].id,
            "Retry race in the worker",
            "Serialize every retry through the journalqueue journal.",
        )
        .expect("the record is corrected");
    assert_eq!(
        corrected.applicability,
        Applicability::NeedsRevalidation,
        "a correction of the words asks for the claim to be checked again"
    );
    assert_eq!(
        ids(&store, &RetrievalQuery::new(REPO, "journalqueue")),
        vec![records[0].id.to_string()],
        "the corrected record is found by the word the correction wrote"
    );
}
