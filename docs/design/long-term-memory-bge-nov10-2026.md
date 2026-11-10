# Lightweight instruction-trained encoder experiment

Baseline: `7a28c691a3b14255d3d5b0c663c4b6c9f1f23f73` on
`ltm-improvements-nov10-2026`.

**Reject both BGE rescue variants.** All 192 registered policies have zero
complete-answer gains. Of these, 142 preserve every result and 50 change negative
packets. Production remains on pinned Chroma MiniLM. This study adds correct
opt-in native CLS pooling, reference export and 96 frozen cases; it does not
enable a second application model or claim an application recall improvement.

## Model and native compatibility

Following the [instruction-reranker recommendation](long-term-memory-instruction-reranker-nov10-2026.md),
test the smaller, native-compatible `BAAI/bge-small-en-v1.5`, pinned at revision
`5c38ec7c405ec4b44b94cc5a9bb96e735b38267a`. Its official float32
`onnx/model.onnx` is 133,093,490 bytes, with repository LFS SHA-256
`828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35`.
The pinned tokenizer SHA-256 is
`d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66`.

The upstream pooling config requires the **first token's 384-dimensional vector,
L2 normalized**, rather than MiniLM's masked mean. The new explicit
`OnnxMiniLm::new_with_pooling` constructor supports this compatible BERT graph
through the existing Tract backend. The existing constructor still selects masked
mean, and the production model identity, downloads and release assets stay exact.
Different model/preprocessing views must use different vector revisions.

Before fixture scoring, six short nonfixture references compare native Tract
vectors against native ONNX Runtime Node 1.31.1 CPU, using the same official graph
and exported token inputs. Independently normalized first-token vectors have a
maximum component difference of **0.000000388**, with cosine above 0.99999999999.
References include empty input, Unicode and an instructed query. This checks two
inference engines and pooling arithmetic, not independent tokenizer/PyTorch parity.
ONNX Runtime remains an externally supplied research dependency only.

The evaluation retains the native 500-character and 256-token windows. BGE accepts
a larger upstream window, but these short fixtures do not need it; this experiment
does not enlarge the application's input window. Unit checks reject masked-first,
zero, invalid-shape and nonfinite pooled vectors. Both existing ignored native
MiniLM reference/window tests pass when explicitly enabled. All **203 existing
development rows** reproduce every non-timing case field exactly.

## Frozen fixtures and policy

The [protocol](../../scripts/eval/memory-bge-protocol.json) and
[new fixture](../../scripts/eval/memory-bge-fresh-cases.json) precede fixture scoring.
Keep original, fresh-semantic and earlier encoder questions and labels unchanged.
Add sixteen new incident-separated source mechanisms: eight development and eight
reserved incidents. Each supplies implicit symptom/procedure questions and literal
and file controls. Each split has 32 positive and 16 no-match cases: **96 new
cases total**. All sixteen source facts remain available in both retrieval corpora;
the reserved query outcomes remain unscored. No-match rationales are assistant-authored
corpus reviews, not independent precision labels.

Both upstream-recommended query variants are registered before calibration:

- Plain query, with no query or document prefix.
- Query prefixed with `Represent this sentence for searching relevant passages: `;
  document prefix is explicitly empty.

Both use normalized CLS and the existing title/newline/claim documents. MiniLM
keeps cutoff 0.30 and short-query scale 11/12. BGE diagnostics use scale 1. Twelve
cosine floors (0.45–0.90) and eight winner-margin floors (0–0.15) make **96 policies
per variant**. BGE cosine is not a calibrated relevance probability; MiniLM cutoffs
cannot simply be reused.

Only empty MiniLM results without query files can change. Rank the actual eligible
primary records by BGE cosine/UUID, require a runner-up and both floors, then append
at most the winner using its **MiniLM native append packet preview**. BGE's own
retrieval packet preview is not a substitute: it starts with different incumbents.
Match candidate ID and claim hash across both pools, and never insert a required
fact specially or select a different winner after rejection. No import/export of
cached vectors is allowed for the CLS or prefix experiments.

The unchanged per-record comparator checks complete delivery, IDs/hashes, ranks,
exact negative returned/rendered/full-text IDs and order, limits, budget and
fallbacks. An additional audit rejects every newly added positive ID outside its
required facts. Existing scope, trust, tag and caller filters determine eligibility.
Historical development and then fresh reserved evaluation are conditional on a safe
development gain. No policy qualifies for those later stages.

## Results and failure mechanism

Twelve native runs cover **753 development views**: MiniLM plus the two BGE views
on four suites, each containing 251 query rows. Primary pools contain 16–20 distinct
eligible records, all below the 32-record diagnostic bound. No pool truncates.

| Development suite   | MiniLM complete answers | Positive cases | Negative cases | Negatives returning any item |
| ------------------- | ----------------------: | -------------: | -------------: | ---------------------------: |
| Original + controls |                      65 |             70 |             40 |                            3 |
| Fresh semantic      |                      27 |             30 |             15 |                           10 |
| Earlier encoder     |                      29 |             32 |             16 |                            8 |
| New BGE             |                      31 |             32 |             16 |                            9 |

These are the same old scores on the same old questions, plus a separately reported
new cohort. Existing negatives returning items remain baseline failures; exact
preservation does not make them successes.

Of the twelve missed answers, seven already return results. Empty-only admission
preserves all seven, including the new scroll-anchor symptom miss. The other five
have empty results, but **neither BGE variant ranks any required fact first**:

| Eligible missed query | Plain required rank | Instructed required rank | Plain winner        | Instructed winner   |
| --------------------- | ------------------: | -----------------------: | ------------------- | ------------------- |
| Pagination            |                   8 |                        7 | ranking             | fallback            |
| Generation            |                  11 |                       11 | retention           | retention           |
| Cancellation          |                   7 |                        4 | resource-cleanup    | resource-cleanup    |
| Stream reservoir      |                   2 |                        3 | shortest-path-queue | shortest-path-queue |
| DNS negative cache    |                   2 |                        3 | constant-time-mac   | constant-time-mac   |

Thus no absolute or margin cutoff can turn the registered top-one policies into
a gain. The 48,192 offline policy replay views use actual MiniLM packet previews:
**142 policies are exactly unchanged, while each of the other 50 changes at least
one negative packet**. Some also add unrequired facts to missed positives. No
candidate is promoted, no historical candidate scoring is needed, and none of the
48 new reserved query rows is evaluated, even for baseline calibration.

The direct BGE diagnostic retrievals return many more candidates and can deliver
additional required facts at cutoff 0.45, while also returning items for nearly all
negative queries. Those are not accepted replacement results. The experiment's
registered acceptance unit is the guarded additive policy, not aggregate recall
from a different whole-encoder retrieval policy.

## Reproduction and retained scope

Provision `onnx/model.onnx` as `model.onnx` and `tokenizer.json` from the pinned
[official repository](https://huggingface.co/BAAI/bge-small-en-v1.5/tree/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a),
and verify the hashes above. Build only the two examples in `src-tauri/`:

```sh
cargo build -p verified-code-memory --example evaluate_memory --example score_memory_pairs
```

A development config for the instructed new cohort is:

```json
{
  "dataset_file": "scripts/eval/memory-bge-fresh-cases.json",
  "split": "development",
  "deterministic": true,
  "memory_budget": 1000,
  "short_query_words": 2,
  "short_query_scale": 1,
  "diagnostics": true,
  "diagnostic_pool_limit": 32,
  "pooling": "cls",
  "query_prefix": "Represent this sentence for searching relevant passages: ",
  "document_prefix": ""
}
```

From the repository root, with an absolute model directory:

```sh
ROVIK_MEMORY_EVAL_MODEL="$BGE_MODEL" \
ROVIK_MEMORY_EVAL_DIMENSIONS=384 \
ROVIK_MEMORY_EVAL_REVISION=bge-small-en-v1.5-5c38ec7-cls-instructed-v1 \
ROVIK_MEMORY_EVAL_THRESHOLD=0.45 \
src-tauri/target/debug/examples/evaluate_memory --experiment-config config.json
```

For plain BGE, omit both prefix keys and use a distinct revision. For MiniLM,
omit pooling and prefixes, use its pinned model/revision, cutoff 0.30 and scale
11/12. Repeat on the original (including short-query controls), fresh-semantic and
earlier encoder fixtures. For each policy, match the BGE winner to the MiniLM
pool by ID/hash, apply its accepted MiniLM preview, recompute delivery/ranks from
preview IDs, and run `memory-regressions.mjs` plus the positive-extra audit.
The [machine-readable evidence](long-term-memory-bge-nov10-2026.json) includes every
trial, eligible winner/runner-up scores and native preview, missed-primary ranks,
reference checks and raw artifact hashes.

The scorer's research-only
`--embedding-reference <model-directory> <texts.json> cls|masked_mean` mode emits
normalized vectors and exact token inputs as JSONL for short reference strings.
It rejects references beyond 200 characters or the native 256-token window;
it never silently clips a reference comparison.

Focused checks pass: four embedding unit tests, both explicitly enabled native
MiniLM tests, 26 evaluator tests and five pair-scorer tests. Clippy passes with
`-D warnings` for the library and both changed examples. Broad crate, core and
frontend tests remain skipped as requested. There are no new dependencies,
model assets, application settings or CI downloads.

Correct pooling and the smaller model resolve backend feasibility, but they do
not resolve relevance on these misses. Further sweeps over the same top-one
cosine/margin features have no gain to isolate. Prioritize source-grounded capture
accuracy and independently judged relevance or paired-agent usefulness before
widening admission to already nonempty results. Repeated exposed synthetic queries
are regression evidence, not fresh generalization evidence.
