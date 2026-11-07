"""Run predeclared recall hypotheses through the actual Rust SQLite retriever.

Requires the protocol's pinned spaCy runtime/model, NumPy and scikit-learn.
Install them in a separate virtual environment; production dependencies do not
change. Build evaluate_memory first and pass its absolute path with --evaluator.
"""

import argparse
import hashlib
import importlib.metadata
import json
import math
from pathlib import Path
import subprocess
import time


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def normalized(vector):
    import numpy as np

    norm = np.linalg.norm(vector)
    return vector / norm if norm else vector


def admissible(report, split, protocol, reference):
    metrics = report["groups"][split]
    ceiling = max(protocol["maximum_no_match_injection_rate"], reference["groups"][split]["no_match_injection_rate"])
    return (
        metrics["no_match_injection_rate"]
        <= ceiling
        and all(not row["prohibited"] for row in report["cases"])
    )


def improves(report, incumbent, split, protocol):
    return admissible(report, split, protocol, incumbent) and (
        report["groups"][split]["recall_at_8"]
        > incumbent["groups"][split]["recall_at_8"] + 1e-12
    )


def non_improvements(trials):
    """Failures are distinct hypotheses and do not reset after an improvement."""
    return len({trial["id"] for trial in trials if trial["improved"] is False})


def run(args):
    import numpy as np
    import spacy
    from sklearn.feature_extraction.text import TfidfVectorizer

    root = Path(__file__).resolve().parents[2]
    protocol_path = root / "scripts/eval/memory-experiment-protocol.json"
    protocol = json.loads(protocol_path.read_text())
    protocol_hash = digest(protocol_path)
    runner_hash = digest(__file__)
    dataset_path = root / "scripts/eval/memory-cases.json"
    if digest(dataset_path) != protocol["dataset_sha256"]:
        raise ValueError("dataset changed after protocol registration")
    if importlib.metadata.version("spacy") != "3.8.11":
        raise ValueError("use the protocol's pinned spaCy runtime")
    if importlib.metadata.version("en-core-web-md") != "3.8.0":
        raise ValueError("use the protocol's pinned vector model")
    args.output.mkdir(parents=True, exist_ok=True)
    controls = root / "scripts/eval/memory-recall-controls.json"
    dataset = json.loads(dataset_path.read_text())["cases"]
    cases = dataset + json.loads(controls.read_text())["cases"]
    corpus = [c for c in dataset if c["category"] == "identifier"]
    documents = [c["title"] + "\n" + c["content"] for c in corpus]
    load_start = time.perf_counter()
    nlp = spacy.load("en_core_web_md", exclude=["tok2vec", "tagger", "parser", "attribute_ruler", "lemmatizer", "ner"])
    model_load_seconds = time.perf_counter() - load_start
    token_cache = {}

    def tokens(text):
        if text not in token_cache:
            token_cache[text] = [
                token for token in nlp.make_doc(text)
                if token.is_alpha and not token.is_stop and token.has_vector
            ]
        return token_cache[text]

    document_frequency = {}
    for text in documents:
        for word in {token.lower_ for token in tokens(text)}:
            document_frequency[word] = document_frequency.get(word, 0) + 1

    def embedding(text, weighted=False):
        words = tokens(text)
        if not words:
            return np.zeros(nlp.vocab.vectors_length)
        weights = [
            math.log((len(corpus) + 1) / (document_frequency.get(t.lower_, 0) + 1)) + 1
            if weighted else 1 for t in words
        ]
        return normalized(np.average([t.vector for t in words], axis=0, weights=weights))

    _, _, components = np.linalg.svd(np.array([embedding(text, True) for text in documents]), full_matrices=False)
    common = components[0]
    characters = TfidfVectorizer(analyzer="char_wb", ngram_range=(3, 5), max_features=512).fit(documents)

    def represent(text, hypothesis):
        if hypothesis == "character_ngrams":
            return normalized(characters.transform([text]).toarray()[0])
        if text in documents and hypothesis in ("title_weighted", "title_only"):
            title, content = text.split("\n", 1)
            text = title if hypothesis == "title_only" else (title + " ") * 4 + content
        vector = embedding(text, hypothesis != "mean_vectors")
        if hypothesis == "remove_common_component":
            vector = normalized(vector - np.dot(vector, common) * common)
        return vector

    def evaluate(label, split, profile=None, threshold=None):
        config = {"split": split, "controls_file": str(controls), "deterministic": True}
        if profile:
            config.update(vectors_file=str(profile), threshold=threshold)
        config_path = args.output / (label + ".config.json")
        config_path.write_text(json.dumps(config))
        completed = subprocess.run(
            [str(args.evaluator), "--experiment-config", str(config_path)],
            check=False, capture_output=True, text=True,
        )
        if completed.returncode:
            raise RuntimeError(f"evaluator failed for {label}: {completed.stderr}")
        report = json.loads(completed.stdout)
        (args.output / (label + ".json")).write_text(json.dumps(report, indent=2) + "\n")
        return report

    def profile(hypothesis, split):
        texts = set(documents)
        texts.update(c["task"] + "\n" for c in cases if c["split"] == split)
        vectors = {text: represent(text, hypothesis).tolist() for text in sorted(texts)}
        path = args.output / (hypothesis + "-" + split + ".vectors.json")
        path.write_text(json.dumps({
            "revision": "en_core_web_md-3.8.0/" + hypothesis,
            "dimensions": len(next(iter(vectors.values()))), "vectors": vectors,
        }))
        return path

    baseline = evaluate("baseline-development", "development")
    incumbent = baseline
    selected = None
    failures = 0
    trials = []
    for hypothesis in protocol["hypotheses"]:
        name = hypothesis["id"]
        vector_path = profile(name, "development")
        candidates = []
        unavailable = None
        for threshold in protocol["thresholds"]:
            label = f"{name}-{threshold:.2f}"
            try:
                report = evaluate(label, "development", vector_path, threshold)
            except RuntimeError as error:
                if 'EmbeddingUnavailable("no vocabulary vector")' not in str(error):
                    raise
                unavailable = str(error)
                break
            if admissible(report, "development", protocol, baseline):
                candidates.append((report["groups"]["development"]["recall_at_8"], threshold, report))
        candidate = max(candidates, key=lambda c: (c[0], c[1])) if candidates else None
        improved = None if unavailable else candidate is not None and improves(candidate[2], incumbent, "development", protocol)
        trial = {**hypothesis, "improved": improved, "threshold": candidate[1] if candidate else None,
                 "metrics": candidate[2]["groups"]["development"] if candidate else None,
                 "status": "unavailable" if unavailable else "evaluated", "error": unavailable}
        trials.append(trial)
        if improved:
            incumbent = candidate[2]
            selected = (name, candidate[1])
        failures = non_improvements(trials)
        print(json.dumps({**trial, "non_improvements": failures}), flush=True)
        if not args.all_hypotheses and failures == protocol["stop_after_distinct_non_improvements"]:
            break
    if not args.all_hypotheses and failures != protocol["stop_after_distinct_non_improvements"]:
        raise RuntimeError("predeclared hypotheses exhausted before the stopping rule")
    # The selected representation and threshold are frozen before scoring these.
    heldout = evaluate("baseline-heldout", "heldout")
    selected_heldout = evaluate("selected-heldout", "heldout", profile(selected[0], "heldout"), selected[1]) if selected else heldout
    summary = {
        "protocol": protocol, "protocol_sha256": protocol_hash,
        "runner_sha256": runner_hash, "controls_sha256": digest(controls),
        "evaluator_sha256": digest(args.evaluator),
        "evaluator_source_sha256": digest(root / "src-tauri/crates/verified-code-memory/examples/evaluate_memory.rs"),
        "base_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "model_vectors_sha256": digest(Path(nlp.path) / "vocab/vectors"),
        "versions": {name: importlib.metadata.version(name) for name in ("spacy", "en-core-web-md", "numpy", "scikit-learn")},
        "model_load_seconds": model_load_seconds,
        "baseline_development": baseline["groups"]["development"],
        "run_mode": "all hypotheses, explicitly requested after implementation" if args.all_hypotheses else "original three-non-improvement stopping rule",
        "selection_no_match_ceiling": max(protocol["maximum_no_match_injection_rate"], baseline["groups"]["development"]["no_match_injection_rate"]),
        "trials": trials, "distinct_non_improvements": failures, "selected": selected,
        "baseline_heldout": heldout["groups"]["heldout"],
        "selected_heldout": selected_heldout["groups"]["heldout"],
        "heldout_improved_under_guardrails": improves(selected_heldout, heldout, "heldout", protocol),
        "agent_answer_quality_evaluated": False, "production_policy_changed": False,
    }
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evaluator", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--all-hypotheses", action="store_true", help="Override the original stopping rule when explicitly requested.")
    run(parser.parse_args())
