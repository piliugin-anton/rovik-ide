# Contextual memory proposal: tested follow-up

The user supplied a seven-hypothesis proposal after
[the first experiment](long-term-memory-experiments-nov7-2026.md) reached its
three-non-improvement stopping rule. This follow-up tests the remaining ideas
separately. It adds research harnesses and results, not a production policy change.

## What improved

The most promising prototype combines **source-bound contextual search keys
with a semantic relevance gate**. On the existing validation split, Recall@8
rises from **80% to 90%** (24/30 to 27/30 answerable cases). No-match injection
stays at **30%** (3/10). This still fails the proposed 5% injection release target.

These are exploratory results: descriptors were manually authored, development
misses were already known, and this validation split was exposed by earlier
experiments. A validation false positive motivated the gate hypothesis, although
its numeric threshold was selected on development only. This is not a fresh
blind test or evidence of real-agent task improvement.

| Requested hypothesis / variant                         | Development Recall@8 | Validation Recall@8 | Validation no-match injection |
| ------------------------------------------------------ | -------------------- | ------------------- | ----------------------------- |
| Existing lexical/file control                          | 82.86%               | 80%                 | 30%                           |
| H1: contextual search keys                             | 100%                 | 90%                 | 40%                           |
| H2: semantic-only word-vector ablation                 | 31.43%               | 16.67%              | 20%                           |
| H1 + existing hybrid fusion                            | 100%                 | 90%                 | 40%                           |
| H3: remove supplied file context, keeping intent only  | 62.86%               | 63.33%              | 30%                           |
| H5: widen lexical candidates to 32, retain final eight | 82.86%               | 80%                 | 30%                           |
| H5: contextual candidates + bi-encoder gate/reranker   | 100%                 | 90%                 | 30%                           |

All configurations use the same corpus, scopes, final limit of eight and memory
allowance of 1,000 estimated tokens. Candidate and delivered recall are recorded
separately; every recalled answer survived packing in this fixture. The largest
packet across all calibration trials was 460 tokens (334 in the selected
configurations). No trial exceeded the allowance. Identical prohibited records were indexed
both lexically and semantically in another project; no variant returned them.

The selected semantic-only threshold is 0.65, contextual hybrid 0.90, and
contextual gate 0.50. Each is chosen on the same predeclared development grid.
The gate retains exact-title and explicit-file candidates, admits other lexical
candidates only when semantic similarity qualifies, and reranks within the
existing admitted pool. It never adds a record from outside that pool.

## Interpretation by hypothesis

**H1 — Contextual keys:** source claims, evidence and record contents remain
unchanged. A separate, claim-bound descriptor set supplies searchable tags;
semantic index representations can include the same descriptors. Descriptors
are applied to every incident and the other-project copy, not just the expected
answer. A changed claim rejects old descriptors; contextual vector revisions
include the descriptor-file fingerprint. This tests the value of supplied
context, not an automatic context extractor. The
[descriptor artifact](../../scripts/eval/memory-context-keys.json) records its
manual provenance. Its production equivalent would need grounded generation
and permission-aware source provenance.

**H2 — Semantic comparison:** the actual pinned 300-dimensional spaCy word-vector
model was evaluated without lexical/file candidates as an ablation. Its poor
standalone result argues for preserving exact signals with this model. Adding
hybrid fusion to enriched lexical keys produced no extra recall in these trials.
This says nothing decisive about a transformer sentence encoder or multilingual
quality, neither of which was tested.

**H3 — Task-state queries:** removing already-supplied file fields loses 14/70
development answers and 5/30 validation answers. The implementation already
supports these fields and read-triggered retrieval; the experiment confirms
their contribution. This does not establish that a model independently discovers
the right file, or measure diagnostic/constraint extraction from live traces.
Those require a different task replay, rather than substituting gold paths into
implicit queries.

**H4 — Linked granularity:** on 100 constructed answerable questions, structured
claim-only records deliver 82% of answers. Both source-linked contextual records
and **flattened records with the identical additional context** deliver 97%,
with zero packet budget overruns. The information-matched control matters:
this supports preserving context, but shows no recall advantage for linkage
itself. Source episodes remain available for audit. These mechanism fixtures
have no no-match controls and do not establish deployment quality.

**H5 — Depth and reranking:** candidate Recall@32 equals Recall@8 for the lexical
control. The missed memories are not hidden below rank eight in that candidate
set. A learned cross-encoder cannot recover absent candidates, so no learned
cross-encoder improvement is claimed. A separate, cheap bi-encoder gate on
contextual candidates tests false-positive admission. Contextual keys added an
unrelated compression match on validation; the gate removes that additional
injection while preserving the recall gain. This gate experiment was motivated
by exposed validation feedback and needs independent confirmation.

**H6 — Diversity:** a constructed candidate-selection stress case has twelve
admitted records from one incident ahead of one independent required fact.
Plain top-eight packing supplies one of two requirements in 514 tokens.
Collapsing source-episode families supplies both in 136 tokens and two records.
Both selectors receive the same thirteen candidates and 1,000-token allowance.
This proves the duplicate-crowding mechanism; the ranking is constructed and
does not estimate its frequency in real project histories. Collapse does not
introduce unrelated candidates to manufacture diversity.

**H7 — Capture:** activity-only episodes contain none of the required claims by
fixture construction, so answer-bearing capture and delivery are zero. Adding
source-linked structured claims raises capture coverage to 100% and answer-bearing
retrieval/delivery to 82%. This separates absent information from retrieval
failure and preserves the original episode. It does **not** measure an LLM's
extraction accuracy, prove inferred causes, or validate that production agents
will write the supplied claims. Fixtures remain provisional; successful activity
does not verify their facts.

## Remaining evidence and implementation decision

The hard negatives are more concerning than the headline recall gain. Three of
four new validation controls are false positives even in the baseline (3/10 when
including the six repeated original spectroscopy controls). The enriched ungated
prototype matches all four. Broad words such as project, order, path and
compression can connect unrelated tasks. The gate restores baseline abstention
but does not solve it. Keep the original zero-injection result scoped to the
original fixture; it does not generalize to these controls.

The source-bound key and gate prototypes are worth a controlled follow-up, but
the evidence does not authorize enabling them by default. A concrete production
change would need:

1. Bounded descriptor generation from actual source/evidence, measured separately
   from the manual hints used here; preserve original claims and trust states.
2. Descriptor fingerprints in derived-index identity and permission checks across
   every linked source; extend existing edit/delete/clear lifecycle tests.
3. A calibrated ambiguity gate that meets abstention targets on fresh independent
   incidents, while protecting exact identifiers and explicit files.
4. Optional incident collapse after relevant candidate generation, evaluated on
   authentic tasks requiring multiple memories.
5. Paired agent tasks under the same pinned model/tools/budget, plus supported
   model placement and 100/1,000/10,000-record latency measurements.

No automated extractor, learned cross-encoder, live agent task-success gain or
new blind holdout result is claimed. All runtime defaults stay unchanged.

## Artifacts and reproduction

The [protocol](../../scripts/eval/memory-followup-protocol.json) records the
user-proposal fingerprint, descriptor/dataset hashes, ablations and the gate
amendment. [Machine-readable results](long-term-memory-proposal-followup-nov7-2026.json)
include per-case candidate, selected and supplied outcomes, every calibration
threshold, model identity and constructed capture/selection results.

With the first experiment's isolated model environment provisioned:

```sh
cd src-tauri
cargo build --locked -p verified-code-memory --examples
cd ..
/tmp/rovik-memory-eval/bin/python scripts/eval/memory-followup.py \
  --evaluator "$PWD/src-tauri/target/debug/examples/evaluate_memory" \
  --structure-evaluator "$PWD/src-tauri/target/debug/examples/evaluate_memory_structure" \
  --output /tmp/rovik-memory-proposal-results
```

Adjust executable paths for `CARGO_TARGET_DIR`. Model initialization after Python
imports took about 0.56 seconds. Warm in-process query encoding measured p50
0.012 ms and p95 0.059 ms over 370 samples. These exclude worker IPC, SQLite,
index creation, model response time and corpus scale; they are not production
end-to-end latency. Raw per-case replay times include rebuilding derived indexes.

Selected-gate recall, admission, candidate ranks and delivery were independently
replayed with identical results. Validation includes 81 nextest cases across
the memory crate and examples, one doctest, Python protocol checks, Clippy with
no warnings, frontend tests/type checking and the repository formatter. No
macOS/Windows or live-provider execution is implied by these Linux checks.
