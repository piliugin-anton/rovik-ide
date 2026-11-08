# Full-pool retries and harder recall cases — November 8, 2026

The evaluation now extends previously perfect isolated CamelCase and snake_case
lookups with **80 cases**, plus **24 separate retention guards**. Three registered
retrieval experiments failed development regression checks. Their code was reverted;
this change retains the fixtures, protocol and measured failures. There is no claimed
production recall gain.

## What the new cases test

The [main fixture](../../scripts/eval/memory-fullpool-retry-cases.json) keeps the
eight source-bound incidents from the existing identifier evaluations, four per
split. Each incident gets isolated CamelCase and reversed snake_case controls,
dense variants of both, a three-fact request, exact-copy pressure, file context,
and three negative controls. This gives 28 answerable cases and 12 negatives per
split. The corpus contains all eight source facts, nine distinct crowding
observations, twelve exact copies of another observation, and unrelated file
context. Earlier fixtures are unchanged.

The [retention fixture](../../scripts/eval/memory-fullpool-retry-retention-cases.json)
adds three requests per incident: singular wording, plural wording, and a separate
failure channel. Each explicitly requests the crowding observations alongside the
source fact. Ten required facts cannot fit eight slots, so these cases are excluded
from recall headlines. They instead guard every previously supplied fact and its
rank. Keeping the total number of supplied facts unchanged cannot conceal a swap.

These are correlated synthetic views of already exposed source incidents, not new
independent incidents or blind validation. Negative cases mean the incident's
expected answer is inapplicable; other corpus records can still be returned. We
compare negative record IDs **and order**, without claiming human-rated precision.
The new crowding negatives return eight records; this does not change the earlier
diagnostic negatives that returned one to four.

## Baseline

Using native MiniLM, cosine admission 0.3, the existing short-query adjustment,
eight final records and a 1,000-token packet:

| Policy  | Development | Reserved split |
| ------- | ----------: | -------------: |
| Lexical |       12/28 |          12/28 |
| Hybrid  |       16/28 |          22/28 |

All isolated CamelCase, isolated snake_case and exact-copy controls still succeed
for both policies: 4/4 in each category and split. The extra dense and composed
cases reveal the remaining gaps. All three-fact requests fail complete delivery
in both splits; dense CamelCase succeeds 0/4 development and 2/4 reserved under
hybrid retrieval. A three-fact answer is counted successful only when all three
facts reach the rendered packet.

The original 48-case identifier fixture still reaches 16/16 positive delivery
per split for both policies. The original 80-case snake_case fixture still reaches
28/28 per split under hybrid retrieval; lexical remains 24/28. These checks use
the restored production implementation.

## Registered retries

The [protocol](../../scripts/eval/memory-fullpool-retry-protocol.json) froze both
new inputs and three fixed trials before scoring. Previous experiments were
reviewed in the [initial rerun](long-term-memory-postimplementation-rerun-nov7-2026.md),
[recall iterations](long-term-memory-recall-iterations-nov8-2026.md), and
[hybrid anchors report](long-term-memory-hybrid-anchors-nov8-2026.md).

Wider pools were worth retrying because earlier trials used word vectors and
simpler fixtures. Complete-symbol fusion retries contextual relevance using
algorithmic name components rather than manually authored aliases. Diversity
selection revisits duplicate/incident pressure using current vector similarities,
while retaining distinct stored claims. The known unsafe unconditional tail
replacement was not retried.

| Trial                                              | Development hybrid delivery | Previously successful answers lost | Changed negatives | Retention fact instances lost / ranks worsened |
| -------------------------------------------------- | --------------------------: | ---------------------------------: | ----------------: | ---------------------------------------------: |
| Baseline                                           |                       16/28 |                                  — |                 — |                                              — |
| 32 candidates per input, existing RRF, final eight |                       14/28 |                                  2 |                 4 |                                         3 / 30 |
| Wider pools plus complete-symbol RRF input         |                       24/28 |                                  0 |                 4 |                                        13 / 69 |
| Wider pools plus MMR, lambda 0.8                   |                       20/28 |                                  2 |                 4 |                                        20 / 43 |

A retention fact instance is one specific required fact in one case; these are
not independent incidents. Rank regressions count still-present facts separately
from lost facts. All trials leave lexical results unchanged. No scope, visibility,
trust, tag or packet-limit violations were observed in these new replays.

Complete-symbol fusion gains eight answers but displaces requested observations.
Diversity gains six answers while losing two file-context answers. Aggregate
recall would obscure both failures. All three trials are rejected; rejected
prototypes were never scored on the reserved split. Native weights, preprocessing,
thresholds, schemas and release assets remain unchanged.

## Evidence and reproduction

The [machine-readable report](long-term-memory-fullpool-retries-nov8-2026.json)
contains trial summaries, exact prototype patches against baseline `42ac482`,
changed answer ranks and negative IDs, individual retention regressions, output
fingerprints, and restored-source verification. The final implementation reproduces
all 208 new baseline policy views exactly, excluding timing fields. Existing
retrieval code and fixtures remain byte-for-byte unchanged.

Build `evaluate_memory` from the `verified-code-memory` crate, then use an experiment
configuration such as:

```json
{
  "dataset_file": "scripts/eval/memory-fullpool-retry-cases.json",
  "split": "development",
  "deterministic": true,
  "candidate_limit": 8,
  "memory_budget": 1000,
  "short_query_words": 2,
  "short_query_scale": 0.9166666666666666
}
```

Run the example with `--experiment-config <path>`. For hybrid retrieval, set
`ROVIK_MEMORY_EVAL_MODEL` to the pinned local ONNX model directory,
`ROVIK_MEMORY_EVAL_DIMENSIONS=384`,
`ROVIK_MEMORY_EVAL_REVISION=chroma-minilm-l6-v2-913d7300-title-claim-native-v1`,
and `ROVIK_MEMORY_EVAL_THRESHOLD=0.3`. Unset those overrides for lexical retrieval.
Use the retention dataset separately and compare per-fact ranks rather than its
intentionally infeasible complete-answer recall. Only a safe improving development
candidate should proceed to reserved-split evaluation.
