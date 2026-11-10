# Instruction-aware relevance experiment

Baseline: `ff9029fd0dc421ec91dc0668e288a9d2bf9f29c1` on
`ltm-improvements-nov10-2026`.

The retained research candidate recovers **one development answer**, pagination,
with no measured regression across **5,796 policy views**, including **2,020
negative views**. Heldout results are unchanged. A lower-cutoff candidate recovers
three answers on the small development suites but fails 11 historical negative
cases and is rejected.

**Keep production MiniLM unchanged.** The measured gain is narrow, the current
application backend cannot execute this model, and its cost does not justify
integration on this evidence. This commit adds reusable evaluation support and
records the complete experiment; it does not improve application recall by
enabling a new default model.

## Model and compatibility

Use instruction-aware `Qwen/Qwen3-Reranker-0.6B` at upstream revision
`e61197ed45024b0ed8a2d74b80b4d909f1255473`. The evaluated ONNX artifact is the
**community conversion**
[onnx-community/Qwen3-Reranker-0.6B-ONNX](https://huggingface.co/onnx-community/Qwen3-Reranker-0.6B-ONNX/tree/9995c50e2310679108a55f5ccd16ba8be9f17c20)
at revision `9995c50e2310679108a55f5ccd16ba8be9f17c20`, variant
`onnx/model_q4.onnx`. It is not an official upstream ONNX export.

The 995,235,866-byte mixed-precision graph matches repository LFS SHA-256
`787193952a9aa58374381ec7647b160ac6c200a386dbfc3bc998a3c458081330`.
The tokenizer matches the pinned upstream tokenizer byte-for-byte, SHA-256
`aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4`.

Actual graph inspection finds 197 `com.microsoft.MatMulNBits` nodes. The model
has 59 inputs, including 28 layers of key/value caches, and 57 outputs, including
full vocabulary logits. This interface differs from the application's existing
three-input BERT embedding/classification paths. Native CPU inference succeeds
with externally provisioned **ONNX Runtime Node 1.31.1**, three intra-op threads,
one inter-op thread and the CPU provider. Both runtime packages' npm SHA-512
integrities are verified. No runtime dependency, lock file, model asset, application
setting, download behavior or release pipeline changes.

The weights were streamed into memory and SHA-256 checked before session creation;
they were not committed or uploaded. Initial resident process RSS after loading
was approximately **2.43 GB**. The three offline scoring jobs took 319, 285 and
574 seconds, excluding download/setup and audits. These are research job costs,
not application latency measurements. Application backend, packaging, lifecycle,
resource limits and live-agent usefulness remain unvalidated.

## Prompt, references and numerical checks

Native Rust tokenization follows the upstream recipe: separately encode the
system/user prefix, the `<Instruct>`, `<Query>`, `<Document>` body, and the assistant
empty-think suffix. Do not add padding or silently truncate. Research inputs have
a 512-token cap; the longest measured prompt is 222 tokens. Read **last-position**
logits for `yes` (9693) and `no` (2152), then compute `log_odds = yes - no` and
`probability = sigmoid(log_odds)`. There is no generated text or embedding pooling.
The model only ranks stored source records; it never supplies a new memory claim.

The frozen coding instruction is:

> Given a coding task, retrieve a stored memory fact only if it directly answers
> the task or provides the required corrective action. Reject facts that are
> merely about the same broad topic.

Before fixture inference, both upstream README positive passages and their
crossed negatives pass behavioral checks using the upstream web-search instruction:

| Reference                      | Native yes probability |
| ------------------------------ | ---------------------: |
| China query, Beijing passage   |               0.999522 |
| Gravity query, gravity passage |               0.999635 |
| China query, gravity passage   |               0.000006 |
| Gravity query, Beijing passage |               0.000094 |

These are behavioral references, **not independent numerical parity with upstream
PyTorch or an assessment of Q4 quantization error**. Coding-domain values are
ranking signals; their normalized binary probabilities are not calibrated
confidence. The recovered pagination fact scores below probability 0.5.

To make CPU evaluation practical, equal queries share their exact common token
prefix and immutable float32 KV tensors. Attention spans the whole prefix/tail,
and position IDs retain their absolute offsets. Every source token and suffix
is preserved. Cached and uncached scores match exactly on all four reference
pairs and 32 additional pair audits: eight selected by SHA order from each scoring
phase, plus eight covering proposed gains and the strongest development negative.
Decision/rank boundaries remain outside the registered 0.001 audit tolerance.

## Frozen selection and failed calibration

The [initial protocol](../../scripts/eval/memory-instruction-reranker-protocol.json)
precedes fixture inference. Reuse the frozen original, fresh and new encoder
suites, their source claims and negatives. MiniLM stays at cutoff 0.30, scale
11/12, eight results, fixed 36-hour fixture age and the existing packet allowances.
Candidate pools are the first 32 eligible distinct primary-index records in
MiniLM cosine/UUID order. Actual eligible pools contain at most 20 records;
no pool truncates and no expected fact is inserted specially.

Only empty MiniLM results without query files qualify. Require a runner-up,
winning log-odds and winner-minus-runner-up margin floors, and an accepted native
append preview. Do not advance to a different record on rejection. Existing
nonempty results stay exact. Required IDs, answerability and categories are
scoring labels, never admission features. A conservative extra audit rejects
new positive IDs outside that case's required facts.

The first 77 policies select log-odds floor **-2**, margin **2**. On 203 development
views, this recovers only `pagination-implicit`: original delivery **65/70 → 66/70**,
fresh **27/30 → 27/30**, new **29/32 → 29/32**. All 71 negative packets stay exact.
Only five of the 11 missed positive answers have empty baselines; six are outside
this admission policy. Pagination's winner scores -1.122531, with margin 2.853205.

The [follow-up protocol](../../scripts/eval/memory-instruction-reranker-followup-protocol.json)
registers 49 lower-cutoff policies after seeing two additional correctly ranked
facts below the first grid, and before historical or heldout candidate inference.
The selected floor **-4**, margin **1** recovers pagination, reservoir sampling and
negative DNS caching on the small development suites. New delivery becomes 31/32,
with no core-suite negative changes or unrequired positive additions.

That candidate fails the historical development gate on **11 negative case
views**: four dense full-pool controls, three identifier-adjacent/generic controls
and four snake-identifier controls. These are correlated constructed controls,
not 11 independent agent tasks. It receives no candidate heldout comparison.
The earlier independently frozen -2/2 policy passes the historical gate and is
the only candidate advanced. No threshold or instruction is changed from
historical or heldout feedback.

## Final comparison

All 42 historical profiles retain lexical/hybrid arms and their existing
1,000/2,000 estimated-token budgets. Add the new encoder suite's hybrid development
and heldout arms. Native collection selects relevant query rows through `case_ids`
while keeping the full original corpus and sibling distractors. Unknown,
duplicate or out-of-split IDs fail. No fixture or expected record is removed.

The source evidence consists of 203 reused native development views and 146 new
native views: 29 additional historical development queries, 69 historical heldout
queries and all 48 new-suite heldout baseline cases. The 98 newly collected old
cases match accepted outputs in every non-timing field after removing diagnostics.
Other query rows are inactive under the empty/no-files rule and retain their
accepted baseline records exactly. This is model-score/native-packet **policy
replay**, not a new production retrieval implementation.

The unchanged [comparator](../../scripts/eval/memory-regressions.mjs) protects
primary/required identities and claim hashes, complete delivery and ranks, exact
negative returned/rendered/full ID order, eligibility, embedding success and
packet/result bounds. The retained candidate passes all **5,796 compared views**;
only pagination's development hybrid view changes. The other **5,795** preserve
all non-timing case fields. All **2,020 negative views** remain exact. Lexical
views remain unchanged.

| Core cohort       | Development before → after | Heldout before → after |
| ----------------- | -------------------------: | ---------------------: |
| Original          |              65/70 → 66/70 |          26/30 → 26/30 |
| Fresh             |              27/30 → 27/30 |          28/30 → 28/30 |
| New encoder cases |              29/32 → 29/32 |          30/32 → 30/32 |

The original heldout denominator differs from development. These cohorts and
repeated budget/policy views are not interchangeable sample sizes. No heldout
answer improves. New-suite heldout MiniLM outcomes are measured for the first
time in this study; their questions were frozen before earlier encoder trials.

There are **1,989 main pair inferences**, 40 auxiliary reference/audit pair
inferences and 107 additional prefix-only forwards. Raw model decisions for
heldout cases are used only after historical selection. The lower-cutoff trial's
two additional development gains are rejected, rather than retained through a
weakened regression gate. All prompts fit the research cap; no unsupported-input
fallback is needed in these measured cases.

## Code, checks and reproduction

Evaluation support adds:

- `score_memory_pairs --tokenize-instruction-pairs` for exact upstream-style
  prompts and label IDs; bounded mode reports unsupported pairs without clipping.
  If any pool candidate is unsupported, retain the entire legacy query result.
- `memory-instruction-scoring.mjs` for named CPU inputs, empty/prefix KV caches,
  full attention/positions and finite final-position label extraction.
- `case_ids` to collect selected query rows without changing fixture contents.

All 25 evaluator, five pair-scorer and four Node helper tests pass. Rust example
clippy targets run separately with warnings denied. Changed Rust/Markdown/JSON
formatting is checked. Broader crate/core/frontend tests remain skipped under
the user's instruction. The Node tests use tensor stubs and run through
`npm run test:eval`; they never load weights or require ONNX Runtime. Production
library/core sources, dependency declarations and lock files remain unchanged.

The [machine report](long-term-memory-instruction-reranker-nov10-2026.json) records
protocols, input/model/source/output hashes, all selection trials, rejected
historical cases, the frozen heldout policy, full profile audits, per-pair scores,
append traces and numerical checks. Legacy input hashes use raw file bytes;
new protocol hashes use canonical parsed JSON, with encodings kept distinct.
Temporary model/runtime files and orchestration stay outside the repository.

To reproduce, verify the pinned tokenizer and graph hashes, provision native
`onnxruntime-node@1.31.1` externally, and build the existing Rust examples. Export
pairs from native `evaluate_memory` with `diagnostics: true`,
`diagnostic_pool_limit: 32`, optionally `case_ids`. Tokenize with:

```sh
src-tauri/target/debug/examples/score_memory_pairs \
  --tokenize-instruction-pairs /path/to/tokenizer pairs.json instruction.txt
```

Create a CPU ONNX Runtime session for the pinned graph. Use the exported helper's
`instructionFeeds` and `instructionScore` around `session.run(feeds, ['logits'])`.
Verify behavioral references before fixture scoring, then compare full-prefill
and cached scores. Apply the exact frozen gates to source IDs and their native
append previews, retaining legacy results on any unsupported input or packet
rejection. Apply the unchanged comparator and separate positive-extra audit.

This is exposed synthetic retrieval evidence, with assistant-authored no-match
judgments. It does not establish precision, capture accuracy, multilingual
behavior, live-agent usefulness or broad recall improvement. Architecture,
training, capacity and instruction all differ from earlier models, so this
comparison does not isolate which factor helps. A smaller native-compatible
instruction-conditioned model is a more practical next comparison; new policies
will need fresh reserved queries because these heldout outcomes are now exposed.
