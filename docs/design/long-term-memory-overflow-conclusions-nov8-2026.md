# Recall experiment: conclusions beyond the encoder window

Baseline: `8064e2a`, branch `memory-improvements-nov8-2026`.

Indexing an explicitly separated conclusion that MiniLM could not see recovers
three complete answers in the new long-note evaluation. Development improves
from **31/40 to 33/40**, and the exposed reserved split from **33/40 to 34/40**.
The original retrieval results keep priority; no full pool is changed.

## Why this experiment

The previous wider-pool, symbol-fusion and diversity experiments lost requested
facts. The historical fixtures also had a coverage gap: every stored claim fit
inside the encoder's 500-character window. They could not expose a memory whose
useful conclusion appears later in a long stored note.

The [110-case fixture](../../scripts/eval/memory-overflow-cases.json) reuses the
20 incidents and negative queries from `memory-semantic-fresh-cases.json`.
Each incident has a fixed, explicitly synthetic session preface followed by the
original lesson, plus a one-paragraph ablation, a short-record control and a
literal identifier control. Each split has 40 positives and 15 negatives. The
preface is frozen before trials; it is not an inferred or manually authored
retrieval alias. These are correlated mechanism checks on exposed incidents,
not fresh blind semantic judgments or measured agent success.

The [protocol](../../scripts/eval/memory-overflow-protocol.json) registers the
representation, strict normal admission, an unused conservative-cutoff alternative,
and the existing per-fact, negative-identity and budget guards.

## What changed

Background indexing retains the existing title-plus-claim vector. When a provider
declares a positive character limit, it can additionally embed one final paragraph
that begins beyond that window. The conclusion view normalizes CRLF, ignores
trailing whitespace, and is bounded to one encoder window. Single-paragraph claims
and short records retain their existing representation.

The extra vector lives in a rebuildable SQLite table keyed by memory, model and
current claim hash. Existing primary vectors gain the extra view without being
reembedded. Both writes commit together after generation, cancellation and claim
checks; inference runs outside the store lock. A failed supplementary inference
preserves successful primary indexing and can be retried. Edits, deletion and
clear cannot revive stale text. Clearing a semantic index removes both views.

Hybrid retrieval looks up conclusion matches only after all existing retrieval
steps and only when slots remain and no incumbent exactly names the requested
title. It applies the unchanged normal cosine cutoff, without short-query
relaxation, and appends results below incumbents. Repository, trust, excluded tags,
caller visibility and duplicate checks apply before the supplementary limit.
Strict primary-vector search and lexical retrieval keep their existing behavior.
The app's blocking background encoder now forwards the native character limit,
so scheduled indexing actually creates these vectors.

No new model, download, Python runtime, release asset, claim rewrite, global
threshold change or larger result/token budget is involved. At most one extra
inference and vector are added per eligible long claim. Intermediate paragraphs
and single-paragraph overflow remain outside this experiment.

## Results and rejected prototype

| Hybrid view                | Development before → after | Reserved before → after |
| -------------------------- | -------------------------: | ----------------------: |
| Hidden final paragraph     |                7/10 → 9/10 |             8/10 → 9/10 |
| Single-paragraph overflow  |                7/10 → 7/10 |             8/10 → 8/10 |
| Short-record control       |                7/10 → 7/10 |             7/10 → 7/10 |
| Literal identifier control |              10/10 → 10/10 |           10/10 → 10/10 |
| All positives              |              31/40 → 33/40 |           33/40 → 34/40 |

Recovered cases are `timezone` and `websocket` in development and `csrf` in the
reserved split, all with the hidden-paragraph variant. Delivery requires the
complete original long claim to reach the actual rendered packet; merely finding
its conclusion vector is insufficient. Lexical scores remain unchanged at
28/40 development and 23/40 reserved.

The initial prototype also indexed arbitrary suffixes of one-paragraph claims.
It passed the new development guards but failed the unchanged existing
`supplementary_diagnostics_require_a_truncated_character_window_a_failure_and_a_free_slot`
test: removing a claim's original context manufactured an additional match.
That prototype was rejected. The protocol amendment restricts indexing to an
explicitly separated, previously invisible paragraph; the original test is intact
and passes. No cutoff tuning was needed.

The preliminary reserved evaluation had already started alongside the failing
crate suite. The amendment responds to the existing unit contract, not reserved
outcome selection. The report retains that exposure explicitly; neither these
records nor the earlier incidents provide a blind generalization estimate.

## Regression checks

All **1,336 historical policy views** retain exactly the same deterministic
outputs, including full-pool facts and ranks, negative IDs and order, scope and
packet budgets. The new fixture adds **220 policy views**. Its 30 negatives retain
their exact outputs; this does not establish human-rated precision. No previously
delivered answer or requested fact is lost or ranked lower. The final limit stays
eight memories and the packet budget stays 1,000 tokens.

Tests cover migration from primary-only vectors, eligibility before limits,
Unicode and CRLF, full-pool retention, cancellation during supplementary inference,
concurrent edit/delete/clear, model identity, and supplementary failures. The
native app-worker test includes a long conclusion in each cloned project and
verifies that scheduled background work leaves no missing vectors.

The [machine-readable report](long-term-memory-overflow-conclusions-nov8-2026.json)
records per-suite metrics, recovered ranks, source/executable hashes, negative
counts and deterministic output fingerprints. Replays overlapped compilation and
frontend checks, so they are not a latency benchmark.

## Reproduction

Build the Rust `evaluate_memory` example in `verified-code-memory`. Use:

```json
{
  "dataset_file": "scripts/eval/memory-overflow-cases.json",
  "split": "development",
  "deterministic": true,
  "candidate_limit": 8,
  "memory_budget": 1000,
  "short_query_words": 2,
  "short_query_scale": 0.9166666666666666
}
```

Pass the file with `--experiment-config <path>`. For hybrid retrieval set
`ROVIK_MEMORY_EVAL_MODEL` to the pinned local ONNX directory,
`ROVIK_MEMORY_EVAL_DIMENSIONS=384`,
`ROVIK_MEMORY_EVAL_REVISION=chroma-minilm-l6-v2-913d7300-title-claim-native-v1`,
and `ROVIK_MEMORY_EVAL_THRESHOLD=0.3`. Unset those overrides for lexical retrieval.
Use the same configuration and fixture against the baseline runtime. Its source
and evaluator are identical to the preceding runtime commit; the intervening
commit added evaluation data and reports only.
