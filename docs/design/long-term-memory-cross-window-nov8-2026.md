# Recall experiment: diagnostics and conclusions across both input windows

Baseline: `aa22132`, branch `memory-improvements-nov8-2026`.

Reusing the independent diagnostic embedding to search hidden conclusions improves
complete-answer delivery from **30/40 to 32/40 on development**. The exposed
reserved split remains **33/40**. This is a development gain on constructed cases;
it has not produced a reserved-split recall gain.

## The missing combination

Existing retrieval handles two truncation cases separately: a task can hide its
failure text beyond MiniLM's input window, and a stored note can hide its conclusion
beyond the same window. Previously, the independent failure embedding searched
only primary memory vectors, while conclusion search used only the combined task
embedding. Neither view covered a hidden diagnostic matching a hidden conclusion.

The [110-case fixture](../../scripts/eval/memory-cross-window-cases.json) composes
the already exposed diagnostic and long-note fixtures. It uses the same long
notes, one-paragraph ablations, short controls, literal controls and negative
queries, placing each query in the failure field after the existing generic task
prefix. Each split has 40 positives and 15 negatives. These are correlated views
of historical incidents, not independent or blind examples. Original fixtures
remain unchanged.

The [protocol](../../scripts/eval/memory-cross-window-protocol.json) froze the
cases, implementation hypothesis, thresholds, limits and guards before scoring.
Development results, historical development replays and the full unchanged crate
contracts passed before the candidate was scored on the reserved split. Reserved
incidents had already been exposed in earlier campaigns; no blind claim is made.

## Implementation

After a successful independent diagnostic embedding and its original primary-vector
search, retrieval retains that vector locally. Conclusion lookup first supplies
all ordinary conclusion matches as before. If slots remain, it searches conclusion
vectors with the retained diagnostic and appends distinct admissible matches below
all incumbents. Scope, trust, tags, caller visibility and duplicate filtering run
before the result limit. Supplementary errors preserve successful current results.

The original diagnostic inference is reused: there is no extra encoder call,
backfill, stored vector, model revision, download or release asset. The additional
work is at most one conclusion-vector table search when both windows matter and
free slots remain. The ordinary cutoff remains 0.3; the supplementary view does
not receive short-query relaxation. Full pools are unchanged.

## Results

| Hybrid view                | Development before → after | Reserved before → after |
| -------------------------- | -------------------------: | ----------------------: |
| Hidden final paragraph     |                7/10 → 9/10 |             8/10 → 8/10 |
| Single-paragraph overflow  |                7/10 → 7/10 |             8/10 → 8/10 |
| Short-record control       |                6/10 → 6/10 |             7/10 → 7/10 |
| Literal identifier control |              10/10 → 10/10 |           10/10 → 10/10 |
| All positives              |              30/40 → 32/40 |           33/40 → 33/40 |

Recovered development cases are `timezone` and `websocket`, both with a hidden
final paragraph. Success requires complete original claim text in the rendered
packet, rather than merely a candidate or fetch pointer. Lexical delivery remains
28/40 development and 24/40 reserved. No cutoff or fixture was retuned after scoring.

All 30 new negative cases keep identical record IDs and order. They already return
records: development counts range from one to eight, and reserved counts from one
to four. Thus these controls do not establish improved precision or abstention;
most also retain unused slots in which an unwanted new result could be detected.

## Regression evidence

All **1,556 historical policy views** retain exactly the same deterministic
outputs, including the preceding conclusion gains, capacity-infeasible retention
facts, negative identities, scope and packet sizes. The new fixture adds **220
policy views**. No previously delivered fact is lost or ranked lower. The final
limit stays eight memories and the packet budget stays 1,000 tokens.

Historical baselines reuse the preceding committed experiment's selected outputs,
bound to the same baseline source, evaluator executable, model and unchanged input
hashes. Every candidate historical view is replayed afresh. The new fixture is
measured against a separate copy of the baseline executable before code changes.
Timing and timed telemetry are excluded from exact comparisons; this is not a
latency benchmark.

Unit tests cover both hidden windows, incumbent and ordinary-conclusion priority,
full pools, eligibility before supplementary limits, failing diagnostics and the
exact number of inference calls. The changed `verified-code-memory` crate's full
suite, native ONNX reference tests, doc test and clippy check pass. No other crate
is changed. Frontend tests, type checking and formatting also pass.

The [machine-readable report](long-term-memory-cross-window-nov8-2026.json) retains
suite metrics, recovered ranks and packet sizes, registration and source hashes,
negative counts, and deterministic output fingerprints. Existing full-pool and
packet-delivery misses remain outside this append-only improvement.

## Reproduction

Build the Rust `evaluate_memory` example in `verified-code-memory`. Use:

```json
{
  "dataset_file": "scripts/eval/memory-cross-window-cases.json",
  "split": "development",
  "deterministic": true,
  "candidate_limit": 8,
  "memory_budget": 1000,
  "short_query_words": 2,
  "short_query_scale": 0.9166666666666666
}
```

Pass it through `--experiment-config <path>`. For hybrid retrieval set
`ROVIK_MEMORY_EVAL_MODEL` to the pinned local ONNX directory,
`ROVIK_MEMORY_EVAL_DIMENSIONS=384`,
`ROVIK_MEMORY_EVAL_REVISION=chroma-minilm-l6-v2-913d7300-title-claim-native-v1`,
and `ROVIK_MEMORY_EVAL_THRESHOLD=0.3`. Unset those overrides for lexical retrieval.
Run the same fixture and configuration against baseline `aa22132`.
