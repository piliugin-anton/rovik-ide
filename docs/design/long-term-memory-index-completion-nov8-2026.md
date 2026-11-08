# Recall experiments: completing background indexing

Branch: `memory-improvements-nov8-2026`. Baseline: `6545921`.

The native MiniLM background job stopped after three changed claims. Enabling
semantic retrieval on an existing store therefore left most of its claims
unindexed until another capture scheduled work. A second project sharing the
model directory could also have its scheduling request suppressed by the first.

The job now completes its pending snapshot, checks the current settings before
and after each inference, and rejects cancelled results. Queued projects have
separate job identities and share a serial background worker, avoiding duplicate
model preparation and competing background inference. Retrieval also schedules
missing work when the encoder is already cached. Existing valid vectors are
reused. Downloads, model assets, preprocessing, admission thresholds and ranking
are unchanged. Retrieval retains its lexical fallback while inference is busy.
Warm queries finish inference before queuing new work. Background inference waits
for the shared encoder when a query holds it, rather than abandoning the rest of
the snapshot and entering the one-minute failure retry delay. Interactive
inference retains its nonblocking lock and fallback behavior.

## Frozen experiment

[The protocol](../../scripts/eval/memory-indexing-experiments.json) was recorded
before product changes. The existing Rust evaluator now accepts `backfill_limit`
and `capture_offset`. Each capture offset rotates deterministic claim ages, so a
three-record budget sees different claims. Leaving both settings absent preserves
the old fully indexed evaluation. The capture-order test checks that rotation
changes the newest claims without changing their hashes.

These are **availability experiments on previously evaluated synthetic claims**,
not new semantic generalization or live coding success. Development uses offsets
0, 7 and 17 and budgets 0, 3, 8 and unlimited. Heldout uses offset 11 and budgets
3 and unlimited. The policy is fixed at cosine 0.30, short-query scale 11/12,
eight output candidates and a 1,000-token packet. There are 28 corpus/split/order/
budget configurations. The selected approach is full completion; the fallback
experiment of resumable batches was unnecessary because the fully indexed
regression gate passed.

## Results

Counts are complete expected claims delivered in the packet, combined across
the original and follow-up corpora. Repeated development orders are correlated
replays of the same 100 answerable cases.

| Split / capture offset | Three indexed claims | Complete index | Added answers | Lost answers / worse delivered ranks |
| ---------------------- | -------------------- | -------------- | ------------- | ------------------------------------ |
| Development / 0        | 85/100               | 92/100         | 7             | 0 / 0                                |
| Development / 7        | 83/100               | 92/100         | 9             | 0 / 0                                |
| Development / 17       | 83/100               | 92/100         | 9             | 0 / 0                                |
| Heldout / 11           | 48/60                | 54/60          | 6             | 0 / 0                                |

Completing the index also exposes the model's **existing false positives sooner**.
Compared with three indexed claims, two original-corpus development controls
return an extra record at offsets 0 and 7, and one original heldout control
returns an extra record at offset 11. The other orders/corpus show no additional
negative records. This is not a precision improvement. Against the previous
fully indexed implementation, all 258 cases match exactly in delivery, candidate
rank, rendered rank, returned count, packet tokens and project scope. No threshold
was relaxed to obtain these gains. The result supports fixing incomplete indexing;
it does not establish that MiniLM's current false-positive rate is acceptable
for default enablement. Semantic retrieval remains opt-in.

[Machine-readable results](long-term-memory-index-completion-nov8-2026.json)
include each changed case, the full ablation matrix, frozen-input fingerprints,
regression results and verification. Recorded native vectors accelerate the
matrix; an additional native replay verifies the 258 fully indexed cases.

## Reproduction

Use the pinned model release and the existing Rust example, with
`ROVIK_MEMORY_EVAL_MODEL` pointing to its directory,
`ROVIK_MEMORY_EVAL_DIMENSIONS=384`,
`ROVIK_MEMORY_EVAL_REVISION=chroma-minilm-l6-v2-913d7300-title-claim-native-v1`
and `ROVIK_MEMORY_EVAL_THRESHOLD=0.3`. Run:

```sh
cargo run --locked -p verified-code-memory --example evaluate_memory -- \
  --experiment-config /path/to/config.json
```

The example runs from `src-tauri`; paths in the configuration are relative to
that working directory. For one development condition use:

```json
{
  "split": "development",
  "deterministic": true,
  "memory_budget": 1000,
  "short_query_words": 2,
  "short_query_scale": 0.9166666666666666,
  "controls_file": "../scripts/eval/memory-short-query-controls.json",
  "capture_offset": 0,
  "backfill_limit": 3
}
```

Omit `backfill_limit` for complete indexing. For the follow-up corpus set
`dataset_file` to `../scripts/eval/memory-semantic-fresh-cases.json` and omit
`controls_file` (it already includes its controls). The protocol supplies the
remaining splits, offsets and budgets. `save_vectors_file` exports native vectors;
`vectors_file` plus `threshold: 0.3` replays them through the real SQLite index.

New regression tests cover completion beyond three claims, cache reuse,
cancellation before/during inference, resume, changes to memory/semantic settings,
and two projects completing after one native scheduling request each. Existing
clear/edit/delete, verification, caller visibility, scope and budget tests remain
in force. Crate suites are run separately, as requested.

## Rebase onto master

Rebased onto `f72ef2e` (release 0.2.7), which already includes the earlier memory
commits through squash merge #257. Only the index-completion change is replayed.
The experiment protocol and original source fingerprints above remain historical
records of the measured run.

Master separates a repository's shared memory identity from its local settings
path. Background jobs now read and recheck settings through that local path and
use it to distinguish jobs. The capture hook passes the same path as retrieval.
The native integration test now covers two clones sharing one repository identity
and model directory, with separate local settings and stores. Cancellation tests
also use different repository and settings identities.

The rebased native replay preserves all 258 regression outcomes. Additional
rebase verification and current source fingerprints are recorded under
`rebase_validation` in the machine-readable report.
