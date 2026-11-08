# Covering multiple explicitly named code facts

Branch: `ltm-improvements-nov9-2026`. Baseline: `f933c77`.

On the unchanged denser recall suite, hybrid delivery improves from **22/28 to
26/28 development** and **24/28 to 28/28 reserved**. All eight existing requests
requiring three facts now receive all three complete claims. No benchmark cases,
model weights, cutoffs, result limits or packet budgets changed.

## Why this direction

The [focused-symbol experiment](long-term-memory-focused-symbols-nov8-2026.md)
recovered a missing named fact while protecting broader requests. Its single-name
scope left the existing three-fact cases unresolved. Some already supplied one
or two requested facts; simply replacing the last record could discard them.
The previous [long-diagnostic replacement trials](long-term-memory-long-failures-nov8-2026.md)
also showed that a stable result count cannot conceal lost required observations.

This experiment treats explicit names as a set to cover. It keeps already supplied
named facts at their original positions and plans all remaining additions before
changing the result list.

## Fixed scope and implementation

The [protocol](../../scripts/eval/memory-multi-symbol-protocol.json) registers two
to four distinct compound identifiers in at most five whitespace words, with at
most one additional context term. Existing collection-word, numeric and wildcard
protections apply. Requests with supplied files/failures or exact incumbent titles
retain their earlier behavior. Only full pools with more than one slot qualify.

For each requested name, the existing indexed conjunctive lookup applies repository,
verification, tags, caller visibility and copy checks before its candidate bound.
An incumbent-only scan protects represented facts even below the first 80 ordinary
supplementary matches. Planned additions can also cover another requested name,
so an alias or a record matching multiple names does not consume duplicate slots.

The leading result, every existing named match, and exact titles mentioned by
individual request terms are protected. The latter includes literal context names,
with punctuation normalization. Only other trailing positions may be used; retained
records do not move. Each replacement inherits its position's rank score.

If any requested name has no eligible match, or there are insufficient unprotected
slots, the entire plan is abandoned. Lookup errors preserve incumbent results.
There is no partial replacement before all checks succeed. Lexical and strict
vector searches retain their existing behavior. Additional work consists of bounded
retained FTS pools; no inference, backfill, vector, model download or schema is added.
The indexed scans may read beyond their retained pool bound when filtering.

A new unit probe found that a literal context record could otherwise be displaced.
That protection was added before reserved scoring. Tests cover recovery of multiple
facts, preserving already supplied ranks, exact context titles, unknown names that
must abort the plan, and broader collection/narrative requests. Previous scope,
trust, tag, deprecated-record, foreign-copy and dense-pool tests remain unchanged.

## Results on the same cases

| Hybrid suite                    | Development before → after | Reserved before → after |
| ------------------------------- | -------------------------: | ----------------------: |
| Denser full-pool cases          |              22/28 → 26/28 |           24/28 → 28/28 |
| Three-fact subset of that suite |                  0/4 → 4/4 |               0/4 → 4/4 |
| Complex identifiers             |              28/28 → 28/28 |           28/28 → 28/28 |
| Long notes                      |              35/40 → 35/40 |           35/40 → 35/40 |
| Combined task/note windows      |              34/40 → 34/40 |           33/40 → 33/40 |

The eight gains are existing, correlated case views. Their named combinations
share historical source incidents; they do not constitute eight new independent
semantic validations. The two remaining development misses in the denser suite
have explicit file context, which this policy preserves.

All **2,100 existing policy views** were replayed with the final candidate.
**2,092 deterministic outputs remain identical**, excluding latency and timed
telemetry. The eight changed views are complete-answer gains. Every previously
supplied primary and required fact retains its delivery and rank. Every negative
retains the same record IDs and order. All retention suites, scope/trust/tag/caller
checks, the eight-record limit and the 1,000-token allowance pass.

This intentionally changes unprotected records in focused pools. The evidence
establishes preservation of requested facts in these fixtures; it does not prove
that every replaced contextual record is irrelevant on every real task. The cases
and reserved incidents are exposed, and no independent precision, multilingual,
latency or live-agent gain is claimed. Existing false injections remain unchanged.

Development, the full unchanged crate contracts and historical development checks
passed before reserved evaluation. No scope rule, cutoff or fixture was retuned
from reserved results.

## Validation and reproduction

The changed memory crate passes **152 all-target tests**, both explicitly enabled
native ONNX reference tests, its doctest and clippy with warnings denied. All
**1,046 frontend tests**, type checking and formatting pass. Rust crate checks
were run separately.

The [machine-readable report](long-term-memory-multi-symbols-nov8-2026.json)
records frozen inputs, all suite metrics, each changed case before/after, source
hashes and deterministic fingerprints. Historical baselines reuse the previous
selected runtime with identical retrieval source and inputs; candidate views are
replayed afresh. Concurrent checks make timings unsuitable as a latency benchmark.

Build the existing Rust `evaluate_memory` example. Select the unchanged
`memory-fullpool-retry-cases.json`, deterministic splits, limit eight and budget
1,000. Use the pinned local native MiniLM model, 384 dimensions, revision
`chroma-minilm-l6-v2-913d7300-title-claim-native-v1`, threshold 0.3, short-query words
two and scale 11/12. Compare against `f933c77`, then replay every regression input
listed in the protocol before accepting further changes.
