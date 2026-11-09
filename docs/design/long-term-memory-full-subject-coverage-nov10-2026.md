# Complete explicit subjects under slot pressure

Baseline: `b13ea77675e4a7231cafb51a43cfc2c3ad655af7`.
Branch: `ltm-improvements-nov10-2026`.

The preceding [dense-context experiment](long-term-memory-context-completion-nov10-2026.md)
left four of twelve seven/eight-fact context requests incomplete. The missing
facts were available in the eligible pool: an unrelated leading observation
reserved one of the eight slots, leaving seven for eight explicit subjects.
This iteration targets that capacity conflict without expanding the pool,
changing the model or shortening claims.

## Extending saturated cases

Two new frozen fixtures extend the perfect title and seven-fact context groups:

- [Full-subject cases](../../scripts/eval/memory-full-subject-cases.json).
- [Reference-pressure cases](../../scripts/eval/memory-full-subject-pressure-cases.json).

Each contains 120 cases: 32 positives and 28 negative/control requests per split.
The **240 additional cases** reuse the preceding eight public-source incidents.
They combine two/three named facts, a named cross-reference fact, a literal context
fact and two/three independent file facts. Complete answers require seven or eight
full claims. Qualified and snake spellings, reversed file order, duplicate path
hints and duplicate canonical name aliases test the subscription boundary.
Unknown names/files/literals, collection wording, failure context, numeric names
and narrative requests remain explicit controls. Older fixtures and denominators
are unchanged.

The reference-pressure fixture gives the cross-reference claim a 4,250-character
body. That body alone exceeds a 1,000-token allowance under the existing
four-characters-per-token estimate. Its 1,000-token profile is an intentionally
infeasible control; the separate 2,000-token profile is feasible. Zero complete
answers there does not mean previous improvements were removed.

The [protocol](../../scripts/eval/memory-full-subject-protocol.json) freezes
inputs before trials. Hashes use `JSON.stringify(parsed, null, 2)` plus a newline.
These are composed exposed-source cases, rather than new independent incidents
or blind real-agent tasks.

## Selected approach and regression review

The [earlier experiment review](long-term-memory-focused-symbols-nov8-2026.md)
rejected unconditional replacement, broad diversification, wider pools and query
expansion after required-context or negative regressions. The new fallback
requires stronger evidence than unconditional leader replacement:

1. First try the existing optional-title placement unchanged.
2. If it fails, permit one additional slot from the leading result only when that
   result has no independent requested-name, supplied-file or literal-title anchor.
   Complete body matches and qualified whole-title matches protect it too.
3. Require every requested canonical title, supplied file and distinctive literal
   subject to appear in a fully rendered claim. Fetch hints do not count.
4. Require at least the result limit's worth of distinct explicit subjects.
   Canonical identifier aliases and overlapping file paths each count once.
5. Preserve every other previously complete protected claim and its rank. Keep
   the original plan if capacity, lookup or full rendering cannot satisfy this.

This deliberately permits displacement of unrelated leading context only for a
fully subscribed explicit request. It does not promise to preserve every old
unanchored leading observation. Existing required-fact/rank checks remain in
force; the independent-anchor contracts cover file, body-name, literal and
qualified canonical-title leaders.

Development selection improved the existing dense group from 8/12 to 12/12 and
the new feasible group from 21/32 to 32/32. Review then tightened qualified
CamelCase leader protection before the final replay. No alternative pool size,
threshold, embedding input, slot pruning or claim truncation was introduced.

## Automatic recall integration

The library planner already accepted file hints and a packet allowance. Automatic
application recall supplied its 2,000-token allowance but did not pass explicit
UI file references separately, and editor/reference wrapper text could obscure
the focused task. The application now reads the current request's matching
`sentContent` metadata, separates supported UI envelopes, and uses the entire
typed intent with validated explicit project-relative file hints.

Only the explicit-reference list supplies paths. An active editor alone does not
become a hint, line-range display aliases do not create extra paths, and spaces
in paths are retained. Stale or unsupported metadata, or a reference envelope
with no usable project paths, keeps the original query. Existing project-path
validation, own-chat exclusion, verification/visibility filters, task bookkeeping,
eight-result ceiling and packet allowance remain in place. The manual
`memory_search` response contract is unchanged.

A scripted-model run checks the actual initial prompt: a memory associated only
with an explicit file is recalled despite no typed keyword overlap, while an
unrelated open-editor wrapper memory is excluded. This validates the application
wiring separately from the pinned-model library evaluations; it is not a paired
live-agent benchmark.

## Results and validation

Every one of the **5,620 policy views** passes the regression comparison,
including **1,956 negative views** with unchanged returned/rendered/full IDs and
order. There are **85 improved complete-answer views**. Profiles and composed
requests reuse incidents, so these are correlated gains rather than 85 independent
tasks.

| Hybrid cohort                                  | Packet allowance | Development before → after | Reserved before → after |
| ---------------------------------------------- | ---------------: | -------------------------: | ----------------------: |
| Existing seven/eight-fact context              |   Each allowance |               8/12 → 12/12 |            8/12 → 12/12 |
| New full-subject cases                         |   Each allowance |              21/32 → 32/32 |           20/32 → 32/32 |
| New reference-pressure cases                   |            2,000 |              21/32 → 32/32 |           20/32 → 32/32 |
| Same pressure cases, infeasible packet         |            1,000 |                0/32 → 0/32 |             0/32 → 0/32 |
| Existing oversized-title and 720-record stress |   Each allowance |              28/32 → 28/32 |           32/32 → 32/32 |
| Preceding source-symbol cases                  |   Each allowance |              24/24 → 24/24 |           24/24 → 24/24 |

These counts require every primary and additional fact in full. Lexical results
are unchanged and reported separately; they do not receive the hybrid planner's
gain. Previously perfect title/file cohorts and feasible five-fact retention
groups remain perfect. The
[machine report](long-term-memory-full-subject-coverage-nov10-2026.json) records
separate policy/split/budget groups, categorized complete-answer counts, individual
gains, regression audits and baseline/candidate artifact hashes.

The matrix includes all 4,660 accepted policy views plus 960 views from the new
240 cases, including every historical 720-record oversized-alternative corpus.
Lexical/hybrid and 1,000/2,000-token profiles stay separate. Old baseline outputs
are copied byte-for-byte from the preceding accepted replay; new baselines use
an immutable copy of the `b13ea77` evaluator. Runtime selection uses development;
the final runtime is frozen before reserved candidate scoring. No fixture,
expected fact or policy is retuned from reserved outcomes.

The unchanged [per-record comparator](../../scripts/eval/memory-regressions.mjs)
checks individual delivered primary/required identities and hashes, required
retrieved ranks, complete-answer retention, exact negative returned/rendered/full
ID order, scope/trust/tag/caller restrictions, embedding fallback and packet/result
bounds. Aggregate recall alone cannot satisfy these checks.

The native evaluator built successfully. All 14 existing focused-symbol tests
and three new full-subject contracts passed. The latter cover complete delivery,
all protected context ranks, independent leader anchors, partial-packet rollback,
and duplicate-alias rejection; they also passed 30 separate processes with strict
file/vector ordering to avoid UUID-dependent preconditions. Three core wire/path
unit tests and the scripted-model automatic-recall test passed, with crates tested
sequentially. Broader crate/core/frontend suites, clippy and the final format check
remain skipped under the user's instruction; changed files were formatted.

For reproduction, use the native configuration from the
[preceding report](long-term-memory-context-completion-nov10-2026.md), substituting
the new fixture paths. Retain the pinned 384-dimensional MiniLM model, threshold
0.30, first 500 title/claim characters, at most 256 encoder tokens, short-query
scale 11/12 and stable 36-hour fixture age. Compare saved outputs with
`node scripts/eval/memory-regressions.mjs baseline.json candidate.json`, appending
`2000` for application-budget profiles. The existing 90-per-symbol oversized
stress expansion is unchanged.

The feasible synthetic groups do not establish general precision, abstention or
latency quality. Negative cases can return known-symbol records; preserving their
identities does not prove abstention. Historical capacity-infeasible cases remain
visible. No new paired live-agent tasks were collected, and the separate
release-quality gate remains unchanged.
