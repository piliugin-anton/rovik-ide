# Semantic misses after the capacity fixes

The production capacity gains were validated and pushed individually as
`b7bfd26` (packing, 3/7 → 7/7 supplied requirements) and `e57fe74` (candidate
pools, 9/21 → 21/21). Those are constructed mechanism results; the original
semantic Recall@8 remains 58/70 on development and 24/30 on the exposed holdout.

## Automatic WordNet concepts: no gain

A different approach maps words to WordNet synset identities, without manually
written contextual keys or changing any source claim. Stem features and synset
features are IDF weighted using only the available source documents. Two
representations use either the first sense or all senses; neither uses expected
answers to author descriptors.

Both were replayed through the real SQLite semantic index and hybrid retriever
at eleven frozen thresholds, over the original development cases and the fresh
development set. Across **22 configurations**, neither representation supplied
an additional answer. The lower thresholds introduced new unrelated memories;
the safe thresholds reproduced the lexical control. No production concept
expansion is enabled. Because development selected no improving variant, the
fresh holdout was not scored.

The [machine-readable result](long-term-memory-semantic-concepts-nov7-2026.json)
records every development trial, individual losses and new false injections,
model/corpus fingerprints and the runner fingerprint. Its acceptance check
rejects each individual lost answer or newly injected no-match case: a larger
aggregate recall cannot compensate for either.

## Fresh cases and the next representation

[Ninety new cases](../../scripts/eval/memory-semantic-fresh-cases.json) were
authored and hashed before the model trials: twenty independent incidents, each
with an identifier, implicit question and explicit-file control, plus thirty
no-match questions including ten adjacent-topic hard negatives. Development
and holdout incidents are disjoint. They remain synthetic authored fixtures,
not independent human judgments or evidence of agent task success.

The fresh development control supplies 24/30 answers and injects a memory in
10/15 no-match cases. This again exposes weak lexical abstention. The fresh
holdout is reserved for a representation that improves development under the
per-case regression constraints.

The next registered representation is the actual MiniLM sentence transformer,
using Chroma's published ONNX export and checksum. The offline runner supports
title-plus-claim and claim-only representations, attention-weighted mean pooling
and normalized 384-dimensional vectors. It verifies both the archive checksum
and that the extracted model/tokenizer match that archive. This is prepared
evaluation code, **not an executed MiniLM result or a production backend**.

Download attempts failed with proxy `403`, including a fresh connection outside
the nested command sandbox using the same proxy. Although the user added
`chroma-onnx-models.s3.amazonaws.com` in settings, the attached environment still
reported revision 1 with no custom allowed hosts. The live proxy also confirmed
that the hostname was absent from its active rules. The allowlist change must
reach the running instance before this experiment can execute.

## Reproduction

Use a separate Python environment with `numpy==2.2.6` and `nltk==3.9.2`.
Download the NLTK WordNet archive from its official `nltk_data` GitHub repository
to `<nltk-data>/corpora/wordnet.zip`; the registered hash is checked by the runner.

```sh
cd src-tauri
cargo build --locked -p verified-code-memory --example evaluate_memory
cd ..
python scripts/eval/memory-semantic-experiments.test.py
python scripts/eval/memory-semantic-experiments.py \
  --backend wordnet \
  --evaluator "$PWD/src-tauri/target/debug/examples/evaluate_memory" \
  --nltk-data /path/to/nltk-data \
  --output /tmp/semantic-concepts
```

For the pending MiniLM run, install `onnxruntime==1.23.2` and
`tokenizers==0.22.1`, download the registered archive, and use `--backend onnx`
with `--model-archive` and `--model-path` pointing to its extracted `onnx` folder.
The runner performs no model downloads. It chooses on development only and
scores only the selected improving representation on holdout.

Before the research commit, all 91 memory tests and the memory doctest passed,
Clippy passed with warnings denied, and four Python acceptance-guard tests passed.
All 1,044 frontend tests, type checking and format checking passed again. The
default evaluator still reproduces all 132 original per-case outcomes exactly.
The WordNet replay used the final evaluator binary and records its hash. These
checks do not stand in for executing the unavailable ONNX model.
