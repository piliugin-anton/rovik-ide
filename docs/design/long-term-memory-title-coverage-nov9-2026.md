# Budget-aware symbol-title coverage and delivery evidence

Baseline: `dec529871065afdf30ee3dd5de05fdc156de896c`.
Branch: `ltm-improvements-nov9-2026-2`.

The unchanged harder file-coverage reserved split improves from **30/32 to
32/32**. Its development split remains **32/32**. The new, harder title-coverage
cases improve from **18/32 to 32/32** in development and **22/32 to 32/32** on
the exposed reserved split. These count complete answers, including every
additional required fact, rather than just retrieving the primary record.

All **3,188 policy views** pass the strengthened per-record comparison: 2,708
historical views, 240 new views at the established 1,000-token allowance, and 240
views of those same new cases at the application's 2,000-token allowance. These
are correlated policy/budget replays, not 3,188 independent incidents.

## Extending saturated cases

The [frozen protocol](../../scripts/eval/memory-title-coverage-protocol.json)
extends the eight source incidents used by the prior file-coverage suite. The
[title fixture](../../scripts/eval/memory-title-coverage-cases.json) adds 104
cases: 32 answerable and 20 negative/control cases per split. It combines pairs,
three/four names, qualified and snake spellings, duplicate names, literal context,
and file hints. A broad cross-reference claim mentions all symbol components and
can satisfy the old complete-name predicate without delivering the actual named
fact. Longer observations crowd the pool. Hidden, provisional, foreign and
caller-denied title alternatives exercise filtering; wrong suffixes and broader
requests exercise existing scope boundaries.

The [retention fixture](../../scripts/eval/memory-title-coverage-retention-cases.json)
adds 16 cases. Each split has four feasible five-fact answers requiring named
facts, longer cross-reference context and two different file claims sharing the
title `File context`. Four further cases require 15 facts despite the eight-record
ceiling. Those capacity controls remain incomplete and are excluded from headline
recall. Feasible cases and packet pressure are reported separately below. The
fixture's legacy description calls the latter controls “all-file”; their actual
requirements retain all crowding context, rather than associating every record
with a supplied file.

Cases and required claims were frozen before candidate scoring. Protocol hashes
use `JSON.stringify(parsed, null, 2)` plus a newline so formatter whitespace does
not change the registered inputs. Baseline and candidate share the new evidence
fields and explicit packet allowance. Baseline retrieval is unchanged: all
2,708 historical rows match the previous accepted outputs in every legacy field,
excluding timing and telemetry. The same stable 36-hour fixture age is used.

## Selected policy and prior experiments

The [focused-symbol review](long-term-memory-focused-symbols-nov8-2026.md)
rejected unconditional replacement, wider pools and diversification after context
or negative regressions. The subsequent name/file planners protected explicit
incumbents, but a prose reference containing every name component could still
prevent a more specific title claim from entering. This experiment retains the
existing focused scope and exact legacy name/file plan, then tries optional
title coverage in remaining unprotected positions.

The title's complete canonical identifier clause must equal the requested name's
clause. Case, component order and snake/space spelling can normalize together;
an extra title component or different short suffix fails. This is whole-clause
component matching, not literal title-string equality or an alias table. Eligible
title lookup reuses the indexed collector with repository, trust, tag and caller
filters applied before its bound.

The caller must supply `RetrievalQuery::with_packet_budget(tokens)`. The planner
renders the exact legacy result first, remembering complete protected incumbent
claims and complete name/file additions. It previews the optional title plan
under the same allowance. Every new title must fit as a complete claim, and every
previously complete protected claim must remain complete. Otherwise it keeps
the legacy plan. Original slot scores and protected ranks are preserved.
Automatic application recall supplies its existing 2,000-token allowance;
callers omitting it retain previous behavior.

No model, vector representation, cutoff, schema or release asset changes are
needed. Additional indexed lookups and two packet previews occur only within
this optional focused planning path; there is no additional model inference.
Unrequested trailing observations can still change, as with the existing focused
planner. The measured protections do not prove universal relevance.

## Complete-answer results

| Hybrid cohort                               | Packet allowance | Development before → after | Reserved before → after |
| ------------------------------------------- | ---------------: | -------------------------: | ----------------------: |
| Historical harder file coverage             |            1,000 |              32/32 → 32/32 |           30/32 → 32/32 |
| New title coverage                          |            1,000 |              18/32 → 32/32 |           22/32 → 32/32 |
| Same new title cases, application allowance |            2,000 |              18/32 → 32/32 |           22/32 → 32/32 |
| New feasible context/file retention         |            1,000 |                  0/4 → 2/4 |               0/4 → 4/4 |
| Same feasible cases, application allowance  |            2,000 |                  0/4 → 4/4 |               0/4 → 4/4 |
| New capacity-infeasible controls            |   Each allowance |                  0/4 → 0/4 |               0/4 → 0/4 |

The two historical gains are `file-cover-MemoryState-four` and
`file-cover-RetrievedMemory-filtered`. Other historical complete-answer counts
are unchanged. Lexical results remain unchanged. The two remaining feasible
development misses at 1,000 tokens stay visible; increasing to the application's
existing allowance is a separate profile, not a replacement of that denominator.

The [machine report](long-term-memory-title-coverage-nov9-2026.json) records
all cohort metrics, exact fact/packet changes, artifact hashes and individual
comparison outcomes. There are 64 improved policy views across both budgets,
including repeated cases; this is not a claim of 64 independent gains.

## Regression review and stronger evidence

The previous checks were insufficient to establish per-fact delivery:

1. A map keyed by title overwrote ranks for different claims sharing that title.
2. Stable ranks and aggregate delivered counts could hide a lost required fact
   in an already incomplete answer.
3. `render_packet_traced` includes fetch hints. Searching packet text could mark
   a hinted record delivered when another complete record had identical content.
4. Comparing negative returned IDs alone missed changes from full text to hints.

`MemoryPacket` now exposes complete claim IDs separately from hint-inclusive
rendered IDs. Existing rendering text and `render_packet_traced` behavior stay
compatible. The evaluator records primary identity/hash plus each required
record's identity, hash, rank and complete delivery. Legacy title-keyed ranks
remain only for compatibility.

The committed [comparator](../../scripts/eval/memory-regressions.mjs) rejects
missing/malformed evidence, duplicate cases, changed claim identities, every lost
delivered primary/required claim, worse delivered-primary or retrieved-required
rank, and complete-answer loss. Negative returned, rendered and complete IDs must
retain their exact order. It also checks eight-record and declared token bounds,
scope/trust/tag/caller violations and embedding fallback. All 964 negative policy
views retain their packet identities. Required facts remain protected even when
an answer was already incomplete.

Mutation checks cover same-title fact swaps with unchanged counts/ranks, negative
full-to-hint changes with unchanged returned IDs, missing evidence, changed hashes,
scope violations and overruns. They are included in the existing `test:eval` CI
job. Rust contracts cover identical-content full/hint ambiguity and a deterministic
filtered title lookup beyond rejected alternatives. A pressure control proves
that losing a previously complete file claim rolls the optional plan back; this
new lookup/pressure contract passed 100 repeated runs.

These checks establish preservation on this corpus, not general precision.
Some negative requests deliberately name known symbols and still return records;
the new title cohort's no-match injection metric remains 100%. Freezing those
packets does not make that a successful abstention result. The separate release
quality gate remains unchanged: at least 90% reserved recall, at most 5% negative
injection, and at least 30 paired real-agent tasks with a ten-point improvement
and no irrelevant-control regression. No new paired-agent evidence was collected.

## Evaluation and validation status

Native evaluation uses the pinned GitHub MiniLM model: 384 dimensions, threshold
0.30, first 500 title/claim characters, at most 256 tokens, two-word scale 11/12,
eight records and the existing four-characters-per-token estimate. It is not a
model-tokenizer context measurement. Development selection and the historical
development replay preceded the final exposed reserved replay. No policy or
fixture was retuned from that reserved replay.

For a native run, set `ROVIK_MEMORY_EVAL_MODEL`, `ROVIK_MEMORY_EVAL_DIMENSIONS`,
`ROVIK_MEMORY_EVAL_REVISION` and `ROVIK_MEMORY_EVAL_THRESHOLD`. The
`evaluate_memory --experiment-config` JSON supplies `dataset_file`, `split`,
`deterministic: true`, `memory_budget`, `short_query_words: 2` and
`short_query_scale: 0.9166666666666666`. Run each fixture/policy/split separately,
then compare saved native outputs:

```sh
node scripts/eval/memory-regressions.mjs baseline.json candidate.json
node scripts/eval/memory-regressions.mjs baseline-app.json candidate-app.json 2000
```

Before the user's instruction to skip further tests, the memory crate's 165
all-target tests passed (two ignored), along with two native-model tests, one
doc test, clippy with warnings denied, 1,066 frontend tests, typecheck, 67 eval
tests and four dedicated comparator tests. Rust crates were run separately.
The final new pressure/visibility contract also passed its 100 repeats. Later
changes only formatted code and added documentation/report artifacts.

The core integration compiled. Outside the outer sandbox its full suite reached
2,606 passed, two environmental failures and 29 ignored. The failures involved
an unrelated `/usr/bin/go` shim and Chrome's temporary socket directory; both
isolated controls passed using real Go and `TMPDIR=/tmp`. The corrected full
rerun was stopped on the user's instruction to **skip the tests**. No clean full
core pass is claimed. Core clippy and the final format check were not run;
changed files were formatted. Recall evaluations and saved-result auditing
completed after stopping those tests.
