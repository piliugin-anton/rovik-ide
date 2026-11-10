# Oversized incumbents and denser required context

Baseline: `60971d73fd61a8cbcf3cd232a0d5359ca653ef18`.
Branch: `ltm-improvements-nov10-2026`.

The existing oversized-title development cohort improves from **18/32 to
28/32** at both packet budgets. The existing feasible context-retention group
at 1,000 estimated tokens improves from **2/4 to 4/4**, without raising the
budget or trimming a claim. These are complete-answer counts: every required
primary and additional fact must appear in full.

## Extension of saturated cases

The preceding [oversized-title experiment](long-term-memory-title-fit-nov9-2026.md)
kept the original title suite at 32/32 and the feasible application-budget
context groups at 4/4. The new
[symbol fixture](../../scripts/eval/memory-context-completion-cases.json) and
[context fixture](../../scripts/eval/memory-context-completion-retention-cases.json)
extend those saturated groups with **128 additional cases** and eight new source
symbols: `MemoryPacket`, `SharedMemoryStore`, `RetrievalQuery`, `MemoryEmbedder`,
`MemoryRecord`, `MemoryStats`, `VectorIndex` and `NewMemory`.

Each source fact records its source path, source hash and symbol. The title
fixture has 88 cases: 24 positive and 20 negative/control cases per split. It
combines pairs, three/four names, qualified/snake spellings, literal context,
two/three file hints, distinct oversized canonical-title alternatives, and
hidden/provisional/foreign/caller-denied alternatives.

The 40 context cases have 12 positives and eight controls per split. Positive
answers require seven or eight facts, including longer reference context, literal
context and two or three independent file claims sharing the same title. The
requirements fit the eight-record ceiling; that alone does not prove they fit
a 1,000-token packet. Packet allowances are reported separately. The basic new
symbol cohort starts at 24/24, while the denser context cohort starts at 8/12
under both allowances. Neither replaces an older suite or its denominator.

The [protocol](../../scripts/eval/memory-context-completion-protocol.json)
froze both datasets before candidate scoring. Its hashes use
`JSON.stringify(parsed, null, 2)` plus a newline. Source symbols and prior incidents
are exposed public-code cases, not blind real-agent validation.

## Review, trials and selected implementation

The [focused-symbol experiment review](long-term-memory-focused-symbols-nov8-2026.md)
rejected unconditional replacement, wider pools, broad diversification and query
expansion after context or negative regressions. This iteration keeps the model,
encoder input, threshold, query scope, eight-result ceiling and protected-rank
rules. It targets the lowest feasible development groups, excluding controls
requiring more than eight records.

| Development experiment                                     |                  Complete-answer result | Decision |
| ---------------------------------------------------------- | --------------------------------------: | -------- |
| Permit compact title lookup despite an oversized incumbent | Existing oversized cohort 18/32 → 28/32 | Keep     |
| Try earlier unprotected slots after a rejected packet      |                Pressure group 2/4 → 2/4 | Discard  |
| Also try slots ordered by displaced rendered size          |                Pressure group 2/4 → 2/4 | Discard  |
| Use one specific title-match explanation                   |                Pressure group 2/4 → 4/4 | Keep     |

The first accepted change distinguishes a retrieved title from a title capable
of full delivery. If an incumbent's claim text alone exceeds the entire known
packet allowance, it cannot suppress optional lookup of another canonical-title
fact. The oversized incumbent remains at its original rank, with its fetch
opportunity; the compact fact uses another unprotected position. Existing
repository/trust/tag/caller filtering still applies before the indexed pool bound.

The second accepted change uses `requested symbol title` directly as the match
explanation for optional title additions. Previously this was appended to generic
BM25 wording. The specific explanation states the relevance signal without that
redundancy. Scores/components, claim text, evidence, file metadata and trust stay
intact. Saving these presentation tokens lets the two previously rejected
five-fact packets fit under the unchanged 1,000-token allowance.

Both changes retain the existing full-packet preview: every optional title must
fit as a complete claim and every previously complete protected name/file/context
claim must remain complete. Protected incumbents retain their ranks. A lookup,
slot-capacity or rendering failure keeps the legacy plan. Callers without a
packet allowance retain their earlier behavior. The changes add no model inference
and require no schema, vector, cutoff, download or release-asset change.

The two placement experiments gave no additional answer on the pressure group
and were removed. No alternate slot ordering, pruning or claim shortening is
included in the final implementation.

## Final complete-answer results

| Hybrid cohort                                          | Packet allowance | Development before → after | Reserved before → after |
| ------------------------------------------------------ | ---------------: | -------------------------: | ----------------------: |
| Existing oversized-title cases                         |            1,000 |              18/32 → 28/32 |           19/32 → 32/32 |
| Same cases, application allowance                      |            2,000 |              18/32 → 28/32 |           19/32 → 32/32 |
| Existing 720-record title stress                       |            1,000 |              18/32 → 28/32 |           17/32 → 32/32 |
| Same stress cases, application allowance               |            2,000 |              18/32 → 28/32 |           17/32 → 32/32 |
| Existing feasible title-context groups (three corpora) |            1,000 |                  2/4 → 4/4 |               4/4 → 4/4 |
| Same feasible context, application allowance           |            2,000 |                  4/4 → 4/4 |               4/4 → 4/4 |
| New source-symbol cases                                |   Each allowance |              24/24 → 24/24 |           24/24 → 24/24 |
| New seven/eight-fact context cases                     |   Each allowance |                8/12 → 8/12 |             8/12 → 8/12 |

The [machine report](long-term-memory-context-completion-nov10-2026.json) records
all cohort metrics, categorized new-case outcomes, per-record changes, artifact
hashes and comparison results. All **4,660 policy views** pass the regression
comparison, including the **1,508 negative views**, whose returned/rendered/full
IDs and order are unchanged. There are 102 improved complete-answer views;
budgets and related composed queries are correlated, not independent incidents.

The original perfect title and file-focused cohorts remain perfect. New cases
extend coverage rather than replacing those denominators. The new context cohort
still has four misses per split at both allowances; these remain visible as
future targets. Historical cases requiring 15 records remain incomplete under
the eight-record ceiling and are excluded from the feasible-context headline.

## Comparison and validation

The final matrix compares all 4,148 previously accepted policy views plus 512
views of the 128 new cases: lexical/hybrid, development/reserved and separate
1,000/2,000-token profiles. The historical 720-record stress corpora are replayed
as well. Old baseline artifacts are copied byte-for-byte from the preceding
accepted run; new baselines use an immutable copy of the `60971d7` evaluator.
The candidate is selected on development and frozen before reserved scoring.
No fixture, expected fact or policy was retuned from reserved outcomes.

The unchanged [per-record comparator](../../scripts/eval/memory-regressions.mjs)
checks each primary/required identity and hash, complete delivery, delivered
primary rank and every retrieved required rank, complete-answer retention, exact
negative returned/rendered/full ID order, scope/trust/tag/caller restrictions,
embedding fallback and packet/result bounds. Positive packet metadata may become
shorter; an identical packet-token count is not the regression criterion.

Native evaluation retains the pinned 384-dimensional MiniLM model, threshold
0.30, first 500 title/claim characters, at most 256 encoder tokens, short-query
scale 11/12 and stable 36-hour fixture age. Packet budgets use the existing
four-characters-per-token estimate. These runs are not a latency benchmark.

The native evaluator built successfully. The existing oversized-alternative
packet-rollback contract and the new oversized-incumbent rank/delivery contract
passed separately. The new contract also exercises a packet boundary where
redundant matching metadata alone hides a complete fact, while claims/evidence
remain intact. It also passed 30 separate processes, using strict
file weights and vector ordering to avoid UUID tie-dependent preconditions.
Broader crate/core/frontend tests, clippy and the final format check remain
skipped under the user's instruction; changed files were formatted.

For reproduction, use the native evaluator configuration described in the
[preceding report](long-term-memory-title-fit-nov9-2026.md), substituting the new
fixture paths and retaining each policy/split/budget separately. Compare saved
outputs with `node scripts/eval/memory-regressions.mjs baseline.json candidate.json`,
appending `2000` for the application-budget profiles. The existing 90-per-symbol
stress expansion is unchanged.

Seven/eight-fact context cases and four-name cases still expose limited spare
slots when oversized incumbents are protected. Capacity-infeasible historical
controls remain visible and are excluded from headline recall. Negative pools
can contain known-symbol records; freezing their identities does not establish
general abstention quality. No new paired real-agent tasks were collected, and
the separate release-quality gate remains unchanged.
