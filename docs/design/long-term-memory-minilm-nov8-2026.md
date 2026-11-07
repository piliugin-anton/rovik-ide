# MiniLM recall evaluation and offline backend

Chroma's MiniLM archive now downloads successfully from
`chroma-onnx-models.s3.amazonaws.com`. Its SHA-256 matches the predeclared
protocol: `913d7300ceae3b2dbc2c50d1de4baacab4be7b9380491c27fab7418616a16ec3`.
Both extracted model and tokenizer were checked against that archive.
This supersedes the earlier report's download blocker; the frozen protocol's
execution-status field records the earlier state.

The experiment ran on `improve-memory-recall-capacity`, based on `ad3910c`.
It used the unchanged original cases, hard-negative controls and ninety fresh
cases authored before MiniLM trials. Both title-plus-claim and claim-only
representations were evaluated at fifteen registered cosine thresholds each.
Selection used development only and rejected every individual lost baseline
answer or newly injected no-match case. The selected representation was
**title-plus-claim at 0.30**, supplying nine additional development answers.
Only that selection was scored on holdout.

| Dataset and split        | Lexical/file delivered recall | MiniLM hybrid delivered recall | Baseline → hybrid no-match injection |
| ------------------------ | ----------------------------- | ------------------------------ | ------------------------------------ |
| Original development     | 58/70 (82.86%)                | 64/70 (91.43%)                 | 2/22 → 2/22                          |
| Fresh development        | 24/30 (80%)                   | 27/30 (90%)                    | 10/15 → 10/15                        |
| Original exposed holdout | 24/30 (80%)                   | 26/30 (86.67%)                 | 3/10 → 3/10                          |
| Fresh holdout            | 23/30 (76.67%)                | 28/30 (93.33%)                 | 7/15 → 7/15                          |

There were no lost baseline answers or newly injected no-match cases in the
selected configuration on either split. The new original holdout answers concern
deduplication and isolation. The fresh holdout gains concern CSRF, HTTP cache
validation, filesystem watching, certificate chains and transactional outboxes.
Fresh holdout MRR rises from 0.7667 to 0.8528 and nDCG@8 from 0.7667 to 0.8731.
The [machine-readable result](long-term-memory-minilm-nov8-2026.json) retains all
development trials and selected holdout case outcomes, plus dataset, evaluator,
runner, model, tokenizer and worker hashes.

These are synthetic retrieval results. The original holdout was previously
exposed; the fresh holdout was authored before these model trials but is not
independent human evaluation. Existing lexical false injections persist, well
above the proposed 5% release target. Live-agent task quality, multilingual
quality and corpus-scale latency were not measured. Semantic-only and gated
policies listed as planned in the protocol were not run; this experiment tests
the implemented hybrid policy. Production defaults remain unchanged.

## Native implementation

The Python worker and its Python unit tests have been replaced by
`embedding::OnnxMiniLm`, a native Rust encoder using tract and tokenizers.
It compiles and reuses the local ONNX graph, bounds input to 500 characters
and 256 tokens, excludes padding from pooling, and emits normalized
384-dimensional vectors. It does not start Python or require a separately
installed ONNX runtime. The original research results above remain a record of
the Python ONNX Runtime experiment; native numerical parity is checked against
a frozen reference vector from that experiment.

When semantic memory is enabled, the core prepares the native model and
backfills in the background, preserving lexical/file retrieval during setup or
failure. The pinned model and tokenizer are downloaded once from the dedicated
`memory-minilm-l6-v2-v1` release in this GitHub repository. This release is
separate from app releases and is not recreated by CI. Exact file sizes and
SHA-256 hashes are checked, a complete staged directory is installed atomically,
and subsequent runs reuse the local copy. Downloads follow the existing
network/proxy settings. Native vector identities use their own revision.

Enable with `memorySemantic: {"mode": "shadow", "threshold": 0.3}` in the
project settings, then opt into `hybrid` after evaluating against your corpus.
No interpreter or manual model path is needed. An optional absolute `modelPath`
can use the exact pinned files provisioned offline. Existing Python fields are
ignored. See the main README for settings, retries and model-cache behavior.

## Native reproduction

Build from `src-tauri` and set `ROVIK_MEMORY_EVAL_MODEL` to the directory containing
the pinned `model.onnx` and `tokenizer.json`:

```sh
cargo test --locked -p verified-code-memory
cargo test --locked -p verified-code-memory native_minilm -- --ignored
cargo test --locked -p rovik-core memory_semantic
cargo build --locked -p verified-code-memory --example evaluate_memory
ROVIK_MEMORY_EVAL_DIMENSIONS=384 \
ROVIK_MEMORY_EVAL_REVISION=chroma-minilm-l6-v2-913d7300-title-claim-native-v1 \
ROVIK_MEMORY_EVAL_THRESHOLD=0.3 \
  target/debug/examples/evaluate_memory
```

The real-model test is ignored by default because model files are provisioned
separately. It compares the native vector with a frozen reference and verifies
paraphrase recall through `SemanticIndex`. Deterministic unit tests check
padding, invalid vectors, incomplete downloads, tampering, retry and cached
reuse. The earlier Python experiment runner remains historical research tooling,
not a runtime dependency or required application setup.

Linux validation ran one crate at a time: 86 memory unit/integration tests,
the memory doctest, the explicitly enabled real-model reference test and six
evaluator tests passed. Core validation passed 2,576 unit tests initially and
20 integration tests; its one remaining Go-dependent unit test passed separately
after installing the actual Go compiler (the environment's `/usr/bin/go` was a
board game). The core doctest command passed with its existing ignored example.
Core command/browser tests required execution outside the nested sandbox and
a private temporary directory. Clippy passed with four existing core warnings
outside these changes, and memory Clippy passed with warnings denied. All
1,044 frontend tests, type checking and formatting checks passed.
