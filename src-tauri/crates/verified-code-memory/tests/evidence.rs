//! The evidence and applicability contract of a memory.
//!
//! A record says two different things: what evidence once supported its claim
//! (`verification`), and whether the claim is expected to hold for the code
//! now (`applicability`). Neither is derived from the other, and neither is
//! derived from how often the memory was recalled or how often the checks of a
//! run passed. Evidence speaks about the words it was given: when an edit
//! changes those words the evidence no longer speaks about the claim, though
//! nothing is forgotten. Only a new commit is not a contradiction: it asks for
//! the claim to be checked again, it never deprecates the memory.

use std::time::Duration;

use verified_code_memory::model::{claim_hash, Applicability, SourceKind, Verification};
use verified_code_memory::{MemoryStore, NewMemory, RetrievalQuery};

const REPO: &str = "payments-service";

/// A pause long enough that a stamp written afterwards is a later one.
fn tick() {
    std::thread::sleep(Duration::from_millis(1100));
}

fn retry_memory() -> NewMemory {
    NewMemory::episode(
        REPO,
        "Checkout retry race condition",
        "Serialize retries by order ID.",
    )
    .with_files(["src/checkout/retry.py"])
    .with_tags(["checkout", "retry"])
}

#[test]
fn a_green_general_check_cannot_promote_a_memory_but_claim_evidence_can() {
    let store = MemoryStore::open_in_memory().unwrap();
    // The strongest thing a passing run of the checks produces: a record whose
    // source is a test result. It is still a provisional claim about the code.
    let memory = store
        .remember(retry_memory().with_source_kind(SourceKind::TestResult))
        .unwrap();
    assert_eq!(
        memory.verification,
        Verification::Provisional,
        "a new record is a provisional claim"
    );
    assert_eq!(
        memory.applicability,
        Applicability::Unknown,
        "nothing is known yet about the code it speaks about"
    );
    assert_eq!(
        memory.source_kind,
        SourceKind::TestResult,
        "the provenance the record was given stays"
    );
    assert!(
        !memory.evidence_matches_claim(),
        "no evidence was given about this claim"
    );
    assert!(!memory.is_reliable(), "nothing was verified");

    // Everything a run of the checks can do besides speaking about the claim.
    store.record_outcome(memory.id, true).unwrap();
    store.record_retrieval(&[memory.id]).unwrap();
    store.mark_stale_for_version(REPO, "def456").unwrap();

    let after = store.get(memory.id).unwrap();
    assert_eq!(
        after.verification,
        Verification::Provisional,
        "a green check is not evidence about this claim: only `verify` promotes"
    );
    assert!(
        !after.is_reliable(),
        "a record nothing was verified about is not reliable"
    );

    // Evidence about the claim itself.
    let verified = store
        .verify(memory.id, "12/12 tests in tests/test_retry.py passed")
        .unwrap();
    assert_eq!(verified.verification, Verification::Verified);
    assert_eq!(verified.applicability, Applicability::Current);
    assert!(
        verified.evidence_matches_claim(),
        "the evidence speaks about the claim as it stands"
    );
    assert!(
        verified.is_reliable(),
        "verified, current and matching evidence is reliable"
    );
}

#[test]
fn an_edit_after_verification_keeps_the_trust_but_the_evidence_stops_speaking_about_the_claim() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store.remember(retry_memory()).unwrap();
    store.verify(memory.id, "12/12 tests passed").unwrap();
    assert!(
        store.get(memory.id).unwrap().is_reliable(),
        "the verified memory is reliable before the edit"
    );

    let edited = store
        .edit(
            memory.id,
            "Checkout retry serialization",
            "Serialize every retry by order ID through the retryqueue.",
        )
        .unwrap();

    assert_eq!(
        edited.verification,
        Verification::Verified,
        "what the evidence once supported is not forgotten"
    );
    assert_eq!(
        edited.evidence.as_deref(),
        Some("12/12 tests passed"),
        "the evidence is kept for audit"
    );
    assert_eq!(
        edited.applicability,
        Applicability::NeedsRevalidation,
        "the corrected claim must be checked again"
    );
    assert!(
        !edited.evidence_matches_claim(),
        "the evidence speaks about the old words, not the new ones"
    );
    assert!(
        !edited.is_reliable(),
        "a verified claim whose evidence is about other words is not reliable"
    );

    let stored = store.get(memory.id).unwrap();
    assert_eq!(
        stored.title, "Checkout retry serialization",
        "the correction is what the memory now says"
    );
    assert_eq!(
        stored.content, "Serialize every retry by order ID through the retryqueue.",
        "the correction is what the memory now says"
    );
    assert!(
        !stored.evidence_matches_claim(),
        "the stored record carries the same broken evidence link"
    );
}

#[test]
fn an_edit_is_the_users_assertion_and_moves_the_stamp_of_the_claim_text() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store
        .remember(retry_memory().with_source_kind(SourceKind::CodeObservation))
        .unwrap();
    let before = store.get(memory.id).unwrap();
    tick();

    let edited = store
        .edit(
            memory.id,
            "Checkout retry serialization",
            "Serialize every retry by order ID.",
        )
        .unwrap();

    assert_eq!(
        edited.source_kind,
        SourceKind::UserAssertion,
        "the words are now the user's assertion"
    );
    assert!(
        edited.content_updated_at > before.content_updated_at,
        "the claim text was corrected, so its stamp moves: {:?} then {:?}",
        before.content_updated_at,
        edited.content_updated_at
    );
}

#[test]
fn an_edit_that_only_rewraps_the_words_leaves_the_applicability_alone() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store.remember(retry_memory()).unwrap();
    store.verify(memory.id, "12/12 tests passed").unwrap();

    let edited = store
        .edit(
            memory.id,
            "  Checkout   retry race\ncondition  ",
            "Serialize retries\n\tby order ID.",
        )
        .unwrap();

    assert_eq!(
        edited.applicability,
        Applicability::Current,
        "the words did not change, so the claim is still current"
    );
    assert_eq!(
        edited.verification,
        Verification::Verified,
        "the trust state is not disturbed either"
    );
    assert!(
        edited.evidence_matches_claim(),
        "the evidence still speaks about the same words"
    );
    assert!(edited.is_reliable(), "nothing about the claim changed");
}

#[test]
fn a_new_commit_marks_other_revisions_for_revalidation_and_deprecates_nothing() {
    let store = MemoryStore::open_in_memory().unwrap();
    let older = store
        .remember(retry_memory().with_code_version("abc123"))
        .unwrap();
    store.verify(older.id, "12/12 tests passed").unwrap();
    let current = store
        .remember(
            NewMemory::episode(REPO, "Current retry approach", "Use the new retry queue.")
                .with_code_version("def456"),
        )
        .unwrap();
    let versionless = store
        .remember(NewMemory::episode(
            REPO,
            "Retry tests",
            "The retries are covered twice.",
        ))
        .unwrap();
    let elsewhere = store
        .remember(
            NewMemory::episode(
                "billing-service",
                "Billing retry",
                "The worker retries at night.",
            )
            .with_code_version("abc123"),
        )
        .unwrap();

    let changed = store.mark_stale_for_version(REPO, "def456").unwrap();

    assert_eq!(
        changed, 1,
        "only the record of another revision of this project is marked"
    );
    let marked = store.get(older.id).unwrap();
    assert_eq!(
        marked.applicability,
        Applicability::NeedsRevalidation,
        "the claim must be checked against the new commit"
    );
    assert_eq!(
        marked.verification,
        Verification::Verified,
        "a new commit is not a contradiction of the claim"
    );
    assert_eq!(
        marked.evidence.as_deref(),
        Some("12/12 tests passed"),
        "the evidence is untouched"
    );
    assert!(
        !marked.is_reliable(),
        "verified but no longer expected to hold is not reliable"
    );

    assert_eq!(
        store.get(current.id).unwrap().applicability,
        Applicability::Unknown,
        "the record of the current revision is left alone"
    );
    assert_eq!(
        store.get(versionless.id).unwrap().applicability,
        Applicability::Unknown,
        "a record that names no revision is not stale"
    );
    assert_eq!(
        store.get(elsewhere.id).unwrap().applicability,
        Applicability::Unknown,
        "another project's record is not stale"
    );
    assert_eq!(
        store.stats(REPO).unwrap().deprecated,
        0,
        "a new commit deprecates nothing"
    );
    assert_eq!(
        store.get(current.id).unwrap().verification,
        Verification::Provisional,
        "and it promotes nothing either"
    );
}

#[test]
fn marking_for_revalidation_twice_keeps_the_evidence_the_trust_and_the_text_alone() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store.remember(retry_memory()).unwrap();
    store.verify(memory.id, "12/12 tests passed").unwrap();
    let verified = store.get(memory.id).unwrap();

    store.mark_needs_revalidation(memory.id).unwrap();
    let once = store.get(memory.id).unwrap();
    store.mark_needs_revalidation(memory.id).unwrap();
    let twice = store.get(memory.id).unwrap();

    assert_eq!(
        once.applicability,
        Applicability::NeedsRevalidation,
        "the claim is asked to be checked again"
    );
    for record in [&once, &twice] {
        assert_eq!(
            record.applicability,
            Applicability::NeedsRevalidation,
            "marking it again lands on the same state"
        );
        assert_eq!(
            record.verification,
            Verification::Verified,
            "what the evidence supported is not changed"
        );
        assert_eq!(
            record.evidence.as_deref(),
            Some("12/12 tests passed"),
            "the evidence is kept"
        );
        assert_eq!(
            record.content_updated_at, verified.content_updated_at,
            "the claim's text was not touched, so its stamp stays"
        );
        assert_eq!(record.title, verified.title, "the title is unchanged");
        assert_eq!(record.content, verified.content, "the text is unchanged");
        assert!(
            record.evidence_matches_claim(),
            "the evidence still speaks about the claim"
        );
        assert!(
            !record.is_reliable(),
            "reliable also asks the claim to be current"
        );
    }
}

#[test]
fn a_recorded_outcome_is_not_a_verdict_on_the_claim_its_trust_or_its_age() {
    let store = MemoryStore::open_in_memory().unwrap();
    let memory = store
        .remember(NewMemory::episode(
            REPO,
            "Webhook signature check",
            "Compare the HMAC in constant time.",
        ))
        .unwrap();
    let before = store.get(memory.id).unwrap();
    tick();

    // Many green runs, and a red one: how the runs went, nothing more. Whether
    // an outcome sways the order of equally relevant memories is the ranking's
    // business (`success` is one of its weights), not the claim's.
    for _ in 0..20 {
        store.record_outcome(memory.id, true).unwrap();
    }
    store.record_outcome(memory.id, false).unwrap();

    let after = store.get(memory.id).unwrap();
    assert_eq!(
        after.content_updated_at, before.content_updated_at,
        "an outcome says how the run went, not that the claim changed"
    );
    assert_eq!(
        after.verification,
        Verification::Provisional,
        "an outcome is not a verdict on the claim"
    );
    assert_eq!(
        after.applicability,
        Applicability::Unknown,
        "an outcome says nothing about the code of today"
    );
    assert_eq!(
        after.evidence, before.evidence,
        "an outcome is not the evidence the claim is supported by"
    );
    assert!(
        !after.is_reliable(),
        "a run going well does not make an unverified claim reliable"
    );
    assert_eq!(
        after.title, before.title,
        "an outcome does not change the claim"
    );
    assert_eq!(after.content, before.content);
    assert_eq!(after.source_kind, before.source_kind);
}

#[test]
fn recalling_a_memory_many_times_does_not_make_it_newer_or_rank_it_higher() {
    let store = MemoryStore::open_in_memory().unwrap();
    let older = store
        .remember(NewMemory::episode(
            REPO,
            "Webhook signature check",
            "Compare the HMAC in constant time.",
        ))
        .unwrap();
    tick();
    // The same words and the same trust state, so only the instant each claim
    // was written separates them: `content_updated_at` is that instant for a
    // new memory, and the one written second is the newer.
    let newer = store
        .remember(NewMemory::episode(
            REPO,
            "Webhook signature check",
            "Compare the HMAC in constant time.",
        ))
        .unwrap();
    let before = store.get(older.id).unwrap();
    let before_newer = store.get(newer.id).unwrap();
    assert!(
        before_newer.content_updated_at > before.content_updated_at,
        "the memory written second is the newer one: {:?} then {:?}",
        before.content_updated_at,
        before_newer.content_updated_at
    );

    for _ in 0..5 {
        store.record_retrieval(&[older.id]).unwrap();
    }
    store.record_retrieval(&[newer.id]).unwrap();

    let after = store.get(older.id).unwrap();
    assert_ne!(
        after.last_retrieved_at, before.last_retrieved_at,
        "a retrieval is recorded on the record it recalled"
    );
    assert_eq!(
        after.content_updated_at, before.content_updated_at,
        "being recalled is not a change of the claim, so it is not fresher"
    );
    assert_eq!(
        store.get(newer.id).unwrap().content_updated_at,
        before_newer.content_updated_at,
        "the other recalled memory is no fresher either"
    );

    let found = store
        .retrieve(&RetrievalQuery::new(REPO, "webhook signature check"))
        .unwrap();
    let order: Vec<String> = found
        .iter()
        .map(|memory| memory.record.id.to_string())
        .collect();
    assert_eq!(
        order,
        vec![newer.id.to_string(), older.id.to_string()],
        "equally relevant memories are ordered by when their claim was written, \
         not by how often they were recalled"
    );
}

#[test]
fn the_claim_hash_ignores_whitespace_and_changes_with_the_words() {
    let claim = claim_hash(
        "Checkout retry race condition",
        "Serialize retries by order ID.",
    );
    assert_eq!(
        claim,
        claim_hash(
            "  Checkout retry race condition  ",
            "  Serialize retries by order ID.  "
        ),
        "leading and trailing spaces are not a different claim"
    );
    assert_eq!(
        claim,
        claim_hash(
            "Checkout  retry   race condition",
            "Serialize retries\n\tby order ID."
        ),
        "re-wrapped and re-spaced words are the same claim"
    );
    assert_ne!(
        claim,
        claim_hash(
            "Checkout retry race condition",
            "Serialize retries by customer ID."
        ),
        "one changed word is a different claim"
    );
    assert_ne!(
        claim,
        claim_hash("Retry race condition", "Serialize retries by order ID."),
        "a changed title is a different claim"
    );
    assert_ne!(
        claim_hash(
            "Checkout retry race condition",
            "serialize retries by order"
        ),
        claim_hash(
            "Checkout retry race condition",
            "serialize retries by orders"
        ),
        "an added word is a different claim"
    );
}
