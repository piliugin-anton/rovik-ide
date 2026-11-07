# Memory recall experiments, November 7, 2026

Base: master `7e140c9`. The experiment follows
[the implementation review](long-term-memory-review-nov7-2026.md).
The measured outcome is retrieval, not agent answer quality. Production retrieval
defaults are unchanged.

## First round: stop after three distinct non-improvements

The original 120-case fixture reproduces development Recall@8 of 82.86% (58/70
answerable cases) and held-out recall of 80% (24/30). Splits contain 14 and six
independent incidents respectively; several questions describe each incident.

The frozen [protocol](../../scripts/eval/memory-experiment-protocol.json) adds
12 [hard no-match controls](../../scripts/eval/memory-recall-controls.json),
eight development and four held out. Unlike repeated spectroscopy negatives,
these are coding tasks that share broad vocabulary with the corpus.

| Hypothesis                                            | Selected threshold | Development Recall@8 | Outcome against incumbent             |
| ----------------------------------------------------- | ------------------ | -------------------- | ------------------------------------- |
| Existing lexical/file retrieval                       | —                  | 82.86%               | Control                               |
| Pretrained word-vector means + existing hybrid fusion | 0.80               | 84.29%               | One additional case: `cache-implicit` |
| Corpus IDF weighting                                  | 0.80               | 84.29%               | No additional recall; failure 1       |
| Remove leading corpus component                       | 0.60               | 84.29%               | No additional recall; failure 2       |
| Character n-gram vectors                              | 0.90               | 82.86%               | No additional recall; failure 3, stop |

Only development outcomes selected thresholds. Each hypothesis used the same
predeclared 17-threshold grid, SQLite corpus, final limit of eight and scope
rules. Selection requires greater recall and no false-positive regression.
The two remaining title hypotheses were not run after the stopping condition.

The selected mean-vector candidate has **80% held-out recall, unchanged from
baseline**, with no changed held-out result ranks. The development improvement
is one question from one incident; this does not establish generalization.
All retained trials had zero cross-project results, including identical records
whose vectors were explicitly indexed in another project.

Hard controls expose a baseline problem: no-match injection is 2/22 (9.09%) in
development and 3/10 (30%) held out, unchanged by the selected candidate. The
original spectroscopy negatives still abstain. Report these alongside the
published zero on the original negatives. The production target of at most 5%
injection is **not met**.

An initial absolute-5% calibration rule made every additive hybrid ineligible
because lexical retrieval already exceeded it. That pilot was discarded. The
corrected research rule permits no increase above `max(5%, baseline injection)`;
it does not relax the production release gate. The correction and exploratory
replays are recorded in the protocol. No held-out outcomes informed threshold
selection. This held-out baseline was already published and has been reused;
it is not a newly blinded benchmark.

## Reproduction and limits

The [machine-readable results](long-term-memory-experiments-nov7-2026.json)
contain every retained threshold, paired case outcomes, model/vector fingerprints,
tool versions and the stopping decision. A second replay verifies identical
rank, admission and recall results after fixing fixture UUIDs and timestamps.
The harness uses the production `SemanticIndex`, FTS retrieval and rank fusion;
it does not substitute a Python reimplementation of those policies.

The real learned model is spaCy `en_core_web_md` 3.8.0, loaded by spaCy 3.8.11.
It provides 300-dimensional word vectors. Mean pooling, IDF, component removal
and character features are explicit experimental representations. This is not
a test of transformer sentence embeddings or the installed production Python
worker. Unknown word-vector queries explicitly fall back to lexical retrieval;
missing replay inputs fail. The model was acquired from its official spaCy
GitHub release. No credentials or project histories are part of the artifacts.

Build `evaluate_memory`, provision the isolated research environment, then run:

```sh
cd src-tauri
cargo build --locked -p verified-code-memory --example evaluate_memory
cd ..
python3 -m venv /tmp/rovik-memory-eval
/tmp/rovik-memory-eval/bin/pip install spacy==3.8.11 numpy==2.3.5 scikit-learn==1.8.0
/tmp/rovik-memory-eval/bin/pip install https://github.com/explosion/spacy-models/releases/download/en_core_web_md-3.8.0/en_core_web_md-3.8.0-py3-none-any.whl
/tmp/rovik-memory-eval/bin/python scripts/eval/memory-experiments.py \
  --evaluator "$PWD/src-tauri/target/debug/examples/evaluate_memory" \
  --output /tmp/rovik-memory-results
```

Adjust the executable path when `CARGO_TARGET_DIR` is configured. No model is
downloaded by the runner. Dataset fingerprints reject accidental fixture changes.

Model loading took approximately 0.6 seconds on this Linux executor. Per-case
timings include rebuilding derived indexes and replaying precomputed vectors;
they are not warm online embedding latency. No <200 ms production hybrid claim
or real-agent quality improvement follows from these measurements. The corpus
has only 20 incidents, gold records have synthetic verification, and distractors
are provisional, so measured ranking is not a production workload estimate.

Validation: 74 memory-crate nextest tests, three evaluator tests, one doctest,
two Python protocol tests, frontend tests and type checking passed. Clippy and
the repository format check are repeated before committing the final harness.
