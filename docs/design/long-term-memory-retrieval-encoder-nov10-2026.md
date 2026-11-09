# Retrieval-trained encoder comparison

Baseline: `9d2d3515f5e26d7c4f89a8ba51f7eafaa80f94f7` on
`ltm-improvements-nov10-2026`.

All eleven tested E5 replacement policies fail development selection. Low
cutoffs recover several implicit answers but change unrelated-query packets and
worsen existing ranks; higher cutoffs lose previously complete answers. Keep
production MiniLM, its threshold, assets and release process unchanged. This
follow-up retains a frozen 96-case suite, evaluation-only role formatting and
the failed results, without claiming a production recall improvement.

## Why this experiment

The [diagnostic report](long-term-memory-diagnostics-nov10-2026.md) found that all
eight remaining original/fresh implicit misses have expected primary cosine
0.147–0.222, with a higher-scoring eligible competitor. Additional title/claim
views and grounded cause fields had not passed regression selection. The next
question was whether a retrieval-trained encoder could improve separation while
preserving lexical/file signals, rather than lowering MiniLM's cutoff again.

The [protocol](../../scripts/eval/memory-retrieval-encoder-protocol.json) freezes
new questions and the eleven-cutoff grid before either model scores them.
Development selection uses the unchanged
[per-record comparator](../../scripts/eval/memory-regressions.mjs). Existing
primary/required claim identity and hashes, complete delivery and retrieved
ranks remain protected. Negative returned/rendered/full ID arrays must match
exactly, including order. Scope, verification, tag and caller filters, eight
results, a 1,000 estimated-token allowance and successful embeddings remain
requirements. A higher aggregate score alone cannot pass.

## Model and evaluation implementation

Use the official [intfloat/e5-small-v2](https://huggingface.co/intfloat/e5-small-v2/tree/ffb93f3bd4047442299a41ebb6fa998a38507c52)
ONNX artifact at revision `ffb93f3bd4047442299a41ebb6fa998a38507c52`, not a
third-party export. The 133,093,468-byte model's SHA-256 is
`4b8205be2a3c5fc53c6534d76a2012064f7309c162b806f2889c6ec8ec4fdcba`;
the 711,396-byte tokenizer's SHA-256 is
`d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66`.
The model matches official LFS metadata. The tokenizer also matches Git blob
`688882a79f44442ddc1f60d70334a7ff5df0fb47`.

The existing native loader executes this compatible float32 BERT graph: three
integer input tensors, 384-dimensional token output, attention-masked mean
pooling and L2 normalization. Upstream's pooling/config metadata agrees. This
establishes native graph compatibility, **not independent numerical parity with
an upstream PyTorch reference**. No Python runtime, dependency, application model
setting or download/release change is introduced.

The Rust `evaluate_memory` example now accepts paired `query_prefix` and
`document_prefix` experiment fields. E5 uses `query: ` for retrieval queries and
`passage: ` for indexing. Its memoization keys include the actual prefixed input,
so identical raw query/document strings cannot share vectors. The example
switches roles before backfill and before retrieval for every temporary fixture;
diagnostic queries use the query role too. Missing, empty, nonstring or oversized
prefixes fail. Prefixed experiments require native inference and reject vector
import/export, whose existing format does not encode roles. Default MiniLM
runs omit both fields.

The loader retains its 500-character and 256-token limits; the prefix wrapper
conservatively advertises 491 payload characters. All scored fixture documents
are at most 154 characters, and queries including their trailing newline are
at most 107, so this comparison does not test long-input truncation. E5's upstream
512-token capacity is not evaluated. This short English study does not establish
multilingual behavior or application latency/cost.

## Fresh cases and baseline

The [new fixture](../../scripts/eval/memory-retrieval-encoder-cases.json) has 16
newly authored synthetic mechanisms, split by incident: eight development and
eight reserved. Each incident has symptom/procedure questions plus literal and
file controls. Each split also has 16 no-match requests with a written rationale:
**32 positive and 16 negative cases per split, 96 total**. No questions, source
claims or labels were revised after scoring.

Development topics include journal durability, lease fencing, reservoir sampling,
Merkle reconciliation, negative DNS caching, observed-remove sets, SQL parameters
and constant-time tag comparison. Reserved topics cover different mechanisms.
All source facts are equally fixture-verified, and both splits' facts are present
in the retrieval corpus. These are retrieval examples, not measured capture from
actual repository episodes. Negative labels are assistant-authored corpus review,
not independent human adjudication. The existing evaluator also stores its
synthetic expected placeholder for a negative case; these are controls under
that fixture contract, not a precision estimate.

| Development cohort                 | Complete answers | Implicit answers | No-match cases returning any record |
| ---------------------------------- | ---------------: | ---------------: | ----------------------------------: |
| Original plus short-query controls |            65/70 |             9/14 |                                3/40 |
| Fresh semantic                     |            27/30 |             7/10 |                               10/15 |
| New encoder cases                  |            29/32 |            13/16 |                                8/16 |

The new symptom subset is 5/8, procedure 8/8, literal 8/8 and file 8/8.
Multiple questions share each incident: 32 positives do not mean 32 independent
tasks. The existing original/fresh native baselines reproduce all 155 accepted
case views in every non-timing field, including non-timing telemetry.

## Cutoff sweep

E5 uses short-query scale 1, rather than transferring MiniLM's 11/12 calibration.
MiniLM remains at cutoff 0.30. Lexical/file retrieval, original short-query
controls, deterministic 36-hour fixture age and packet/result limits stay fixed.
Each cell below reports complete positive delivery; the second table keeps
unrelated-query injections separate. All rows fail the comparator.

| E5 cutoff | Original /70 | Fresh /30 | New /32 |
| --------: | -----------: | --------: | ------: |
|     0.700 |           67 |        29 |      31 |
|     0.725 |           67 |        29 |      31 |
|     0.750 |           67 |        29 |      31 |
|     0.775 |           65 |        27 |      31 |
|     0.800 |           62 |        25 |      29 |
|     0.825 |           59 |        24 |      28 |
|     0.850 |           58 |        24 |      28 |
|     0.875 |           58 |        24 |      28 |
|     0.900 |           58 |        24 |      28 |
|     0.925 |           58 |        24 |      28 |
|     0.950 |           58 |        24 |      28 |

| E5 cutoff | Original no-match /40 | Fresh no-match /15 | New no-match /16 |
| --------: | --------------------: | -----------------: | ---------------: |
|     0.700 |                    40 |                 15 |               16 |
|     0.725 |                    40 |                 15 |               16 |
|     0.750 |                    39 |                 15 |               16 |
|     0.775 |                    12 |                 15 |               16 |
|     0.800 |                     4 |                 12 |               13 |
|     0.825 |                     2 |                 10 |                7 |
|     0.850 |                     2 |                 10 |                7 |
|     0.875 |                     2 |                 10 |                7 |
|     0.900 |                     2 |                 10 |                7 |
|     0.925 |                     2 |                 10 |                7 |
|     0.950 |                     2 |                 10 |                7 |

At cutoff 0.725, original gains are pagination, provenance and fallback, but
migration loses its complete answer. Fresh gains are rate limits and optimistic
locking. New gains are journal-durability and negative-DNS-cache symptom questions:
new implicit delivery becomes 15/16, while literal/file controls stay 16/16.
Existing primary ranks nevertheless worsen on five new positive cases, including
lease-fencing and reservoir file controls. Original and fresh also regress ranks.

At that cutoff, all 71 no-match packet traces change. Of these, 50 are newly
nonempty queries; 21 already returned something under MiniLM and change their
identities or order. Counts of comparator error fields are not counts of newly
injected cases. At 0.825, fresh negative packets match the baseline exactly, but
three previously complete fresh answers are lost. Lower injection counts at high
cutoffs do not excuse complete-answer/rank losses elsewhere.

There are **2,436 native policy views**: 203 MiniLM baselines and 2,233 E5 views
(11 cutoffs × 203 development cases). These are 11 policies evaluated on three
cohorts, not 33 independently selected models. Every policy fails development;
no candidate proceeds to reserved or full historical/oversized evaluation. New
reserved questions remain unscored. No acceptance gate or baseline was weakened.

## Evidence, checks and reproduction

The [machine report](long-term-memory-retrieval-encoder-nov10-2026.json) records
input/model/output hashes, each trial's categories, gains, losses and comparator
errors, exact changed packet traces and accepted-baseline parity. Trace indices
refer to its shared `trace_pool`; repeated identities are stored once. Raw native
outputs and temporary model files remain local research artifacts.

All 21 evaluator unit tests pass, including new contracts for role-separated
memoization and invalid/unreplayable prefix configurations. Focused example
clippy passes with warnings denied; changed Rust and documentation/JSON formatting
are checked. Broader crate/core/frontend suites remain skipped under the user's
instruction. No production library/core source changed.

For reproduction, download `onnx/model.onnx` and `tokenizer.json` from the pinned
upstream revision into an external directory and verify the hashes above. Build
`cargo build -p verified-code-memory --example evaluate_memory` from `src-tauri`.
A new-suite E5 experiment config is:

```json
{
  "dataset_file": "scripts/eval/memory-retrieval-encoder-cases.json",
  "split": "development",
  "deterministic": true,
  "memory_budget": 1000,
  "short_query_words": 2,
  "short_query_scale": 1,
  "query_prefix": "query: ",
  "document_prefix": "passage: "
}
```

Run the built example from repository root with `--experiment-config <path>` and
`ROVIK_MEMORY_EVAL_MODEL=<external directory>`,
`ROVIK_MEMORY_EVAL_DIMENSIONS=384`,
`ROVIK_MEMORY_EVAL_REVISION=e5-small-v2-ffb93f3-query-passage-native-research` and
`ROVIK_MEMORY_EVAL_THRESHOLD=<grid value>`. For MiniLM, omit prefixes, use the
pinned Chroma model/revision, scale 11/12 and cutoff 0.30. Original uses
`memory-cases.json` plus `memory-short-query-controls.json` as `controls_file`;
fresh uses `memory-semantic-fresh-cases.json`. Compare native JSON outputs using
`node scripts/eval/memory-regressions.mjs baseline.json candidate.json`.

A whole-encoder replacement is not supported by this evidence. A distinct next
retrieval study could inspect E5 candidate separation for an additive rescue that
preserves existing MiniLM records and ranks. It must first pass the same frozen
negative traces; agreement or a raw cosine cutoff alone is not an established
gate. Real capture accuracy and paired memory-on/off agent tasks remain separate
priorities, and this study supplies no evidence of gains there.
