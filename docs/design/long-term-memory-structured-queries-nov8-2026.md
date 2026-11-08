# New recall approaches: structured queries

Baseline: `a7362af`, branch `memory-improvements-nov8-2026`.
The [protocol](../../scripts/eval/memory-structured-query-protocol.json) prepares
two approaches not used in the earlier representation, threshold, packing or
index-completion campaigns: an independent diagnostic query and a conjunctive
identifier fallback. Model weights, download assets and cutoffs stay unchanged.

## Independent diagnostic view

The native encoder sees the first 500 characters of `task + newline + failure`.
A sufficiently long task hides the entire diagnostic from semantic search, even
though lexical retrieval already sees it. When that happens and the incumbent
result has a free slot, the retriever now embeds the failure separately and
appends distinct admitted claims. Existing results and ranks retain priority.
Scope, verification, excluded tags, caller visibility and duplicate filters apply
before the supplementary limit. A failed supplementary inference preserves the
successful incumbent result. Other providers keep their existing behavior unless
they declare a positive character limit through the default-compatible trait.

The evaluator now passes fixture failure text to the actual retrieval query;
a regression test verifies it changes retrieval. It also records returned IDs so
negative checks compare record identity, rather than just counts. Both baseline
and candidate use this same corrected evaluation path. Native inference, query
and document preprocessing, index revision and stored vectors are unchanged.

| Constructed diagnostic split | Baseline supplied | Candidate supplied |
| ---------------------------- | ----------------- | ------------------ |
| Development                  | 24/30             | 27/30              |
| Exposed heldout              | 24/30             | 28/30              |

All 30 negative cases retain exactly the same returned IDs and counts. **They
already return 1–4 records each**: the negative injection rate is 100% for both
policies. This is an existing precision problem, not a precision gain, and these
cases still have free slots in which a regression could be detected. No answer
was lost or ranked lower. Project scope and the 1,000-token allowance hold.
The 258 existing cases are additionally checked under both lexical and hybrid
policies (516 replays); their outcomes remain unchanged.

The new diagnostic cases are constructed views of the previously exposed fresh
corpus: a repeated generic task exceeds 500 characters, and the old task becomes
the failure. This isolates the hidden-input mechanism. It is not a fresh blind
semantic benchmark, a realistic distribution of long prompts, or live-agent
success. The existing no-match failures remain a rollout limitation; hybrid
retrieval remains opt-in.

Reproduce using the native `evaluate_memory` Rust example with the pinned model,
384 dimensions, the existing model revision and threshold 0.3. Supply
`dataset_file: "scripts/eval/memory-diagnostic-cases.json"`, `deterministic: true`,
`memory_budget: 1000`, `short_query_words: 2`, `short_query_scale: 0.9166666666666666`
and `split: "development"` (then `"heldout"`) through `--experiment-config`.
Paths are relative to the process working directory. The
[machine-readable report](long-term-memory-structured-queries-nov8-2026.json)
records case changes, fingerprints, the negative record-ID audit and checks.

## Conjunctive identifier fallback

The second registered experiment uses source-grounded views of eight disjoint
symbols, four per split, with prose, qualified names, adjacent unknown names and
generic CamelCase controls. The claims are synthetic summaries of the source at
the protocol's baseline commit; source references bind each to that revision.
This experiment was run separately against the committed diagnostic improvement
(`04c29e3`). The fallback splits ASCII CamelCase/PascalCase and acronym boundaries,
requires all distinct components with FTS `AND`, and rejects names containing
generic words, components shorter than three letters, digits or underscores.
It accepts at most four distinct names, eight components per name and 128 bytes
per name. Ordinary lexical or explicit-file results prevent the fallback from
running. The existing FTS collector applies scope, trust, tags and caller
visibility before its result limit; no new index or stored aliases are needed.

| Identifier split / policy | Baseline supplied | Candidate supplied |
| ------------------------- | ----------------- | ------------------ |
| Development / lexical     | 8/16              | 16/16              |
| Heldout / lexical         | 8/16              | 16/16              |
| Development / hybrid      | 16/16             | 16/16              |
| Heldout / hybrid          | 14/16             | 16/16              |

All 16 negative cases retain exactly the same record IDs. Lexical retrieval
abstains on all of them before and after. Hybrid still injects on 3/8 development
and 4/8 heldout negatives: this change preserves, rather than fixes, the model's
existing false matches. No delivered answer is lost or ranked lower. The 258
incumbent cases under both policies and the 90 diagnostic hybrid cases are
checked again against the first improvement. The second improvement preserves
the first one's gains.

Use `dataset_file: "scripts/eval/memory-identifier-cases.json"` with the same
evaluator configuration to reproduce this trial. Omit all `ROVIK_MEMORY_EVAL_*`
environment variables for lexical retrieval; provide the pinned native model
for hybrid. These are constructed source-symbol/prose views, not independent
human-rated coding results or a corpus-wide recall estimate.
