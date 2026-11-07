# Memory hypotheses after the conservative fixes

Implementation commit [`ac17433`](https://github.com/piliugin-anton/rovik-ide/commit/ac174333a143e90e10bd9d58658f42df32c247a6)
was tested, committed and pushed **before** these experiments began, as requested.
The [implementation review](long-term-memory-safe-improvements-nov7-2026.md)
maps the accepted requirements to code and regression tests.

The rerun finds **no additional recall improvement** from the earlier semantic
variants. Production lexical/file Recall@8 remains **80%** (24/30 validation
answers). All seven proposal variants reproduce their previous selected metrics
and all **924 per-case outcomes**, including ranking, supplied answers, returned
counts, packet sizes and false injections. Exact deduplication improves the
constructed crowding case without changing those retrieval outcomes.

## Complete representation rerun

The original campaign stopped after three distinct non-improvements. The user's
subsequent request to retry everything explicitly overrides that stopping rule
for this separate run; the original report and protocol remain unchanged.
Thresholds are still selected on development only, with no increase over baseline
false injections, and only the selected representation is scored on validation.

| Representation          | Selected threshold | Development Recall@8 | Result against the incumbent           |
| ----------------------- | ------------------ | -------------------- | -------------------------------------- |
| Lexical/file control    | —                  | 58/70 (82.86%)       | Baseline                               |
| Mean word vectors       | 0.80               | 59/70 (84.29%)       | One additional development answer      |
| IDF-weighted vectors    | 0.80               | 59/70 (84.29%)       | No additional improvement              |
| Remove common component | 0.60               | 59/70 (84.29%)       | No additional improvement              |
| Character n-grams       | 0.90               | 58/70 (82.86%)       | No additional improvement              |
| Title weighted fourfold | 0.85               | 59/70 (84.29%)       | No additional improvement              |
| Title only              | —                  | Unavailable          | Pinned model cannot encode every title |

The selected mean-vector model again supplies 24/30 validation answers, with
3/10 no-match injections: both equal the lexical/file control. Title-only was
attempted, but a document without an in-vocabulary word causes the real index to
reject its zero vector. It is an unsupported representation for this model,
not a measured recall failure. No fabricated or random vector substitutes for it.

## Proposal hypotheses H1–H7

These reuse the frozen corpus, hard negatives and manually authored, source-bound
context descriptors from the [earlier proposal report](long-term-memory-proposal-followup-nov7-2026.md).
There is no retuning against validation and no new descriptor authoring.

| Hypothesis / selected variant                         | Validation Recall@8 | No-match injection | Comparison with before the fix |
| ----------------------------------------------------- | ------------------- | ------------------ | ------------------------------ |
| Lexical/file control                                  | 80%                 | 30%                | Identical                      |
| H1: contextual search keys                            | 90%                 | 40%                | Identical                      |
| H2: semantic-only ablation                            | 16.67%              | 20%                | Identical                      |
| H1 + hybrid fusion                                    | 90%                 | 40%                | Identical                      |
| H3: intent without supplied file context              | 63.33%              | 30%                | Identical                      |
| H5: 32 lexical candidates, final eight                | 80%                 | 30%                | Identical                      |
| H5: contextual candidates with semantic gate/reranker | 90%                 | 30%                | Identical                      |

The thresholds remain 0.65 for semantic-only, 0.90 for contextual hybrid and 0.50
for the contextual gate. The gate's validation MRR is 0.8333 and nDCG@8 is
0.8508, also identical to the prior machine-readable result. Recall alone does
not describe ordering quality.

H4's information-matched representation controls also reproduce the earlier
constructed results: structured claims deliver 82/100 answers, and both flattened
and source-linked contextual claims deliver 97/100. This supports retaining
context but does not demonstrate an advantage from linkage itself. H7's
activity-only records contain none of the answers by construction; supplying
structured claims yields 100% capture coverage and 82% delivery. Automatic
extraction accuracy and agent task success remain unmeasured.

H6's same thirteen-candidate crowding test now distinguishes suppression while
rendering from suppression **before** the result limit:

| Selection with the new renderer                 | Requirements supplied | Tokens | Records |
| ----------------------------------------------- | --------------------- | ------ | ------- |
| Take eight, then render                         | 1/2                   | 70     | 1       |
| Production exact deduplication, then take eight | 2/2                   | 136    | 2       |
| Experimental incident collapse, then take eight | 2/2                   | 136    | 2       |

Before this fix, plain top-eight rendering repeated the first fact in 514 tokens.
The new renderer removes those repetitions, but cannot recover an independent
candidate already excluded by the limit. Production retrieval deduplicates before
the limit and therefore supplies both facts. This establishes the tested
mechanism, not how often crowding occurs in authentic histories. Production
equality preserves different claims, evidence, scopes and source episodes;
incident-level merging remains experimental.

## Decision and verification

Keep the conservative implementation: suppress exact copies, retrieve using
actual successful reads/search hits, and retain distinct source observations.
The scripted core request tests cover that runtime integration. The flat
retrieval fixture already supplies file context and cannot measure the benefit
of discovering it during a real task.

Keep contextual expansion and semantic gating experimental. Their 90% recall
still comes with 30% false injections, above the 5% release target. Manual
descriptors and previously exposed validation also prevent a generalization
claim. No transformer encoder, learned cross-encoder, automatic extractor or
live-agent answer-quality experiment is implied by this rerun.

Across both campaigns, 13,332 case evaluations had zero project-scope violations
and zero 1,000-token allowance overruns. The largest packet was 463 estimated
tokens. All recalled answers survived packing in the selected configurations.
The real model remains spaCy 3.8.11 with `en_core_web_md` 3.8.0, using pooled
300-dimensional pretrained word vectors. Input hashes, binary/source hashes,
threshold trials and compact per-case outcomes are in the
[machine-readable artifact](long-term-memory-postimplementation-rerun-nov7-2026.json).

Before the implementation push, Linux core nextest passed 2,592 tests (27 existing
skips), and memory nextest passed 86. After the research harness changes, all 86
memory tests passed again, including the production deduplication stress check;
Clippy added no warnings. Python protocol checks, frontend tests, type checking
and `format:check` also passed before the results commit. Native macOS/Windows
and the kernel-dependent Landlock matrix were not executed locally.

To reproduce with the earlier report's pinned model environment and rebuilt
Rust examples (adjust executable paths for `CARGO_TARGET_DIR`):

```sh
python scripts/eval/memory-experiments.py \
  --all-hypotheses \
  --evaluator /tmp/rovik-ci-target/debug/examples/evaluate_memory \
  --output /tmp/memory-all-rerun
python scripts/eval/memory-followup.py \
  --evaluator /tmp/rovik-ci-target/debug/examples/evaluate_memory \
  --structure-evaluator /tmp/rovik-ci-target/debug/examples/evaluate_memory_structure \
  --output /tmp/memory-proposal-rerun
```
