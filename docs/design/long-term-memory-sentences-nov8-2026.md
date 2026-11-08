# Complete hidden sentences: recall experiment

Branch: `ltm-improvements-nov8-2026`. Baseline: `31f1a7c`.

On the same existing cases, indexing a complete final sentence outside the encoder
window improves long-note delivery **33/40 → 35/40 development** and **34/40 →
35/40 reserved**. Combined task/note windows improve **32/40 → 34/40 development**;
the reserved score remains **33/40**. No benchmark cases or budgets changed.

## Direction and discarded trial

The previous arbitrary-suffix trial failed an existing context contract. The
accepted paragraph view required an explicit blank-line boundary, leaving the
single-paragraph ablation cases unresolved. A complete sentence offers another
linguistic boundary without indexing a fragment cut from the middle of a claim.

Before this trial, a [compact-packet experiment](../../scripts/eval/memory-compact-packet-protocol.json)
tried recovering already-retrieved facts by reducing repeated metadata. It retained
baseline complete claims and fetchable IDs, and would activate only when another
full claim fitted. It recovered no additional development answers in the existing
long-note and combined-window suites; the remaining expected answers were absent
from the retrieved pool. That code was removed before the sentence trial.

## Selected change

Background indexing preserves the existing final-paragraph view. When no explicit
paragraph boundary is present, it can use a complete final sentence, provided:

- It begins after a period followed by a space, outside the encoder window.
- Its predecessor is an alphabetic word of at least three bytes, excluding
  abbreviation-like boundaries such as `e.g.`.
- The sentence begins with an uppercase character, ends in sentence punctuation,
  and fits entirely within the declared character window.

The existing checks for a positive provider limit, current model and claim hash,
visibility, repository, verification and excluded tags remain in force. Existing
primary vectors are reused; a previously unindexed sentence gets one derived
conclusion vector during normal backfill. The total remains at most two vectors
per eligible long record. Inference remains native Rust with the same pinned
MiniLM files and preprocessing.

Retrieval uses the existing conclusion and independent-diagnostic paths, with
matches appended after incumbents. It never evicts an existing result. The complete
original claim must reach the rendered packet for a successful answer. No stored
claim, evidence or trust state is rewritten, and strict primary-vector search
retains its original behavior.

This is a conservative sentence-boundary heuristic, not a linguistic parser.
The experiment reuses exposed, correlated synthetic incidents and does not establish
blind generalization, multilingual quality, precision or live-agent task success.

## Results on unchanged suites

| Hybrid suite               | Development before → after | Reserved before → after |
| -------------------------- | -------------------------: | ----------------------: |
| Long notes                 |              33/40 → 35/40 |           34/40 → 35/40 |
| Combined task/note windows |              32/40 → 34/40 |           33/40 → 33/40 |
| Complex identifiers        |              28/28 → 28/28 |           28/28 → 28/28 |
| Denser full-pool cases     |              22/28 → 22/28 |           24/28 → 24/28 |
| Long diagnostics           |              26/30 → 26/30 |           28/30 → 28/30 |

The five improved views concern `timezone` and `websocket` in development and
`csrf` in reserved evaluation. They are five case gains across three source
incidents. All are existing single-paragraph overflow cases. New complete claims
reach ranks one to three in packets of 222–677 estimated tokens.

All **2,100 policy views** were replayed, including the newly committed long-failure
and three retention suites. **2,095 deterministic outputs remain identical** after
excluding latency and timed telemetry; the five changed views are complete-answer
gains. Every previously delivered primary and additional requested fact retains
its delivery and rank. Every negative retains exactly the same returned IDs and
order. Scope, trust, tag, caller-visibility and 1,000-token guards pass; the final
result limit remains eight. Existing false injections persist.

Development, all historical development replays and the unchanged crate contracts
passed before reserved scoring. No cutoff or sentence policy was retuned from
reserved results. The cases and reserved incidents had been exposed before;
this is a fixed validation check rather than a blind holdout.

## Validation and reproduction

The changed memory crate passes **150 all-target tests**, both explicitly enabled
native ONNX tests, its doctest and clippy with warnings denied. The new regression
checks complete hidden sentences, incomplete fragments, abbreviation-like contexts,
packet delivery, cache reuse and stale edits. All existing tests remain intact.
All **1,046 frontend tests**, type checking and formatting pass. Crate checks were
run separately.

The [protocol](../../scripts/eval/memory-sentence-protocol.json) and
[machine-readable report](long-term-memory-sentences-nov8-2026.json) record input
hashes, suite metrics, gained cases, source bindings and output fingerprints. The
baseline reuses the preceding selected runtime outputs with unchanged source and
inputs; candidate views are replayed afresh. Concurrent compilation and frontend
checks mean the recorded timings are not a latency benchmark.

Build the existing Rust `evaluate_memory` example and select the unchanged
`memory-overflow-cases.json` or `memory-cross-window-cases.json`. Use deterministic
splits, eight records, a 1,000-token budget, short-query words two and scale 11/12.
For hybrid retrieval use the pinned local model, dimensions 384, revision
`chroma-minilm-l6-v2-913d7300-title-claim-native-v1` and normal cutoff 0.3. Compare
both lexical and hybrid outcomes against `31f1a7c`, and run all listed regression
fixtures before accepting another policy.
