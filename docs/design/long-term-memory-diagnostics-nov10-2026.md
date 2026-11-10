# Diagnosing recall before the next policy experiment

Baseline: `3cf69f0692d7986f1dc2e1d2caa97e6de4943923`.
Branch: `ltm-improvements-nov10-2026`.

This completes the first diagnostic step in the
[research directions](long-term-memory-directions-nov10-2026.md). The native
fixture evaluator now supports opt-in read-only index diagnostics. There is no
retrieval-policy or recall gain in this change. All **272 development cases**
retain their earlier outcomes, with diagnostics off and on: **544 native policy
views**, plus comparison to their preceding accepted outputs.

## Measurements

Diagnostics read actual primary/conclusion rows bound to the current model and
claim hash. An unindexed or edited claim stays visibly missing; the probe does
not manufacture a gold document vector. It reports negative cosine values too,
which cannot be obtained through the production search API's nonnegative cutoff.

For every primary and additional required fact the output includes current
eligibility reasons, actual lexical-pool membership, selected rank and full-text
delivery. Each task/failure query probe scores both stored index views, reporting
eligible distinct ranks, exact-copy representatives, admission cutoff, pool bound,
and margin to the best nonrequired competitor. The three strongest nonrequired
competitors retain their identities and titles. Selected-record reasons, content
sizes and full/hint state identify downstream selection and delivery behavior.

All semantic views are explicitly marked as probes. A passing supplementary score
does not prove that a guarded lookup ran. Focused identifier placement and its
packet rollback are not traced; selected reasons and source inspection support
that part of the interpretation. Nonrequired competitors can be useful context,
so their presence is not automatically a precision failure.

## Original and fresh implicit misses

Every expected record below is indexed and eligible. All eight miss the actual
lexical pool and final selection, with zero to three returned results. No stored
conclusion view supplies another admitted match.

| Development case   | Expected primary cosine | Cutoff | Eligible distinct rank | Best nonrequired cosine |
| ------------------ | ----------------------: | -----: | ---------------------: | ----------------------: |
| pagination         |                   0.178 |  0.300 |                      3 |                   0.231 |
| worktree           |                   0.156 |  0.300 |                     10 |                   0.412 |
| provenance         |                   0.205 |  0.300 |                      2 |                   0.272 |
| fallback           |                   0.147 |  0.275 |                      5 |                   0.231 |
| generation         |                   0.169 |  0.300 |                      3 |                   0.247 |
| cancellation       |                   0.201 |  0.300 |                      2 |                   0.288 |
| rate limits        |                   0.222 |  0.300 |                      2 |                   0.304 |
| optimistic locking |                   0.210 |  0.300 |                      5 |                   0.362 |

These are substantial similarity gaps, not just boundary rounding. All expected
records score below another eligible nonrequired record. More final-result space
cannot admit them. This also explains why the earlier title/primary corroboration
experiment removed the title-only gains: pagination and provenance primary scores
are below its 0.275 corroboration condition.

The evidence supports a future representation/encoder study with fixed queries
and judged negatives. It does not support blindly lowering the cutoff: competing
records already score higher, and the earlier cutoff trials exposed false matches.
No thresholds or representations were tuned in this diagnostic run.

## Other failure mechanisms

| Development cohort | Complete answers | Observations                                                               |
| ------------------ | ---------------: | -------------------------------------------------------------------------- |
| Original           |            65/70 | Five implicit misses below semantic admission                              |
| Fresh              |            27/30 | Three implicit misses below semantic admission                             |
| Cause fields       |             3/12 | Three selected-but-not-full cases and six matching/selection misses        |
| Long diagnostics   |            26/30 | Four misses below semantic admission, despite full lexical/hybrid pools    |
| Oversized titles   |            28/32 | Four-name cases expose semantic pool and subsequent name-planning pressure |

For the cause-field clone case, the hidden primary score is -0.011; the explicit
paragraph control scores 0.214 in its conclusion view, still below 0.300. The
short-record control scores 0.123. The analogous indexing control remains below
cutoff even with its source assertion visible. Field visibility alone therefore
does not explain these missing answers. The packing cases are already selected
at rank seven/eight and lose full delivery near the packet budget.

For the four primary oversized-title misses, raw primary scores are 0.284, 0.359,
0.477 and 0.267 against a 0.275 cutoff. Their eligible ranks are 17, 12, 9 and 18.
Three pass admission but lie below the retained semantic pool; one also fails
admission. This describes the semantic route, not the entire focused planner.

The selected match reasons reveal a more specific next target. In the
`title-fit-OnnxMiniLm-four` request, slots four through seven contain oversized
claims marked `requested complete symbol match`. Their content alone costs
3,545–4,280 estimated tokens, so none can fit the 1,000-token packet. Three
crowding observations and the literal `crowding` claim occupy the other slots.
The four required compact claims are absent.

Those oversized entries were chosen as **new name additions**. The current name
collector selects its highest-ranked eligible match before considering packet
feasibility. The later optional-title collector already rejects individually
oversized additions, but has too few unprotected slots to add all four compact
facts. This refines the earlier description of an oversized-incumbent conflict:
in this example the name plan itself introduces the oversized claims.

The next scoped experiment should apply packet feasibility when selecting a new
name addition, with fallback and full-packet preservation checks. Existing
incumbents, independent anchors, filters and rank/delivery contracts must remain
protected. This experiment has not been run here; the present commit completes
measurement first.

## Validation and reproduction

All **19 evaluator tests** pass. New contracts compare diagnostic eligibility and
copy representatives with actual production search, check negative scores and
stale/missing index rows, verify that probes leave stored records unchanged, and
separate selected-but-not-full delivery from complete delivery. Invalid diagnostic
vectors fail rather than producing a fabricated score. Focused example clippy
passes with warnings denied, and the native evaluator builds.

Normal and diagnostic runs reproduce every prior case field except elapsed
timings, including non-timing telemetry. The existing per-record comparator passes
on all 272 cases. There are no changed claims, expected facts, filters, negatives,
required ranks or complete answers. Only evaluator code and documentation change.
Broader crate/core/frontend tests remain skipped under the user's instruction.
No reserved or 720-record stress replay is needed to claim this development-only
measurement result; neither was run.

Add `"diagnostics": true` to the existing experiment JSON alongside
`"deterministic": true`, `"split": "development"`, `"memory_budget": 1000`,
`"short_query_words": 2` and `"short_query_scale": 0.9166666666666666`.
Use the pinned native 384-dimensional MiniLM backend, revision
`chroma-minilm-l6-v2-913d7300-title-claim-native-v1`, threshold 0.3. Diagnostics
requires the hybrid backend; semantic-only and lexical-rerank ablations are
rejected. Omitting diagnostics preserves the normal result schema.

The [machine report](long-term-memory-diagnostics-nov10-2026.json) contains input,
source and result hashes, unchanged metrics, comparison audits, all missed facts'
probe measurements and selected-record traces. Probe scans add work, and extra
query encoding can warm later fixture caches. Although the per-case retrieval
timer ends before probe collection, this is not a latency benchmark. Exposed
synthetic incidents, correlated variants and unjudged background context remain
limitations; no precision or live-agent improvement is claimed.
