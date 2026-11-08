# Long diagnostic recall experiments

Baseline: `db856ab`. These experiments are on `ltm-improvements-nov8-2026`.
They found no production change that passed the per-fact regression checks. The
retrieval implementation was restored byte-for-byte.

## Cases and baseline

The [90-case fixture](../../scripts/eval/memory-long-failure-cases.json) reuses
existing semantic-fresh incidents and negative queries. It places each original
query in the final paragraph of a long diagnostic, with a generic task. The
existing model sees the beginning of that diagnostic; lexical search sees all of
it. Each split contains 30 answerable cases and 15 negatives. These are constructed,
correlated views of exposed incidents, not independent or blind evaluations.

Baseline hybrid delivery is **26/30 development** and **28/30 reserved**. The
four development misses have full eight-record pools. The new negative cases
already return seven or eight records each, so this does not establish precision
or a reduction in unrelated matches. These counts describe this new fixture; the
older diagnostic fixture still returns one to four records in its negatives.

Three separate 24-case retention fixtures add long diagnostics to existing
multi-fact cases. Each case asks for ten facts with an eight-record limit and is
therefore excluded from complete-answer recall. Instead, every previously
supplied fact and its rank is checked:

- [Long-diagnostic retention](../../scripts/eval/memory-long-failure-retention-cases.json)
  retains the original tasks.
- [Generic-task retention](../../scripts/eval/memory-long-failure-generic-retention-cases.json)
  moves the collection request into the diagnostic.
- [Plural retention](../../scripts/eval/memory-long-failure-plural-retention-cases.json)
  expresses the collection without universal words such as “all” or “every.”

The first retention fixture was added after the initial full-pool development
result and before that retention check. The generic-task guard was added before
the gated trial; the plural guard was added after its initial development result
and before its own scoring. These timings are recorded in the
[protocol](../../scripts/eval/memory-long-failure-protocol.json).

## Trials

| Development trial                                                               | Complete answers | Retention outcome                                             | Decision |
| ------------------------------------------------------------------------------- | ---------------: | ------------------------------------------------------------- | -------- |
| Baseline                                                                        |            26/30 | Existing supplied facts                                       | Control  |
| Append the final diagnostic paragraph into free slots                           |            26/30 | No headline gain                                              | Reject   |
| Replace the final slot with a qualifying diagnostic match                       |            28/30 | 10 previously supplied fact instances lost                    | Reject   |
| Replace only for generic tasks and diagnostics without explicit universal words |            28/30 | 12 previously supplied fact instances lost in plural controls | Reject   |

A fact instance is one required observation in one case; cases share incidents.
Negative IDs and order did not change in these trials, but a stable result count
cannot make up for a displaced requested fact. Collection intent can be expressed
without a universal keyword, so the narrower rule is not reliable enough to ship.
No candidate was evaluated on the reserved split. Baseline results on that split
are shown above for context; the incidents were already exposed in prior work.

The [machine-readable report](long-term-memory-long-failures-nov8-2026.json)
records each trial and displaced fact. The original retrieval source was restored,
and development replays of all four new fixtures reproduce their baseline
outcomes. The branch retains these cases and evidence for future experiments;
it does not claim a production recall improvement.

## Reproduction

Build the existing `verified-code-memory` Rust `evaluate_memory` example. Select
`dataset_file: "scripts/eval/memory-long-failure-cases.json"`, a deterministic
split, result limit eight, memory budget 1,000, short-query words 2 and scale
11/12. For hybrid retrieval use the pinned local MiniLM model, 384 dimensions,
revision `chroma-minilm-l6-v2-913d7300-title-claim-native-v1` and threshold 0.3.
Run the three retention datasets separately and compare each required fact and
its rank rather than their intentionally infeasible complete-answer headline.
