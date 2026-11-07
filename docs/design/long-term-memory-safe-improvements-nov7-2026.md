# Conservative memory improvements: implementation review

This implements the accepted follow-up to the recall experiments. It adds
exact-copy suppression and observed-file context, and fixes capture admission
that could discard a new observation behind identical claim text. Keyword
expansion and new semantic gates remain research prototypes.

## Code and acceptance review before pushing

| Requirement                                       | Implementation and regression evidence                                                                                                                                                                                                                                                                                                  |
| ------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Exact copies do not consume separate result slots | `MemoryRecord::same_context_claim`, lexical retrieval, semantic candidates/fusion, and packet rendering. `tests/exact_duplicates.rs` verifies refill before limits and one supplied copy.                                                                                                                                               |
| Preserve distinct information                     | Equality includes exact title/content, kind, project, identity, trust/applicability, source kind/episode, evidence binding, code version, confidence, files and tags. Distinct evidence, source links, scopes and sibling claims survive. Hash collisions alone cannot merge text.                                                      |
| Preserve history                                  | Selection chooses a ranked representative. No duplicate is deleted, rewritten, promoted or superseded. Existing IDs remain readable and manager history is intact.                                                                                                                                                                      |
| Use observed source context                       | Successful reads and structured code-search results trigger exact-file verified/current retrieval before the next model request. Paths come from successful tool inputs or structured hits, never from arbitrary result prose. Parallel and sequential calls share the recording path.                                                  |
| Keep retrieval bounded and cached                 | At most four observed files per tool result and three additional records per lookup; only unused slots in the existing eight-record run list are filled. Existing verified context takes precedence. The production packet allowance stays 2,000 estimated tokens. Canonical file keys and the active task survive paused verification. |
| Preserve eligibility and controls                 | The observed-file predicate checks known IDs/copies, file match, verification/evidence, applicability, current source fingerprints and visibility before results consume slots. Failed tools do not retrieve. Current project settings are rechecked; child lookups do not write retrieval associations.                                |
| Use actual failure locations                      | Repair queries include both changed and other files reported by the check. Known, stale or inaccessible candidates are filtered before result limits.                                                                                                                                                                                   |
| Keep useful observations provisional              | Both the tool precheck and transactional write admission compare the captured source/scope, not just claim identity. A two-file regression preserves both original evidence strings, associated paths and provisional trust. Existing source-episode links and original activity records remain unchanged.                              |

The audit corrected an overstatement in the older review: successful source reads
did not previously add file-specific memory to the model packet. The old code had
write UI hints and failure-triggered retrieval. Flat retrieval experiments with
provided file fields did not prove that runtime integration. New scripted tests
inspect actual model requests after parallel reads and searches, including cache,
failed-read, restricted-file and provisional-record cases.

Deduplication is deliberately stricter than incident grouping. Two different
claims from one episode retain separate slots. Different evidence or provenance
also remains separate even when text matches. This avoids treating a repeated
sentence as permission to discard a different observation. Timestamps and neutral
association counters alone do not make identical records distinct.

## Validation and staged delivery

- Linux core nextest CI profile: **2,592 passed**, 27 existing skips.
- Memory crate and examples: **86 passed**, no skips.
- Core doctest command passed with its one existing ignored example.
- Clippy for both changed crates passed with four existing core warnings outside
  these changes and no new warnings.
- Frontend tests, type checking and format checking passed before this
  implementation commit was pushed.

This is a Linux local validation record, not a claim that the remote native
platform/Landlock matrix has finished. No database migration, new embedding
dependency or protocol schema is introduced.

Per the user's requested order, the validated implementation was committed and
pushed as `ac17433` before rerunning the hypotheses. The
[complete rerun](long-term-memory-postimplementation-rerun-nov7-2026.md) identifies
that commit, preserves the original datasets and reports recall, abstention and
packing independently. The existing synthetic retrieval score already supplied
file fields; runtime integration improvements need their scripted request tests
and cannot be inferred from that score alone.
