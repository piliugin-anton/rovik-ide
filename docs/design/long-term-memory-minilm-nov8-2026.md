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

## Implementation

`LocalSentenceEmbedder` now recognizes a local directory containing
`model.onnx` and `tokenizer.json` as a MiniLM export and requires 384 dimensions.
Other model directories retain the sentence-transformers backend. The same
advanced project settings and persistent offline worker are used; no model
download occurs during retrieval. Model/provider failure still falls back to
lexical retrieval through the existing core path.

The bundled Python encoder uses one CPU thread, bounds input to 500 characters
and 256 tokens, excludes padding from mean pooling, and L2 normalizes the output.
Non-finite, zero and wrong-dimensional vectors fail. The evaluation imports this
same encoder so calibration cannot silently use different pooling or truncation.
No Rust dependency, database migration or protocol change is needed.

## Reproduction and verification

Provision the checked archive separately and extract its `onnx` folder. In a
separate virtual environment install `numpy==2.2.6`, `onnxruntime==1.23.2` and
`tokenizers==0.22.1`. Build the Rust evaluator from `src-tauri`:

```sh
cargo build --locked -p verified-code-memory --example evaluate_memory
```

From the repository root, using that Python environment:

```sh
python scripts/eval/memory-minilm.test.py
python scripts/eval/memory-semantic-experiments.test.py
python scripts/eval/memory-semantic-experiments.py \
  --backend onnx \
  --evaluator "$PWD/src-tauri/target/debug/examples/evaluate_memory" \
  --model-archive /absolute/path/to/onnx.tar.gz \
  --model-path /absolute/path/to/onnx \
  --output /absolute/path/to/results
```

The Rust real-model regression is ignored by default because it needs an
operator-provisioned interpreter and model. It was explicitly executed with
`ROVIK_MEMORY_EVAL_MODEL` and `ROVIK_MEMORY_EVAL_PYTHON` set, and verified repeated
normalized embeddings plus paraphrase recall through `SemanticIndex`:

```sh
cargo test --locked -p verified-code-memory local_minilm_worker -- --ignored
```

The memory crate tests and doctest pass, as do three Python encoder contracts
(padding exclusion, Unicode/input bounds, invalid output rejection), four Python
acceptance-guard tests and Clippy with warnings denied for memory tests/examples.
All 1,044 frontend tests, type checking and repository formatting checks also
pass. The six Rust evaluator regression tests pass separately.

A complete replay of the 120 original cases through the actual persistent
Python worker also passes. Answer delivery, candidate presence, result counts,
scope checks and packet sizes match the recorded-vector trials in every case.
This replay excludes the additional hard-negative controls; those are included
in the calibration and holdout results above.
