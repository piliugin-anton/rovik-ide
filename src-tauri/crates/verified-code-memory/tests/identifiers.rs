use verified_code_memory::{MemoryStore, NewMemory, RetrievalQuery, Verification};

#[test]
fn qualified_and_failure_symbols_find_prose_without_admitting_partial_or_generic_names() {
    let store = MemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store",
            "Reuse a locked connection.",
        ))
        .unwrap();
    for task in [
        "SharedMemoryStore",
        "sharedMemoryStore",
        "crate::SharedMemoryStore",
    ] {
        let (hits, telemetry) = store
            .retrieve_with_telemetry(&RetrievalQuery::new("a", task))
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.id, wanted.id);
        assert!(telemetry.used_full_text_search);
    }
    let hits = store
        .retrieve(&RetrievalQuery::new("a", "Fix the tests").with_failure("SharedMemoryStore"))
        .unwrap();
    assert_eq!(hits[0].record.id, wanted.id);
    for task in [
        "SharedMemoryCompressor",
        "SharedStoreExporter",
        "FixSharedMemoryStore",
        "FixTheTests",
        "SharedMemoryStore2",
    ] {
        assert!(
            store
                .retrieve(&RetrievalQuery::new("a", task))
                .unwrap()
                .is_empty(),
            "{task}"
        );
    }
}

#[test]
fn original_literal_and_file_matches_prevent_identifier_expansion() {
    let store = MemoryStore::open_in_memory().unwrap();
    let literal = store
        .remember(NewMemory::procedure(
            "a",
            "SharedMemoryStore",
            "Keep the literal symbol.",
        ))
        .unwrap();
    store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store",
            "Prose alternative.",
        ))
        .unwrap();
    let hits = store
        .retrieve(&RetrievalQuery::new("a", "SharedMemoryStore"))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.id, literal.id);
    let context = store
        .remember(
            NewMemory::procedure("a", "Other context", "File-bound observation.")
                .with_files(["src/context.rs"]),
        )
        .unwrap();
    let query = RetrievalQuery::new("a", "SharedMemoryStore").with_files(["src/context.rs"]);
    // The exact symbol still participates normally alongside an explicit file.
    assert_eq!(store.retrieve(&query).unwrap().len(), 2);
    store.delete(literal.id).unwrap();
    let hits = store.retrieve(&query).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.id, context.id);
}

#[test]
fn scope_trust_tags_and_caller_visibility_apply_before_identifier_limits() {
    let store = MemoryStore::open_in_memory().unwrap();
    let wanted = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store",
            "Visible constraint.",
        ))
        .unwrap();
    store.verify(wanted.id, "fixture").unwrap();
    for n in 0..100 {
        let hidden = store
            .remember(
                NewMemory::procedure(
                    "a",
                    format!("Shared memory store {n}"),
                    "Private constraint.",
                )
                .with_tags(["hidden"]),
            )
            .unwrap();
        store.verify(hidden.id, "fixture").unwrap();
    }
    let private = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store private",
            "Caller-private constraint.",
        ))
        .unwrap();
    store.verify(private.id, "fixture").unwrap();
    store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store provisional",
            "Unverified.",
        ))
        .unwrap();
    let obsolete = store
        .remember(NewMemory::procedure(
            "a",
            "Shared memory store obsolete",
            "Obsolete.",
        ))
        .unwrap();
    store.deprecate(obsolete.id, "replaced").unwrap();
    let foreign = store
        .remember(NewMemory::procedure(
            "b",
            "Shared memory store foreign",
            "Foreign.",
        ))
        .unwrap();
    store.verify(foreign.id, "fixture").unwrap();
    let query = RetrievalQuery::new("a", "SharedMemoryStore")
        .with_verifications([Verification::Verified])
        .without_tags(["hidden"])
        .with_limit(1);
    let (hits, _) = store
        .retrieve_filtered_with_telemetry(&query, |record| record.id != private.id)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.id, wanted.id);
}
