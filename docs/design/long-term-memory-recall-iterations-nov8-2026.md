# Recall iterations after native ONNX

Native support and the GitHub downloader were pushed as `f9a7ee9` before this
campaign. The dedicated model release was published once, and the Rust network
integration test downloaded its two files, checked their sizes and SHA-256
hashes, and ran inference successfully. The latest application release stayed
`v0.2.6`; subsequent code pushes do not republish the model.

Four code changes were evaluated in sequence. A failed approach was removed
before trying the next. The original claims and task splits stayed unchanged.
Selection used development only. Each candidate had to preserve every delivered
answer and its rank, avoid increasing returned records in any no-answer case,
stay inside the 1,000-token allowance, and preserve project isolation.

| Approach                                                           | Development trials | Decision                                                                       |
| ------------------------------------------------------------------ | ------------------ | ------------------------------------------------------------------------------ |
| Lower admission when the closest candidate has a clear cosine lead | 18                 | One potential extra answer came with extra false matches; rejected.            |
| Encode the claim before its title                                  | 11                 | Gains at lower cutoffs came with ranking or false-match regressions; rejected. |
| Average both document orders and prioritize explicit context       | 11                 | No improving configuration passed all guards; rejected.                        |
| Calibrate short, distinctive queries separately                    | 16                 | One additional development answer, with all guards passing.                    |

The selected short-query policy uses at most **two whitespace-separated words**
and multiplies the normal cutoff by **11/12**. At the default normal cutoff of
0.30 this gives 0.275. Requests containing only generic terms keep the strict
cutoff, as do longer requests. The failure text counts toward query length.
Strict `SemanticIndex::search` calls retain their explicit cutoff. Other
embedding backends retain their existing behavior unless their caller opts into
the calibration builder; the core enables the calibrated policy for MiniLM.
Projects can set `memorySemantic.shortQueryScale: 1` to retain a strict cutoff.

| Corpus / split       | Before         | After          |
| -------------------- | -------------- | -------------- |
| Original development | 64/70 (91.43%) | 65/70 (92.86%) |
| Fresh development    | 27/30 (90%)    | 27/30 (90%)    |
| Original validation  | 26/30 (86.67%) | 26/30 (86.67%) |
| Fresh validation     | 28/30 (93.33%) | 28/30 (93.33%) |

The additional answer is the historical migration procedure for the request
“database upgrade.” No existing delivered answer lost its position, and no
no-answer case gained extra returned records. Thirty-six additional short
negative controls were frozen before this approach's trials, including adjacent
coding topics and unrelated domains. The 258-case replay includes those controls.
Existing false matches remain; this is a small development gain with unchanged
validation recall, not evidence of improved live-agent task success. Original
answerable validation cases had already been exposed before these experiments.

The [machine-readable artifact](long-term-memory-recall-iterations-nov8-2026.json)
records all trials, selected validation outcomes, registration inputs and model
bindings. The registered control JSON is included verbatim so its original byte
hash can be checked even when the committed fixture is formatted differently.
The failed prototype APIs are not part of the shipped implementation.

## Native inference and evaluation

The first native implementation padded every request to 256 tokens. The model
now uses a symbolic sequence length and processes only the tokens present, up
to the same 256-token bound. The real-model reference regression still passes.
A separate real-model test covers empty input, maximum-length Unicode input and
reuse of the same compiled model across those shapes.

The evaluator caches each successfully encoded input for its process lifetime
and can export vectors using `save_vectors_file` in its experiment config.
Provider failures are not cached. This lets native experiments replay the same
real vectors across cutoff trials without Python or repeated inference for
every fixture store. The application's retrieval cache behavior is unchanged.
For 91 matched fixture cases, warm evaluator p95 fell from about 933 ms to
23 ms; this is not a corpus-scale latency guarantee. All 222 previously
measured baseline case outcomes matched after changing the input shape. The
final native replay reproduced the selected outcomes for all 258 cases.

To reproduce the selected policy, build the Rust `evaluate_memory` example and
provide the pinned local model through `ROVIK_MEMORY_EVAL_MODEL`, with dimensions
384, the pinned revision and normal threshold 0.3. Supply this configuration
through `--experiment-config` (use `split: "heldout"` for validation):

```json
{
  "split": "development",
  "deterministic": true,
  "memory_budget": 1000,
  "controls_file": "scripts/eval/memory-short-query-controls.json",
  "short_query_words": 2,
  "short_query_scale": 0.9166666666666666
}
```

Use `dataset_file: "scripts/eval/memory-semantic-fresh-cases.json"` and omit
`controls_file` for the fresh corpus. For the strict baseline, set
`short_query_scale` to 1. Semantic retrieval remains an explicit project opt-in;
this change does not enable it globally or modify stored claims and evidence.

## Regression checks

Tests ran one crate at a time. Core passed 2,578 unit tests and 20 integration
tests; its 28 gated tests and one doctest remained ignored. The memory crate
passed 88 unit/integration tests, its doctest, seven evaluator tests and both
explicitly enabled real-model tests. Clippy passed with no new warnings
(four existing core warnings). All 1,044 frontend tests, type checking and
formatting checks passed.
