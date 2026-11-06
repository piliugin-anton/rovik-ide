//! The shape of the full-text query `build_fts_query` produces: every term
//! quoted, the terms joined by `OR`. The crate's own unit test pins one
//! punctuation-laden payload; these pin the rest of that shape from outside.

use verified_code_memory::retrieval::build_fts_query;

/// The parts of a query, split on the `OR` that joins its terms.
fn terms_of(query: &str) -> Vec<&str> {
    query.split(" OR ").collect()
}

#[test]
fn fts_query_joins_its_terms_with_or() {
    // FTS5's space is an implicit AND: joining with it made a multi-word task
    // miss a memory whose title holds only some of its words.
    assert_eq!(build_fts_query(&["retry race"]), "\"retry\" OR \"race\"");
    // `fix` is a word that names nothing, so it is no longer a term: the
    // inputs here are words that name something.
    assert_eq!(
        build_fts_query(&["webhook", "retry race"]),
        "\"webhook\" OR \"retry\" OR \"race\"",
        "several inputs are one list of OR-ed terms"
    );
}

#[test]
fn an_fts_query_of_any_text_stays_quoted_terms() {
    // A payload that closes the quote and opens an operator of its own is
    // still only terms, each one quoted: what the user typed is searched for,
    // not obeyed.
    let smuggled = build_fts_query(&["alpha\" OR \"beta"]);
    for term in terms_of(&smuggled) {
        assert!(
            term.starts_with('"') && term.ends_with('"') && term.len() > 2,
            "{term:?} is a quoted term in {smuggled:?}"
        );
    }
    assert!(
        smuggled.starts_with("\"alpha\"") && smuggled.ends_with("\"beta\""),
        "{smuggled:?}"
    );

    // A word that merely looks like an operator keeps its place as a term:
    assert_eq!(build_fts_query(&["order"]), "\"order\"");
}
