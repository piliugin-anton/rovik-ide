# Oversized alternatives in focused title recall

Baseline: `457a5d0c2ae741a486968f773295eb92d6b75680`.
Branch: `ltm-improvements-nov9-2026-2` (the requested branch already existed).

The new harder title cases improve from **4/32 to 18/32 development** and
**10/32 to 19/32 reserved**, separately at both the 1,000-token evaluation
allowance and the application's existing 2,000-token allowance. These are
complete answers: the primary and every required claim must be delivered in
full. The previous suites and their denominators remain separate.

## Extension and prior work

The [title-coverage experiment](long-term-memory-title-coverage-nov9-2026.md)
reached 32/32 on each split, but its corpus did not contain competing oversized
canonical-title claims. The new
[fixture](../../scripts/eval/memory-title-fit-cases.json) extends its 104 cases
with eight alternate title spellings and distinct verbose claims. Each claim is
approximately 14,000–17,000 characters, so its text alone exceeds either packet
profile. Repeated symbol references make these strong complete-name candidates;
their opening archive context differs from the concise required source finding.

The original required source facts, request structures and filtering controls
are retained. Cases include pairs, three/four names, qualified/snake spellings,
duplicate names, explicit reference/literal context and file requirements.
Each split has 32 answerable and 20 negative/control cases. The
[retention extension](../../scripts/eval/memory-title-fit-retention-cases.json)
adds the same alternatives to 16 prior controls: four feasible five-fact answers
and four capacity-infeasible 15-fact answers per split. The latter exceed eight
slots and remain excluded from headline recall.

The [protocol](../../scripts/eval/memory-title-fit-protocol.json) froze both
fixtures before candidate scoring. Hashes use `JSON.stringify(parsed, null, 2)`
plus a newline. These are synthetic stress cases built from eight already exposed
source incidents, not independent semantic or real-agent validation.

The [focused-symbol review](long-term-memory-focused-symbols-nov8-2026.md)
previously rejected unconditional tail replacement, wider pools and diversification
after context or negative regressions. This experiment keeps the focused scope,
existing incumbents and packet-preservation rules. It addresses an availability
problem inside optional title collection rather than changing global ranking.

## Selected change

Optional title lookup previously chose its highest-ranked eligible whole-title
candidate and then checked the full plan's rendering. An oversized candidate
could make that plan fail even when another eligible claim about the same symbol
would fit.

The selected change adds one necessary condition before the existing bounded
collector: `estimate_tokens(record.content) <= packet_budget`. A claim violating
that condition can never render in full under the supplied allowance, regardless
of its metadata or other records. Rejecting it early prevents such candidates
from filling the retained pool and permits later eligible title claims to be
considered. It does not truncate or summarize any stored claim.

This is a lower bound, not a claim that an admitted record will fit. The existing
full-plan rendering preview still requires every optional title to be complete
and every previously complete protected name/file/context claim to remain complete.
Any rendering, capacity or lookup failure keeps the legacy plan. The condition
applies only to optional focused title lookup with a caller-supplied allowance;
it does not filter ordinary lexical/vector results or replace oversized incumbents.
Repository, trust, tag and caller filtering remain in the existing indexed path.
No model, inference, cutoff, result limit, schema or release asset changed.

## Complete-answer results

| New hybrid cohort                        | Allowance | Development before → after | Reserved before → after |
| ---------------------------------------- | --------: | -------------------------: | ----------------------: |
| Oversized title alternatives             |     1,000 |               4/32 → 18/32 |           10/32 → 19/32 |
| Same title cases, application profile    |     2,000 |               4/32 → 18/32 |           10/32 → 19/32 |
| Feasible long-context/file retention     |     1,000 |                  0/4 → 2/4 |               0/4 → 4/4 |
| Same feasible cases, application profile |     2,000 |                  0/4 → 4/4 |               0/4 → 4/4 |
| Capacity-infeasible controls             |      Each |                  0/4 → 0/4 |               0/4 → 0/4 |

The remaining misses are retained. In particular, an oversized canonical-title
record already in the incumbent pool can still represent that name and occupy
a protected position. This change deliberately leaves that behavior intact.
Combined rendering pressure can also block an otherwise eligible plan.

The [machine report](long-term-memory-title-fit-nov9-2026.json) separates all
cohorts, budgets and policies and records per-case delivery changes. The repeated
budget profiles are correlated views of the same cases, not additional incidents.

## Regression comparison and validation

Native evaluation keeps the pinned GitHub MiniLM model, 384 dimensions, threshold
0.30, first 500 title/claim characters, at most 256 tokens, short-query scale
11/12, eight records and the stable 36-hour fixture age. Budgets use the existing
four-characters-per-token estimate. The unchanged
[comparator](../../scripts/eval/memory-regressions.mjs) checks each primary and
required claim's identity/hash, complete delivery and protected ranks, exact
negative returned/rendered/full ID order, caller/scope/trust/tag restrictions,
embedding fallback and declared packet/result bounds.

Historical baselines are byte-for-byte copies of the previous accepted final
native results. New baselines use an immutable copy of the `457a5d0` evaluator.
Development gains selected the candidate before reserved candidate scoring;
historical and reserved replays did not retune the candidate or expected facts.
For reproduction, use the evaluator configuration described in the
[preceding report](long-term-memory-title-coverage-nov9-2026.md), substituting
the new dataset paths and keeping the two budgets separate. Compare saved outputs
with `node scripts/eval/memory-regressions.mjs baseline.json candidate.json`,
appending `2000` for the application profile.

The native evaluator compiled successfully. The existing Rust pressure/visibility
contract was extended with 90 visible oversized title alternatives, beyond the
collector's retained bound, to exercise early rejection alongside the existing
rollback checks. This focused Rust test **passed** after the user specifically
requested executing oversized-alternative coverage. Other crate/frontend tests,
clippy and the final format check remain skipped; changed files were formatted.
Native recall evaluation and saved-result auditing provide the corpus-level
validation in this iteration.

Unchanged negative packets do not establish good abstention. Some control
requests deliberately include known symbols and still retrieve records. No new
paired real-agent tasks or latency benchmark were collected, and the separate
release-quality gate remains unchanged. Preservation claims apply to the replayed
evaluation corpus.

## Full oversized-alternative stress run

At the user's request, the native evaluation also expands **every** oversized
source alternative to 90 distinct archive records, for **720 oversized records
per corpus**. It runs all 104 title cases and 16 retention cases, both splits,
lexical and hybrid policies, and both packet allowances. This adds 480 compared
policy views; results are recorded separately from the smaller extension.
The runtime candidate and expected facts remain unchanged throughout this run.

| Full 720-record hybrid stress cohort     | Allowance | Development before → after | Reserved before → after |
| ---------------------------------------- | --------: | -------------------------: | ----------------------: |
| Title alternatives                       |     1,000 |               3/32 → 18/32 |            8/32 → 17/32 |
| Same title cases, application profile    |     2,000 |               3/32 → 18/32 |            8/32 → 17/32 |
| Feasible context/file retention          |     1,000 |                  0/4 → 2/4 |               0/4 → 4/4 |
| Same feasible cases, application profile |     2,000 |                  0/4 → 4/4 |               0/4 → 4/4 |
| Capacity-infeasible controls             |      Each |                  0/4 → 0/4 |               0/4 → 0/4 |

All **4,148 compared views** pass the per-record regression checks: 3,188
previously accepted views, 480 views of the eight-alternative extension and 480
views of the full 720-record stress corpus. All 3,188 historical rows are
identical except for latency/telemetry; all 1,284 negative views preserve their
returned/rendered/complete IDs and order. The stress corpus contributes 62
improved complete-answer views, including repeated budgets and correlated cases.

Lower starting scores describe a harder memory corpus, with the previous
improvements already present in the baseline. The original title suite remains
32/32 in both splits. The eight-alternative and 720-record corpora are separate
extensions; the latter exposes additional capacity weaknesses, including the
remaining 15 reserved misses. Lexical-only rows and retention controls are also
separate from hybrid title recall. A zero complete-answer score can coexist with
retrieved memories when any required fact is missing. No policy was retuned from
these reserved or stress outcomes.

To reproduce the inputs, load each new fixture, replace each corpus record tagged
`oversized-archive` with the following expansion, and serialize the resulting
dataset with `JSON.stringify(data)` and no trailing newline:

```js
Array.from({ length: 90 }, (_, n) => ({
  ...record,
  content: record.content + `\nArchive copy ${n}.`,
}));
```

Other records and all cases remain unchanged. The machine report includes the
source and expanded input hashes. Each alternative retains its complete claim
and a distinct claim identity, so the existing deduplication rules cannot collapse
the large pool into one record.

The evaluator now keys its existing in-process embedding cache by the encoder's
declared effective character input. MiniLM truncates to 500 characters before
tokenization, so archive copies with identical prefixes can reuse exactly the
same vector. Providers without a positive declared limit retain full-text keys.
Every record remains independently stored and indexed. Normal eligibility,
candidate limits, eight-result retrieval and packet rendering still apply.
Both baseline and candidate use this cache, and its Unicode boundary contract
passed. Completed uncached profiles are also compared for output parity,
excluding latency and telemetry: all **180 views across six profiles** match
exactly, including ranks and full delivery. This is an evaluation-only optimization;
application encoding and stored-vector lifecycle are unchanged.
