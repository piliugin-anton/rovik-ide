# Query-document relevance reranker

Baseline: `960612e035a41585080f351608ff6f273c325e88` on
`ltm-improvements-nov10-2026`.

The tested cross-encoder does not produce a safe recall improvement. Nonnegative
cutoffs leave every case unchanged. Lower cutoffs recover cancellation and
reservoir sampling, but every gaining policy changes negative packets. Scoring
stored bodies without titles does not fix separation. Reject all 213 policies;
keep production MiniLM, its settings, dependencies, provisioning and release
assets unchanged.

This follows the rejected
[encoder-agreement study](long-term-memory-additive-encoders-nov10-2026.md).
The new code is evaluation-only: a native pair scorer and bounded diagnostic
pool export. No second model is enabled in the application or downloaded by CI.

## Model provenance and native verification

Use the official
[cross-encoder/ms-marco-MiniLM-L6-v2](https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2/tree/233902d25c440f23af6f7d6e94d2946bac0bee0a)
float32 ONNX artifact at revision `233902d25c440f23af6f7d6e94d2946bac0bee0a`.
The 91,011,230-byte model matches upstream LFS SHA-256
`5d3e70fd0c9ff14b9b5169a51e957b7a9c74897afd0a35ce4bd318150c1d4d4a`.
The tokenizer SHA-256 is
`d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66`.

`score_memory_pairs` uses existing Rust `tract-onnx` and `tokenizers` dependencies.
It encodes a genuine BERT query/document pair, including document segment IDs,
with longest-first truncation at 512 tokens and no padding for a single example.
Inputs are matched by their graph names, not assumed position. It accepts one
finite float32 classification logit shaped `[1,1]`; there is no embedding pooling,
sigmoid or probability interpretation. Missing/unsupported inputs, output shapes,
nonfinite logits, empty text and duplicate pair IDs fail explicitly.

Before fixture inference, the native scorer reproduces both examples in the
pinned model's README:

| Upstream example  | Published logit | Native logit | Absolute difference |
| ----------------- | --------------: | -----------: | ------------------: |
| Berlin population |        8.607138 |    8.6071415 |           0.0000035 |
| Berlin museums    |       -4.320078 |    -4.320077 |           0.0000010 |

The preregistered tolerance is 0.01. A separate process interleaving the population
reference and two low-scoring coding pairs reproduces those coding logits
exactly. This checks named inputs, paired formatting, classification extraction
and repeatability; it is not comprehensive numerical parity against PyTorch.
The low coding scores are observed model outputs, not a missing inference or an
embedding fallback.

## Frozen development pools and admission

The [initial protocol](../../scripts/eval/memory-cross-encoder-protocol.json)
precedes cross-encoder fixture scoring. Reuse original, fresh and new encoder
development cases, including all existing negatives: **203 views, 132 positive
and 71 negative**. No query, source fact, expected identity or label changes.
MiniLM retains cutoff 0.30, short-query scale 11/12, eight returned records,
1,000 estimated tokens and deterministic 36-hour fixture age.

New `diagnostic_pool_limit: 32` exports the first 32 eligible distinct primary-index
records in actual MiniLM cosine/UUID order, including scores below admission.
It requires `diagnostics: true` and accepts integers 1–128. The default remains
the query's existing limit. This is a diagnostic bound, not a retrieval/result
limit. Scope, verification, tags, caller visibility, current claim hash and
exact-copy rules apply before the bound.

Each pool entry now exports the stored `title + newline + content` as `document`,
its claim identity/hash and the existing native append packet preview. Candidate
ordering and export do not use expected labels. There are at most **20 eligible
records** in these cases, so no pool truncates and all 11 missed primaries are
present. No expected record is inserted specially. Across the 203 pools there
are 3,884 candidate appearances and 2,792 distinct query/document pairs.

Admission considers only **empty MiniLM results without query files**. Rank the
pool by cross-encoder logit, then UUID, and require a runner-up. Append at most
one winning source record when its logit and top-versus-second margin pass the
policy and the native packet preview accepts it. Do not advance to another
record on rejection. An accepted preview supplies the complete candidate while
preserving all previously complete incumbents and all returned positions under
the original packet/result bounds.

Only five of the 11 remaining positive misses have empty baselines. Six already
return another record and cannot improve under this admission rule. Previously
answered positive queries retain their exact results. A separate conservative
acceptance audit flags any new positive ID outside that case's required facts;
those labels do not enter candidate admission. This audit is explicit in the
follow-up protocol. Every gaining policy already fails the unchanged comparator,
so the audit does not decide any otherwise safe gain.

## Experiments and results

Initial title/body scoring tests raw logit floors 0–8 and margins 0–4: **45
policies**. Every one leaves all cases unchanged. Published reference values do
not provide a suitable cutoff for these coding snippets; expected implicit
source facts frequently score near -11.

The [follow-up protocol](../../scripts/eval/memory-cross-encoder-followup-protocol.json)
registers two adaptive development refinements after observing that failure:

1. Reuse title/body scores with 12 lower logit floors and seven margin floors.
2. Score the exact stored body without its title, using the same lower-cutoff
   grid. The delivered record still retains its original title/content/hash.

Each refinement has 84 policies. Questions and claims stay frozen, and no heldout
outcome informs either refinement. Body-only inference uses a further 2,792
pairs, with full delivered-record packet previews reused from the same store.

| Trial                             | Policies | Policies with complete-answer gains | Safe policies with gains | Fewest changed negatives among gaining policies |
| --------------------------------- | -------: | ----------------------------------: | -----------------------: | ----------------------------------------------: |
| Title/body, initial cutoffs       |       45 |                                   0 |                        0 |                                               — |
| Title/body, low-logit calibration |       84 |                                   9 |                        0 |                                              11 |
| Body only, low-logit calibration  |       84 |                                  15 |                        0 |                                               6 |

The title/body trial has two possible recovered identities:
`cancellation-implicit` and `retrieval-stream-reservoir-implicit-symptom`.
Body-only scoring recovers only the latter. The existing comparator protects
primary/required identities and hashes, complete delivery and ranks, exact
negative returned/rendered/full ID order, eligibility, embedding success and
packet/result limits. It is unchanged.

The cleanest gaining title/body policy, floor -11 and margin 0.1, recovers the
reservoir case, **new development 29/32 → 30/32**, while changing 11 negative
packets. It adds no unrequired positive ID. Original/fresh complete delivery
stays 65/70 and 27/30; original negative injection rises 3/40 → 13/40 and new
injection 8/16 → 9/16. All 11 are previously empty negative results.

The fewest-negative body policy, floor -10.5 and margin 0.1, also reaches 30/32
on new development, but changes six negative packets and adds an unrequired MAC
fact to the still-missed DNS-cache query. These are rejected measurements, not
current production scores.

Of 213 policies, 141 leave every case unchanged. Two further title/body policies
pass the comparator without a gain but add an unrequired positive ID. The other
70 policies change negatives. Passing the per-fact/negative comparator alone
still does not judge relevance of every extra positive record; this is why the
additional source-identity audit is retained.

## A cutoff-independent counterexample

The frozen TLS-renewal no-match request receives an unrelated isolation fact.
It exceeds each correctly ranked empty-baseline answer on both admission
features, and the native renderer accepts it in full:

| Scored document view | Proposed recovery  | Recovery logit | Recovery margin | TLS-negative logit | TLS-negative margin |
| -------------------- | ------------------ | -------------: | --------------: | -----------------: | ------------------: |
| Title/body           | cancellation       |     -11.324035 |        0.082668 |         -10.149654 |            0.370562 |
| Title/body           | reservoir sampling |     -10.756563 |        0.160485 |         -10.149654 |            0.370562 |
| Body only            | reservoir sampling |     -10.476465 |        0.170308 |          -9.386696 |            0.531855 |

The negative packet is 63 estimated tokens; the correct cancellation and
reservoir packets are 69 and 79. All have empty baselines, free result slots,
no query files and fit within 1,000 tokens. Any monotone lower bounds on logit
and winner margin admitting these recoveries also admit this negative, even
outside the tested grids. Packet rollback cannot fix the separation failure.
This conclusion concerns this model, document views and admission features; it
does not rule out another relevance model or candidate selector.

## Verification and reproduction

There are **5,584 main native pair inferences**, plus five upstream-reference and
consistency inferences. The maximum paired sequence is 62 tokens for title/body
and 54 for body-only, so none reaches the 512-token truncation limit. Three
MiniLM diagnostic runs provide 203 case views and preserve every accepted
non-timing case field, including non-timing telemetry. The 213 policies perform
**43,239 case replays** using native scores and real native packet previews;
these are not 43,239 separately executed retrievals.

All 24 evaluator and three scorer unit tests pass, run as separate targets.
Focused clippy passes for each example separately with warnings denied. Changed
Rust and Markdown/JSON formatting are checked. Broader crate/core/frontend tests
remain skipped under the user's instruction. Production library/core sources
and dependency/lock files are unchanged. No candidate proceeds to historical,
oversized or heldout scoring after development rejection. Reserved questions
remain unscored.

The [machine report](long-term-memory-cross-encoder-nov10-2026.json) records both
protocols, fixture/model/source/executable/output hashes, per-policy gains and
negative/positive-extra audits, missed-fact ranks, native packet traces and the
counterexamples. Raw pair inputs, logits, temporary model files and orchestration
remain local research artifacts. The newer no-match labels remain
assistant-authored corpus review, not independently judged precision evidence.
The cases are exposed synthetic retrieval examples with correlated queries per
incident; no capture, live-agent, multilingual or latency improvement is claimed.

For reproduction, build the examples from `src-tauri`:

```sh
cargo build -p verified-code-memory --example evaluate_memory
cargo build -p verified-code-memory --example score_memory_pairs
```

Run the existing MiniLM evaluation commands with `diagnostics: true` and
`diagnostic_pool_limit: 32`. Export query/primary candidate documents without
using expected labels. The scorer accepts a JSON array of unique text pairs:

```json
[
  {
    "id": "reference",
    "query": "How many people live in Berlin?",
    "document": "Berlin is well known for its museums."
  }
]
```

From repository root, run:

```sh
src-tauri/target/debug/examples/score_memory_pairs \
  --model /path/to/verified/pinned-model --pairs /path/to/pairs.json
```

Each output JSONL row contains `id`, raw `logit` and paired `tokens`. Download
`onnx/model.onnx` and `tokenizer.json` from the pinned upstream revision into an
external directory, verify the hashes above, and first reproduce both upstream
reference logits. Apply the exact protocol grids to the independently ranked
scores, respecting empty/no-files admission and native append guards. Compare
packet identity/rank/delivery traces with the existing comparator, then audit
unrequired positive additions separately.

The next model comparison should change the relevance training/domain rather
than repeat score thresholds on this MS MARCO passage ranker. A coding- or
instruction-oriented relevance model would be a distinct hypothesis, evaluated
against the same full pools and stronger negatives. Captured problem/remedy
accuracy and paired memory-on/off agent usefulness remain separate priorities;
these experiments do not establish gains in either.
