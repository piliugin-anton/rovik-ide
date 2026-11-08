# Experiment review and focused symbol recall

Baseline: `e62a3ba`, branch `memory-improvements-nov8-2026`.

The review led to a narrower retry of the previously rejected full-pool symbol
experiment. On the **same existing cases**, complex-identifier hybrid delivery
improves from **24/28 to 28/28 development** and **26/28 to 28/28 reserved**.
The denser full-pool suite improves **16/28 to 22/28 development** and **22/28 to
24/28 reserved**. No benchmark cases were added or changed. Previous long-note
and combined-window results remain unchanged.

## Review of the earlier experiments

| Family                                                                                        | Earlier evidence                                                                                                               | Direction for this round                                                                            |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| Word-vector means, IDF, common-component removal, character ngrams, weighted/title-only views | Only the initial mean-vector development result added an answer; title-only was unsupported by the old word model.             | Keep MiniLM. Do not assume an old word-model failure proves a native transformer view cannot work.  |
| Manual contextual keys, semantic gates, semantic-only ablations and task-state removal        | Grounded context helped, but manual aliases and false injections limited the result; removing provided files regressed recall. | Preserve observed context and exact signals; do not author benchmark aliases.                       |
| WordNet concepts                                                                              | First/all-sense expansions supplied no safe additional answer.                                                                 | Avoid broad automatic vocabulary expansion without new evidence.                                    |
| MiniLM title/claim order, claim-first and averaged representations, cosine lead gaps          | Gains coupled to ranking or false-match regressions. The short distinctive-query scale was the safe exception.                 | Keep the model, primary vectors and cutoffs; consider terse requests separately.                    |
| Exact deduplication, admissible candidate pools and packing after oversized claims            | Fixed distinct availability/delivery failures while preserving original claims.                                                | Measure requested facts in the actual packet, not only candidate counts.                            |
| Complete cancellable indexing, independent clone jobs and source/file context                 | Fixed missing availability without fabricating relevance.                                                                      | Preserve lifecycle, trust and visibility protections.                                               |
| Compound-name fallback, snake names and free-slot hybrid supplements                          | Complete name components recover prose claims without broad aliases.                                                           | Complete identifier evidence remains useful when generic context dominates.                         |
| Unconditional tail replacement, wider pools, symbol RRF and MMR                               | Some regained answers, but requested retention facts or negative identities regressed.                                         | A broad full-pool replacement rule is unsafe. Separate terse focused lookups from broader requests. |
| Hidden diagnostics, explicit concluding paragraphs and their combination                      | Safe gains in free slots. Arbitrary paragraph suffixes failed a legacy context contract.                                       | Preserve those gains and require explicit scope guards for a different full-pool policy.            |

The source reports are the [first experiments](long-term-memory-experiments-nov7-2026.md),
[proposal follow-up](long-term-memory-proposal-followup-nov7-2026.md),
[complete rerun](long-term-memory-postimplementation-rerun-nov7-2026.md),
[semantic concepts](long-term-memory-semantic-followup-nov7-2026.md),
[MiniLM](long-term-memory-minilm-nov8-2026.md),
[recall iterations](long-term-memory-recall-iterations-nov8-2026.md),
[capacity gains](long-term-memory-capacity-gains-nov7-2026.md),
[safe source-context changes](long-term-memory-safe-improvements-nov7-2026.md),
[index completion](long-term-memory-index-completion-nov8-2026.md),
[structured queries](long-term-memory-structured-queries-nov8-2026.md),
[complex identifiers](long-term-memory-complex-identifiers-nov8-2026.md),
[snake identifiers](long-term-memory-snake-identifiers-nov8-2026.md),
[hybrid anchors](long-term-memory-hybrid-anchors-nov8-2026.md),
[full-pool retries](long-term-memory-fullpool-retries-nov8-2026.md),
[conclusions](long-term-memory-overflow-conclusions-nov8-2026.md), and
[combined windows](long-term-memory-cross-window-nov8-2026.md).

## First attempt this round: query rephrasing

The [registered query-view trial](../../scripts/eval/memory-query-view-protocol.json)
added one grammatical view of a terse request, at the unchanged normal cutoff,
after incumbent results. Fixed variants were `How to {task}?`, `Problem: {task}`,
and `Solution for {task}`. They applied only to short requests with free slots,
no supplied file/failure context, no exact incumbent title and no encoder overflow.

All three reproduce **65/70 original development** and **27/30 fresh development**
delivery. None adds an expected answer. The question variant changes one positive
returned pool and the solution variant changes three without helping delivery;
negative IDs remain unchanged. The extra inference is therefore unjustified.
All three were removed, and none was scored on the reserved split.

## Selected retry: a narrowly focused full-pool request

The [focused protocol](../../scripts/eval/memory-focused-symbol-protocol.json) was
registered after rejecting the query views and before scoring this policy. It
uses the unchanged fixtures and thresholds.

A full hybrid pool can reserve its last slot for a complete named fact only when:

- The request has one or two whitespace words and exactly one complete compound
  identifier clause.
- There is no supplied file or failure, and no exact incumbent task/failure title.
- The request contains none of the registered collection words, numeric characters,
  or wildcard/question markers. Collection checks recognize punctuation boundaries.
- The result limit is greater than one, so the leading result is never displaced.
- No eligible complete match is already represented among the incumbent results.

The first results keep their order and scores. Only the last record is replaced,
by the highest-ranked eligible complete match. The replacement stays last and
uses the existing scope, trust, tag, caller-visibility and exact-copy rules. This
changes hybrid retrieval only; lexical retrieval and strict vector search retain
their previous behavior. The model, representations, cutoff and stored history do
not change. No inference, model download or new vector is added.

This is deliberately a ranking change for a restricted class of requests. The
regression evidence protects the requested facts in these fixtures; it does not
prove that every displaced tail is useless in every real task. The incidents
are exposed, correlated source-bound views rather than independent blind or
live-agent evaluations.

## Guard probes before reserved scoring

Two unit probes caught flaws in the initial prototype, both fixed before the
first reserved candidate evaluation:

1. Whitespace-only collection checking missed `all_crowding` and `all-crowding`.
   The final check recognizes registered collection words across punctuation.
2. A supplied complete match can rank below the supplementary pool's first 80
   rows. Checking presence only in that bounded pool could replace an already
   supplied fact. The final implementation checks incumbents in a separately
   filtered scan before selecting a new candidate, so the pool cap cannot hide it.

Both probes were reproduced failing before their fixes. Tests additionally cover
wildcards, narrative requests, explicit files/failures, one-slot lookups, ordinary
numeric-context fallback, partial names, caller exclusions, verification, tags,
fully indexed foreign copies and deprecated records. Existing tests are unchanged.

## Results on unchanged suites

| Existing hybrid suite      | Development before → after | Reserved before → after |
| -------------------------- | -------------------------: | ----------------------: |
| Complex identifiers        |              24/28 → 28/28 |           26/28 → 28/28 |
| Denser full-pool cases     |              16/28 → 22/28 |           22/28 → 24/28 |
| Original                   |              65/70 → 65/70 |           26/30 → 26/30 |
| Fresh semantic             |              27/30 → 27/30 |           28/30 → 28/30 |
| Long notes                 |              33/40 → 33/40 |           34/40 → 34/40 |
| Combined task/note windows |              32/40 → 32/40 |           33/40 → 33/40 |

The 14 gains are correlated views of six source incidents. Development gains
concern `SharedMemoryStore`, `RetrievalQuery`, `MemoryRecord` and `WorkloadStatus`;
reserved gains concern `CommandOutput` and `ProcessTree`. Each recovered claim
reaches the actual packet at rank eight. Dense three-fact and some explicit-file
misses remain; no difficult category was removed to obtain a higher score.

All **1,776 existing policy views** were replayed. Exactly **14 changed**, all
complete-answer gains; **1,762 remain identical** after excluding timing fields.
Every previously delivered primary and required fact retains its delivery and
rank. All negative IDs and order are unchanged. Both capacity-infeasible retention
suites preserve their entire deterministic outputs, including each previously
supplied observation. These controls are excluded from headline complete-answer
recall because ten requested facts cannot fit eight slots.

The final result limit stays eight and the packet allowance stays 1,000 tokens.
No scope, trust, tag, caller-visibility or budget violation occurred. This does
not establish improved precision: existing false injections remain unchanged.
The selected policy was checked on all development suites and unchanged crate
contracts before reserved evaluation. No policy or cutoff was retuned from
reserved outcomes.

## Validation and reproduction

The changed `verified-code-memory` crate passes **149 all-target tests**, both
explicitly enabled native ONNX tests, its doctest and clippy with warnings denied.
All **1,046 frontend tests**, type checking and formatting pass. No other Rust
crate changed, and crate tests were not run together.

The [machine-readable report](long-term-memory-focused-symbols-nov8-2026.json)
contains the review, discarded query-view outcomes, frozen inputs, selected
metrics, every changed case with before/after identities, retention comparisons,
source/executable hashes, and deterministic output fingerprints. Historical
baselines reuse the preceding selected runtime's outputs with verified source,
executable and input identity. Candidate views are replayed afresh. Concurrent
compilation and checks mean the timings are not a production latency benchmark.

Build the existing Rust `evaluate_memory` example. Keep the pinned MiniLM model,
384 dimensions, revision `chroma-minilm-l6-v2-913d7300-title-claim-native-v1`, normal
cutoff 0.3, short-query words 2 and scale 11/12. Use the same deterministic split,
result limit eight and memory allowance 1,000 against baseline `e62a3ba` and the
candidate. Select the unchanged `memory-complex-identifier-cases.json` or
`memory-fullpool-retry-cases.json`; run both retention fixtures separately using
per-fact ranks. The protocol lists every unchanged regression fixture and hash.
