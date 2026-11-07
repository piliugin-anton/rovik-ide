//! The identity of a memory's claim: which words, and which field they are in.
//!
//! A memory remembers the hash of the text its evidence was about, so a later
//! edit can tell that the words changed — and an edit cannot keep the trust
//! earned by different wording. Whitespace is not part of the identity: extra
//! spaces, leading or trailing spaces and re-wrapping the words are the same
//! claim. The boundary between the title and the content is part of it: moving
//! a word from the title into the body is a different claim, and so is a swap
//! of the two fields.

use verified_code_memory::model::{claim_hash, Applicability, Verification};
use verified_code_memory::{MemoryStore, NewMemory};

const REPO: &str = "payments-service";

#[test]
fn moving_a_word_across_the_title_boundary_is_a_different_claim() {
    let claim = claim_hash("Retry race", "seed the queue");
    assert_ne!(
        claim,
        claim_hash("Retry", "race seed the queue"),
        "a word moved from the title into the body is a different claim"
    );
    assert_ne!(
        claim,
        claim_hash("", "Retry race seed the queue"),
        "the same words under an empty title are a different claim"
    );
    assert_ne!(
        claim,
        claim_hash("Retry race seed the queue", ""),
        "the same words with an empty body are a different claim"
    );
    assert_ne!(
        claim_hash("", "Retry race seed the queue"),
        claim_hash("Retry race seed the queue", ""),
        "which of the two fields holds the words is part of the claim"
    );
    assert_eq!(
        claim,
        claim_hash("Retry race", "seed the queue"),
        "the same title and the same body is the same claim"
    );
}

#[test]
fn swapping_the_title_and_the_body_is_a_different_claim() {
    assert_ne!(
        claim_hash("a b", "c d"),
        claim_hash("c d", "a b"),
        "the title and the body are not one run of words"
    );
}

#[test]
fn whitespace_is_ignored_within_each_field() {
    assert_eq!(
        claim_hash("  Retry   race ", "seed\nthe  queue"),
        claim_hash("Retry race", "seed the queue"),
        "re-spacing either field's words is not a different claim"
    );
    assert_eq!(
        claim_hash("Retry race", "seed\nthe\tqueue"),
        claim_hash("Retry race", "seed the queue"),
        "re-wrapped body words are the same claim"
    );
    assert_eq!(
        claim_hash("Retry race", " \n\t "),
        claim_hash("Retry race", ""),
        "a body of whitespace only is the same claim as an empty body"
    );
    assert_ne!(
        claim_hash("Retry race", "seed the queue"),
        claim_hash("Retry race", "seed the queues"),
        "a different word is a different claim"
    );
}

#[test]
fn an_edit_that_moves_words_from_the_title_into_the_body_keeps_the_trust_but_needs_revalidation() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store
        .remember(NewMemory::episode(
            REPO,
            "Checkout retry race condition",
            "Serialize retries by order ID.",
        ))
        .unwrap();
    store.verify(memory.id, "12/12 tests passed").unwrap();
    let verified = store.get(memory.id).unwrap();
    assert!(
        verified.is_reliable(),
        "a verified, current memory with matching evidence is reliable"
    );
    assert_eq!(
        verified.applicability,
        Applicability::Current,
        "nothing has changed yet"
    );

    // The same words in the same order, but "race condition" has moved from the
    // title into the body. The evidence was about the old grouping of words.
    store
        .edit(
            memory.id,
            "Checkout retry",
            "race condition Serialize retries by order ID.",
        )
        .unwrap();

    let stored = store.get(memory.id).unwrap();
    assert_eq!(
        stored.verification,
        Verification::Verified,
        "what the evidence once supported is not forgotten"
    );
    assert_eq!(
        stored.evidence.as_deref(),
        Some("12/12 tests passed"),
        "the evidence is kept for audit"
    );
    assert_eq!(
        stored.applicability,
        Applicability::NeedsRevalidation,
        "words that changed field are a changed claim, so it must be checked again"
    );
    assert!(
        !stored.evidence_matches_claim(),
        "the evidence speaks about the old grouping of the words"
    );
    assert!(
        !stored.is_reliable(),
        "a verified claim whose evidence is about other words is not reliable"
    );
}

#[test]
fn an_edit_that_only_respaces_the_words_leaves_the_claim_unchanged() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store
        .remember(NewMemory::episode(
            REPO,
            "Checkout retry race condition",
            "Serialize retries by order ID.",
        ))
        .unwrap();
    store.verify(memory.id, "12/12 tests passed").unwrap();
    let before = store.get(memory.id).unwrap();
    assert_eq!(before.applicability, Applicability::Current);

    store
        .edit(
            memory.id,
            "  Checkout   retry race\ncondition  ",
            "Serialize retries\n\tby order ID.",
        )
        .unwrap();

    let after = store.get(memory.id).unwrap();
    assert_eq!(
        after.applicability, before.applicability,
        "the words did not change, so nothing has to be checked again"
    );
    assert_eq!(
        after.verification,
        Verification::Verified,
        "the trust state is not disturbed either"
    );
    assert!(
        after.evidence_matches_claim(),
        "the evidence still speaks about the same words"
    );
    assert!(after.is_reliable(), "nothing about the claim changed");
}
