# Packet-feasible new name additions

Baseline: `5899a3e4638c77b722081aef6a8c9ddd4b8a0ab4`.
Branch: `ltm-improvements-nov10-2026`.

The [diagnostic step](long-term-memory-diagnostics-nov10-2026.md) found that
four-name requests were selecting oversized claims as new name additions. The
existing optional title pass then lacked enough free slots for the four compact
source facts. The final native comparison improves the existing
oversized-title cohort **28/32 → 32/32**, separately at 1,000 and 2,000 estimated
tokens. No fixture, expected fact, threshold or result limit changes.

## Selected policy

Keep the exact existing name/file/title plan first. Retry only if that plan
introduces individually oversized new name claims and its packet still lacks
complete coverage of the requested canonical titles. The retry can prefer a
fitting alternative only when the old new-name candidate has a whole canonical
title matching the requested symbol. A body-only mention does not qualify.

The existing indexed collector applies repository, verification, excluded tags,
caller visibility and duplicate filtering. Whole canonical title matching and
`estimate_tokens(content) <= packet_budget` run before its retained bound, so
many oversized alternatives cannot conceal a compact eligible claim. Without an
alternative, the original candidate remains available to the legacy plan.

Accept the retry only if:

1. Every oversized new name addition selected by the legacy plan is replaced
   together; a missing fitting alternative rolls the entire retry back.
2. Every replacement has a requested whole canonical title and renders in full.
3. Every other legacy record retains its ID, context claim and exact position.
4. Every previously complete claim remains complete under the same allowance.

The packet preview includes real metadata and evidence. Individual body size is
only a necessary bound, not proof that the combined packet fits. Any lookup,
capacity or rendering failure keeps the exact legacy plan. A legacy packet that
already fully supplies all requested canonical titles skips the retry.

The change never removes an original incumbent. It can replace the selected
fetch pointers of newly planned oversized alternatives, while preserving their
stored records and history. All other legacy identities, positions and complete
claims stay protected, including short additions and supplied-file/literal
anchors. Callers without a packet allowance keep their earlier behavior.

This adds bounded collector work and packet previews, with no new inference,
model file, vector, schema, claim rewrite, truncation or broader query scope.
Automatic hybrid recall already supplies its 2,000-token allowance. Manual
callers omitting the allowance retain their existing behavior.

## Review and focused contracts

The [protocol](../../scripts/eval/memory-name-fit-protocol.json) records the
initial hypothesis and unchanged inputs. Development review made two safety
refinements before reserved scoring. A missing-alternative test caught partial
replacement in the initial prototype; the final retry is atomic. The final guard
also checks `same_context_claim` for unchanged IDs, preserving content, evidence
and trust context as well as identity and position.

The two new contracts passed ten separate processes with fresh UUIDs. They exercise four-name recovery with 90 competing archive claims
for one name, filtering before the pool bound, provisional/tagged/foreign/caller
exclusions, exact preservation of retained records and rank scores, full delivery,
a partial-render boundary that must roll back, and a missing fitting name that
must preserve the entire legacy plan.

Repeated runs exposed an ordering precondition in the new test fixture: equal
semantic scores could omit its literal context or make it the leader, changing
legacy optional-title capacity. Strict vector and lexical ordering now retains a
leading observation and a separate literal anchor. The original assertions are
retained, with explicit precondition checks; the production code was not changed
to accommodate that fixture race.

The existing 14 focused-symbol tests, three full-subject contracts and 19 native
evaluator tests pass, alongside the two new contracts. Library and all focused name-fit/symbol/full-subject/evaluator clippy targets
pass with warnings denied. The check exposed identical branches in the existing
full-subject test model; equal-return conditions were combined without changing
vectors or assertions, and its three contracts passed again. Broader crate/core/frontend suites remain
skipped under the user's instruction. Changed Rust files are formatted.

## Final comparison

All **5,700 policy views** pass the unchanged per-record comparator, including
**1,988 negative views** and every historical 720-record oversized-alternative
corpus. There are **16 complete-answer gains** and exactly 16 changed case views;
the other **5,684** retain every non-timing field, including non-timing telemetry.
All **2,850 lexical views** are unchanged. No earlier partial replay is used as a
final result after the guard refinement.

| Hybrid cohort                         | Allowance | Development before → after | Reserved before → after |
| ------------------------------------- | --------: | -------------------------: | ----------------------: |
| Existing oversized title alternatives |     1,000 |              28/32 → 32/32 |           32/32 → 32/32 |
| Same cases, application allowance     |     2,000 |              28/32 → 32/32 |           32/32 → 32/32 |
| Full 720-record title stress          |     1,000 |              28/32 → 32/32 |           32/32 → 32/32 |
| Same stress, application allowance    |     2,000 |              28/32 → 32/32 |           32/32 → 32/32 |

The previously missing four-name subset improves **0/4 → 4/4** in each development
profile. Every primary and additional required source claim reaches the actual
packet in full. Already perfect symbol/file/context groups remain perfect;
original/fresh implicit scores and infeasible controls remain unchanged. No
negative returned/rendered/full identity or order, required delivery/rank,
filter, fallback or packet/result bound regresses. The
[machine report](long-term-memory-name-fit-nov10-2026.json) records all profiles,
individual changed cases, input/output hashes, the frozen runtime and review notes.

The matrix retains all 5,620 preceding views plus the 80 lexical/hybrid views of
the existing cause-field cases. The latter add no cases this turn: their runtime
was restored in the preceding campaign. Baselines are copied byte-for-byte from
accepted native outputs. Development selection and all development comparisons
precede candidate reserved scoring. The frozen source/executable identity is
checked before reserved replay.

Both budgets and lexical/hybrid outputs remain separate. All previously delivered
primary/required identities and hashes, complete delivery, required retrieved
ranks, negative returned/rendered/full ID order, scope/trust/tag/caller filters,
embedding fallback and packet/result bounds remain acceptance requirements. The
[comparator](../../scripts/eval/memory-regressions.mjs) is unchanged.

For reproduction, use the existing native `evaluate_memory` example and the
fixture mapping in the [prior full comparison](long-term-memory-full-subject-coverage-nov10-2026.md),
plus `memory-cause-field-cases.json`. Retain pinned 384-dimensional MiniLM,
revision `chroma-minilm-l6-v2-913d7300-title-claim-native-v1`, cutoff 0.30,
short-query scale 11/12, eight records and fixed 36-hour fixture age. Compare
native outputs with `scripts/eval/memory-regressions.mjs`, appending `2000` for
application-budget profiles. The full stress expansion remains 90 distinct
alternatives for each of eight archive records, using the preceding report's
recipe and input hashes.

These are exposed synthetic source cases. The four failed development queries
are ordered views of the same four-symbol bundle, repeated across budgets and
stress profiles; they do not represent independent agent tasks. Existing implicit
semantic weaknesses, false injections and infeasible controls remain separate.
No precision, live-agent quality, latency or rollout-gate improvement is claimed.
