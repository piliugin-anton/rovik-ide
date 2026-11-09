# Additive encoder experiments

Baseline: `e52336de1317f934f04c7f90aa3733211d9ff62e` on
`ltm-improvements-nov10-2026`.

Neither tested additive family produces a safe recall improvement. An E5 winner
alone cannot recover any remaining development answer. Deeper MiniLM/E5 rank
agreement recovers pagination, but every gaining policy changes at least 14
negative packets. Keep production MiniLM, its defaults, model provisioning and
release process unchanged. No new fixture or expected answer was added or revised.

This follows the rejected
[whole-encoder replacement](long-term-memory-retrieval-encoder-nov10-2026.md).
The implementation adds evaluation-only, label-independent candidate diagnostics
and native append packet previews. It introduces no second production encoder.

## Fixed inputs and admission features

Use the same official pinned E5 graph, tokenizer, role prefixes and normalization
as the preceding study, with pinned MiniLM at cutoff 0.30 and short-query scale
11/12. The original, fresh and new development cohorts have 203 cases: 132
positive and 71 negative, with 11 positive misses. Both models see the same
corpus, query inputs, trust/filter rules and deterministic 36-hour fixture age.
The eight-result limit and 1,000 estimated-token allowance remain fixed.

The [first protocol](../../scripts/eval/memory-e5-additive-protocol.json) freezes
220 score/margin/corroboration policies before the new score-margin probes.
After that family fails, the
[consensus protocol](../../scripts/eval/memory-e5-consensus-protocol.json)
registers a distinct 210-policy rank-agreement trial before its deeper pool
probes. This second hypothesis follows observed development failure; it was not
registered before the first experiment. Neither uses heldout outcomes.

Admission sees independently ranked eligible candidates, their scores, ranks,
and existing MiniLM IDs. Expected identities, categories, answerability and
required-fact labels are used only to score the result. Missing runners-up reject
the query. Existing records are never removed before candidate selection, and a
rejected/already-present winner does not cause selection of a different record.

## First family: E5 winner and margin

Consider only E5's first primary-index candidate, with at least two eligible
distinct candidates. Append it only to an unused MiniLM slot when it is absent,
its cosine passes a registered floor and its first-versus-second margin passes
a registered floor. Optional corroboration requires membership in MiniLM's
independent top three. This produces 11 × 10 × 2 = 220 policies.

There are **zero possible complete-answer gains**, even before packet checks.
Every remaining expected primary is absent from the MiniLM result, and none is
E5's first candidate:

| Development miss                | E5 eligible primary rank |
| ------------------------------- | -----------------------: |
| pagination                      |                        2 |
| worktree                        |                       13 |
| provenance                      |                        2 |
| fallback                        |                        5 |
| generation                      |                       14 |
| cancellation, fresh             |                       12 |
| rate limits, fresh              |                        2 |
| optimistic locking, fresh       |                        6 |
| journal durability symptom, new |                        3 |
| reservoir sampling symptom, new |                       10 |
| negative DNS cache symptom, new |                        3 |

An absolute or top-margin gate cannot promote a lower-ranked fact. This family
stops at a candidate upper-bound screen; its 220 policies are not claimed as
native packet replays. The earlier E5 replacement gains came from lower-ranked
pool members, rather than correct first results.

## Second family: deeper rank agreement

Intersect the independent, filtered, distinct primary-index top-eight lists.
Rank shared IDs by equal reciprocal rank fusion,
`1 / (60 + MiniLM rank) + 1 / (60 + E5 rank)`, then UUID for ties. Consider only
the first consensus record, requiring at least two shared records. Gate it with
registered E5-cosine, MiniLM-cosine and fused winner-versus-runner-up margin
floors: 7 × 6 × 5 = 210 policies. No query-specific terms, aliases or special cases
are introduced.

Native diagnostics now provide, independently of expected labels:

- `best_eligible`: the first three eligible, deduplicated score/identity/hash rows.
- `candidate_pool`: up to the query limit, with the same production search order.
- Each pool candidate's `append_preview`: the actual native packet after an
  append, or a result-limit, same-context-copy or packet-guard rejection.

A preview preserves all incumbent identities and positions. It accepts only when
the candidate renders in full and every previously complete incumbent stays
complete under the unchanged allowance. It reads already filtered index records
and does not modify retrieval, records or stored vectors. New diagnostics remain
opt-in through the existing `diagnostics: true` experiment field.

Each policy replays either its unchanged native MiniLM case or its winner's
accepted native packet preview. Full returned/rendered/complete ID arrays and
estimated packet tokens come from Rust's real renderer. Primary/required ranks
and complete delivery are derived from those identities; claim hashes stay
unchanged. This is diagnostic policy replay, not a production dual-model
retrieval implementation or a latency measurement. Packet previews were added
after the initial consensus screen to verify the proposed gains and negative
changes; no admission thresholds changed during that confirmation.

The unchanged [comparator](../../scripts/eval/memory-regressions.mjs) rejects every
gaining policy. It protects primary/required identities and hashes, complete
answers and retrieved ranks, exact negative packet identities/order, eligibility,
embedding success and declared bounds. None of the replay policies loses a
previous positive fact or worsens its rank. Negative changes still disqualify them.

| Family                                             | Policies | Policies with a gain | Safe policies with a gain |
| -------------------------------------------------- | -------: | -------------------: | ------------------------: |
| E5 first candidate, candidate screen               |      220 |                    0 |                         0 |
| Shared top-eight rank fusion, native packet replay |      210 |                    8 |                         0 |

The other 202 consensus policies have no gains: 99 pass the comparator without
improving recall, and 103 change negatives. The only recovered identity is
`pagination-implicit`. Among gaining policies, the fewest changed negatives is
14, with E5 floor 0.75, MiniLM floor 0.15 and fused margin floor 0.0001:

| Cohort            | Complete answers before → after | Negative queries returning any record before → after | Changed negative packets |
| ----------------- | ------------------------------: | ---------------------------------------------------: | -----------------------: |
| Original          |                   65/70 → 66/70 |                                          3/40 → 9/40 |                        6 |
| Fresh             |                   27/30 → 27/30 |                                        10/15 → 11/15 |                        2 |
| New encoder cases |                   29/32 → 29/32 |                                         8/16 → 13/16 |                        6 |

These are 12 newly nonempty negative queries plus two that already returned
records. Forty-two comparator errors represent three changed ID arrays per
negative, not 42 independent failed cases. Multiple policies/queries share the
same incidents; their counts are not independent successful agent tasks.

## Why tighter cutoffs cannot select the only gain

The new no-match request “How should an HTTP client follow a permanent redirect?”
receives a CORS-preflight fact as the first candidate in both models. Its frozen
rationale says that CORS permissions do not answer redirect handling. It exceeds
the pagination gain on every admission feature:

| Native feature                        | Pagination recovery | CORS fact for unsupported redirect request |
| ------------------------------------- | ------------------: | -----------------------------------------: |
| E5 cosine                             |            0.774617 |                                   0.822923 |
| MiniLM cosine                         |            0.178180 |                                   0.275462 |
| Fused winner margin                   |            0.000224 |                                   0.000785 |
| Packet after append, estimated tokens |                  65 |                                         87 |

Both MiniLM baselines return zero records and have free slots. Both candidates
render in full under 1,000 tokens. Therefore any combination of monotone lower
bounds on these three features that admits this gain also admits this negative,
even outside the registered grid. Packet rollback does not separate them. This
conclusion applies to this consensus winner and these features; it does not rule
out another model, candidate selector or relevance signal.

## Verification, limits and reproduction

Fifteen native diagnostic runs provide **1,015 case views**: six top-three runs,
six top-eight runs and three MiniLM append-preview runs. All reproduce their
previously accepted MiniLM or measured E5-0.725 outputs in every non-timing case
field after removing diagnostic data. The 210 consensus policies reuse native
previews for **42,630 case replays**; these are not 42,630 separate encoder calls.
All inputs, models and comparison rules remain fixed. No candidate proceeds to
historical/oversized or heldout scoring after development rejection; the new
reserved questions remain unscored.

All 23 focused evaluator tests pass, including new contracts proving leader/pool
independence from expected labels, search order, scope/trust/tag/caller filtering,
exact-copy handling, query limits, full append delivery and packet rollback.
Example clippy passes with warnings denied. Changed Rust and documentation/JSON
formatting are checked. Broader crate/core/frontend tests remain skipped under
the user's instruction. Production library/core sources are unchanged.

The [machine report](long-term-memory-additive-encoders-nov10-2026.json) records
input protocols, source/executable/output hashes, baseline parity, all policies,
per-case candidate features and exact native append traces. Its trial observation
indices refer to the shared `observations` array. The stronger negative and gain
are identified explicitly. Temporary native outputs and orchestration stay local.

To reproduce, use the preceding encoder report's pinned model/config commands
with `diagnostics: true`, development only, and E5 cutoff 0.725 for diagnostics.
Use query/primary `best_eligible` for the first family and `candidate_pool` for
the second; their scores are reported below admission cutoffs too. Apply the
registered feature gates without labels. For each selected consensus winner,
use its MiniLM `append_preview`, retaining the baseline on any guard rejection.
Compare the resulting identity/rank/delivery traces with the unchanged comparator.
The protocol JSON files enumerate every floor and rank-fusion constant.

These remain exposed synthetic retrieval cases, not independently judged
precision or capture/agent-task evidence. The comparator also does not judge
relevance of every added record on an already answered positive query; several
file controls gain extraneous records while retaining their expected answer.
Passing fact/rank guards alone would not establish a safe precision improvement.

Simple encoder agreement and monotone score gates have now been tested and
rejected. A distinct next study is a query-document relevance reranker on a
frozen eligible pool containing the low-ranked expected facts and these strong
unsupported-query counterexamples. It must improve separation before adding a
model to production. Capture accuracy and paired memory-on/off agent tasks remain
separate priorities; repeating cosine/rank cutoffs supplies no new evidence there.
