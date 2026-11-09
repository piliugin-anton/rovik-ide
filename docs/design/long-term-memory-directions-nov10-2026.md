# Directions supported by the memory experiments

Reviewed baseline: `697dd33d9ccb2a2573031be0ad15c9e1b0f1875a` on
`ltm-improvements-nov10-2026`. This review summarizes the 30 existing design
reports and their recorded results. It does not establish live-agent quality.

## Facts and interpretation

| Direction                            | Recorded evidence                                                                  | Interpretation                                                                           |
| ------------------------------------ | ---------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Eligible, distinct candidate pools   | Required delivery 9/21 → 21/21 in constructed crowding cases                       | Apply scope/visibility and exact deduplication before pool bounds.                       |
| Complete background indexing         | Reserved delivery 48/60 → 54/60                                                    | Fix missing representations; completion can expose existing false positives sooner.      |
| MiniLM hybrid representation         | Fresh reserved delivery 23/30 → 28/30                                              | Preserve exact lexical/file evidence while adding learned semantic matching.             |
| Explicit request structure           | Code-list delivery 8/28 → 28/28 in both splits                                     | Normalize complete names and delimiters without broad aliases.                           |
| Previously hidden text               | Sentence-view long-note delivery 33/40 → 35/40 development, 34/40 → 35/40 reserved | Additional bounded, meaningful views can recover information outside the primary window. |
| Complete subject and packet coverage | Dense context 8/12 → 12/12 in both splits and budgets                              | Plan additions against complete claims and preserve independently required context.      |

Sources: [capacity](long-term-memory-capacity-gains-nov7-2026.md),
[index completion](long-term-memory-index-completion-nov8-2026.md),
[MiniLM](long-term-memory-minilm-nov8-2026.md),
[lists](long-term-memory-code-lists-nov8-2026.md),
[sentences](long-term-memory-sentences-nov8-2026.md), and
[complete coverage](long-term-memory-full-subject-coverage-nov10-2026.md).

Broad pool expansion, symbol rank fusion and MMR traded required facts for other
facts. Lower cutoffs, representation rearrangement, WordNet and templated query
views supplied no safe broad improvement. Preserve their counterexamples; retry
only when a new mechanism distinguishes the earlier failures. See the
[experiment review](long-term-memory-focused-symbols-nov8-2026.md) and
[full-pool failures](long-term-memory-fullpool-retries-nov8-2026.md).

Context helped in the earlier proposal experiment: claim-only delivery was 82%,
while contextual linked and information-matched flattened records both delivered
97%. This supports retaining information, without establishing a linkage benefit.
Manual contextual keys and a relevance gate improved exposed reserved recall
80% → 90%, but false injection remained 30%. Automatic context extraction was
not measured. See the [proposal study](long-term-memory-proposal-followup-nov7-2026.md).

## Remaining targets

The latest [machine report](long-term-memory-full-subject-coverage-nov10-2026.json)
retains original implicit delivery 9/14 development and 2/6 reserved; fresh
implicit delivery 7/10 and 8/10; long-diagnostic implicit delivery 6/10 and 8/10.
The four-name oversized category remains 0/4 development at both budgets.

All 14 original/fresh misses omit the expected record while returning zero to
three records in saved outputs whose hashes match that report. They have spare
result slots. Those cases motivate representation/admission experiments, rather
than additional final-packet capacity. Long diagnostics and oversized alternatives
have different selection conflicts and need separate experiments.

## Priorities and gates

1. Test source-grounded symptom, remedy and applicability context for implicit
   requests. Keep original claims, evidence and vectors; evaluate automatic
   extraction separately from manually supplied descriptors.
2. Test bounded independent retrieval views derived entirely from existing text.
   Preserve incumbent ranks, complete delivery, exact filters and normal cutoff.
   Avoid benchmark-specific synonyms. Measure each added inference's benefit.
3. Investigate the remaining four-name oversized selection conflict while
   preserving independently anchored facts and full-packet rollback.
4. Verify actual application capture, indexing, retrieval and prompt delivery.
   Earlier observed-file and explicit-reference integration gaps were invisible
   to library-only evaluations. Add paired real-agent tasks for usefulness.
5. Add separately adjudicated true no-match cases for precision experiments.
   Existing negative-ID invariance protects behavior but also rejects removal of
   irrelevant results. Keep those controls; do not silently relax them or treat
   every record in an incident-negative pool as irrelevant.

Select on development before candidate reserved scoring. Freeze cases, model,
cutoffs and budgets. Compare primary/required identity, hash, full delivery and
rank; exact negative returned/rendered/full IDs; trust/scope/tag/caller filters;
embedding fallback and declared result/packet limits. Use the existing comparator.
Report correlated source incidents and budgets separately. Infeasible controls
(too many required records or one claim exceeding the whole allowance) stay
visible outside feasible recall headlines.

Current 100% explicit-name suites do not establish implicit, precision or agent
quality. Use fresh independent incidents and realistic task replays for further
semantic progress. The existing rollout gate remains unchanged.

## Follow-up experiment log

Trials and results are recorded in the companion
[implicit-view report](long-term-memory-implicit-views-nov10-2026.json).

Five native development trials followed this review, against the unchanged
original and fresh suites. The initial three trials were registered together;
the last two refinements were registered after observing title-only development
regressions. No candidate reserved results selected any rule.

| Trial                                    | Original complete answers | Fresh complete answers | Decision                                |
| ---------------------------------------- | ------------------------: | ---------------------: | --------------------------------------- |
| Baseline                                 |                     65/70 |                  27/30 | Control                                 |
| Distinctive words from the existing task |                     65/70 |                  27/30 | Discard: no gain                        |
| Independent short-title vector           |                     67/70 |                  27/30 | Reject: three negative controls changed |
| Independent short-claim vector           |                     65/70 |                  27/30 | Discard: no gain                        |
| Title plus primary-vector corroboration  |                     65/70 |                  27/30 | Discard: gains removed                  |
| Stricter title cutoff (0.40)             |                     65/70 |                  27/30 | Discard: gains removed                  |

The title-only gains were `pagination-implicit` and `provenance-implicit`.
Changed negatives were `control-tls`, `short-negative-development-1` and
`short-negative-development-11`. Original no-match injection rose from 3/40 to
5/40; one further negative already returned a record and changed its packet.
These controls were retained unchanged. The primary-corroboration trial required
primary cosine at least 0.275 alongside title cosine at least 0.30. The stricter
trial required title cosine at least 0.40. Existing long-conclusion cutoffs stayed
unchanged. These development refinements do not establish an acceptable title gate.

All five prototypes were removed. Across 1,550 trial policy views there is no
accepted runtime change. A further 310 lexical/hybrid original/fresh development
views reproduce the accepted baseline in every case field except latency and
timed telemetry after restoration. Source restoration is byte-for-byte verified.
No full historical/oversized replay, candidate reserved scoring or broad tests
were needed after every prototype failed development selection. Broader tests
remain skipped under the user's instruction.

The next preferred study is grounded symptom/remedy context from actual source
assertions or recorded episodes, with capture accuracy evaluated separately.
Title/body projections only rearrange existing information; they cannot supply
missing problem context. Freeze new independent implicit queries and true
no-match judgments before evaluating extraction. Preserve original assertions,
claim hashes, evidence, applicability and trust; do not replace measured extraction
with hand-authored benchmark aliases. Compare both library delivery and actual
agent prompts/tasks before accepting a gain.

## Grounded cause-field follow-up

The [cause-field experiment](long-term-memory-cause-fields-nov10-2026.md) adds 40
source-bound cases and tests three approaches to exposing an existing captured
assertion: labeled field, assertion-only field, and independent rank fusion.
All remain 3/12 on development, with no accepted runtime change. The nine misses
separate into three delivery and six matching/selection failures across correlated
variants. Preserve these cases instead of revising their questions after scoring.
Measure expected/distractor similarities and admission stages before another
representation trial; additional grounded text alone did not establish a gain.

## Diagnostic step completed

The [diagnostic implementation and measurements](long-term-memory-diagnostics-nov10-2026.md)
add opt-in read-only index probes and preserve all 272 measured development
outcomes. Expected primary similarities for the eight original/fresh misses are
0.147–0.222, below cutoffs of 0.275/0.300; every one has a higher-scoring eligible
nonrequired competitor. These are not small cutoff-boundary misses.

The oversized four-name example selects four new name additions whose individual
content sizes exceed the whole packet. Later optional-title filtering cannot
recover all four compact facts with the remaining slots. The next distinct trial
is packet-feasible selection for new name additions, preserving existing anchors
and full-packet rollback. This follows the observed addition path, rather than
assuming every oversized selected record was an original incumbent.
