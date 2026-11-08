# Harder identifier recall experiments

Baseline: `c9e0fd9`. Branch: `memory-improvements-nov8-2026`.

The earlier identifier fixture reached 100% on isolated symbol/prose lookups.
Those cases remain unchanged regression controls. The new
[80-case fixture](../../scripts/eval/memory-complex-identifier-cases.json) retains
isolated checks and adds competing text, failure and file context, composed
answers requiring two facts, full eight-record pools, and trust/tag/caller
exclusions. Each split has 28 answerable cases and 12 negative controls across
four source symbols. Repository isolation includes fully indexed foreign copies.
The claims and source references come from the existing grounded set; these are
constructed stress cases, not fresh blind semantic judgments or agent success.

The Rust evaluator now supports explicit corpus records and per-case additions,
verification/tag filters and caller exclusions. A composed answer succeeds only
when every required record and its complete claim text reach the actual rendered
packet. Primary and additional-fact ranks are recorded separately, so partial
answers cannot hide a lost fact. Malformed fixture protections fail explicitly.
Both baseline and candidates use the extended evaluator; the baseline keeps the
previous retrieval behavior with a forwarding API for filtered telemetry.

## Results

The [protocol](../../scripts/eval/memory-complex-identifier-protocol.json) fixes
claims, policies, an eight-record limit and a 1,000-token budget before trials.
Selection uses development; only the selected variant is scored on heldout.
Source incidents and models were exposed in earlier work, so heldout is a fixed
validation condition, not a claim of independent generalization.

| Policy / split        | Baseline complete answers | Selected complete answers |
| --------------------- | ------------------------- | ------------------------- |
| Lexical / development | 4/28                      | 16/28                     |
| Lexical / heldout     | 4/28                      | 16/28                     |
| Hybrid / development  | 24/28                     | 24/28                     |
| Hybrid / heldout      | 20/28                     | 24/28                     |

The old 100% score survives on the isolated controls, while the new aggregate
reveals failures it could not measure. Hybrid heldout candidate recall was
21/28, but only 20/28 complete answers were supplied before the change: checking
multiple required facts matters.

The first trial appended conjunctive symbol matches after any ordinary pool
with a free slot. It reached 24/28 lexical development answers, but failed the
existing test that protects literal and explicit-file lookups from expansion.
That prototype is rejected. The old test was retained unchanged. The protocol
records an amendment, made before the next trial, restricting supplementation to
unresolved text contexts. The conditional experiment to separate hybrid
supplements was unnecessary because development hybrid ranks did not regress.

The selected implementation fills free slots only when ordinary results exist,
no explicit file is requested, and no incumbent title equals the task or failure.
It keeps ordinary ranks and puts new records below them. Complete conjunctive
symbol terms, trust, tags, caller visibility, scope and exact-copy rejection are
checked before the supplementary pool is limited. It changes no claims, aliases,
stored index schema, model weights, cutoffs or download/release assets.

No delivered fact is lost or ranked lower, including independently supplied
members of composed answers. All 24 negative controls retain exactly the same
record IDs and order. Eight of twelve negatives per split intentionally retain
background context; the four unrelated-symbol controls per split still abstain.
The overall any-record injection rate therefore stays 8/12. This is not a claim
of improved precision or perfect abstention.

All **792 earlier replays**—the original, fresh, diagnostic and identifier sets,
each under lexical and hybrid policies—retain delivery, rank, returned IDs,
packet size and scope exactly. Original literal/file protection tests still
pass. Tests also cover duplicates crowding the supplementary pool and eligibility
checks before its limit.

## Remaining hard cases

The selected lexical policy still misses misleading-file contexts and full
pools (12/28 positives in each split). Hybrid still misses four positives per
split; the exact IDs are retained in the
[machine-readable report](long-term-memory-complex-identifiers-nov8-2026.json).
No difficult category was removed, and no larger result/token budget was used
to recover the old 100% headline.

## Reproduction

Build the existing Rust `evaluate_memory` example. Select the new dataset with
`dataset_file: "scripts/eval/memory-complex-identifier-cases.json"`, plus
`deterministic: true`, `memory_budget: 1000`, `short_query_words: 2`,
`short_query_scale: 0.9166666666666666` and the desired split. Paths are relative
to the process working directory. Omit `ROVIK_MEMORY_EVAL_*` variables for lexical
retrieval. For hybrid use the pinned local model, dimensions 384, the existing
model revision and threshold 0.3.

To build the reference, use the baseline body's
`MemoryStore::retrieve_filtered_with_telemetry` from `c9e0fd9` with the extended
fixture evaluator and the new forwarding method on `SharedMemoryStore`; these
support fixture measurement without enabling supplementation. The report binds
the evaluated reference and candidate executables and source snapshots. Dataset
registration uses two-space `JSON.stringify` plus a final newline; any formatter
reflow must preserve that reconstructed hash and the parsed data exactly.
