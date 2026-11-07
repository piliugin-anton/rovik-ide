"""Run the untested parts of the user's contextual-memory proposal.

Uses the already pinned research environment. Produces retrieval ablations and
separate, explicitly constructed capture/selection mechanism results.
"""
import argparse
import hashlib
import importlib.util
import importlib.metadata
import json
from pathlib import Path
import subprocess
import time


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main(args):
    import numpy as np
    import spacy

    root = Path(__file__).resolve().parents[2]
    protocol_path = root / "scripts/eval/memory-followup-protocol.json"
    protocol = json.loads(protocol_path.read_text())
    protocol_hash = sha(protocol_path)
    runner_hash = sha(__file__)
    for package, expected in [("spacy", "3.8.11"), ("en-core-web-md", "3.8.0")]:
        if importlib.metadata.version(package) != expected:
            raise ValueError(f"expected {package}=={expected}")
    keys_path = root / "scripts/eval/memory-context-keys.json"
    dataset_path = root / "scripts/eval/memory-cases.json"
    assert sha(keys_path) == protocol["keys_sha256"], "context keys changed after registration"
    assert sha(dataset_path) == protocol["dataset_sha256"], "dataset changed after registration"
    spec = importlib.util.spec_from_file_location("first_round", Path(__file__).with_name("memory-experiments.py"))
    first = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(first)
    args.output.mkdir(parents=True, exist_ok=True)
    keys = json.loads(keys_path.read_text())["entries"]
    dataset = json.loads(dataset_path.read_text())["cases"]
    controls_path = root / "scripts/eval/memory-recall-controls.json"
    cases = dataset + json.loads(controls_path.read_text())["cases"]
    documents = {case["title"] + "\n" + case["content"]: case["title"] for case in dataset if case["category"] == "identifier"}
    load_start = time.perf_counter()
    nlp = spacy.load("en_core_web_md", exclude=["tok2vec", "tagger", "parser", "attribute_ruler", "lemmatizer", "ner"])
    load_seconds = time.perf_counter() - load_start

    def encode(text):
        tokens = [token.vector for token in nlp.make_doc(text) if token.is_alpha and not token.is_stop and token.has_vector]
        return first.normalized(np.mean(tokens, axis=0)) if tokens else np.zeros(300)

    query_times = []
    for _ in range(5):
        for text in sorted({case["task"] for case in cases}):
            started = time.perf_counter()
            encode(text)
            query_times.append((time.perf_counter() - started) * 1000)

    def vectors(split, contextual):
        texts = sorted(set(documents) | {case["task"] + "\n" for case in cases if case["split"] == split})
        result = {}
        for original in texts:
            text = original
            if contextual and original in documents:
                text += "\n" + " ".join(keys[documents[original]]["terms"])
            vector = encode(text)
            result[original] = vector.tolist()
        path = args.output / f"{'contextual' if contextual else 'plain'}-{split}.vectors.json"
        path.write_text(json.dumps({"revision": "en_core_web_md-3.8.0/" + (protocol["keys_sha256"] if contextual else "mean"), "dimensions": 300, "vectors": result}))
        return path

    def evaluate(name, split, options=None):
        config = {"split": split, "deterministic": True, "memory_budget": 1000, "controls_file": str(controls_path), **(options or {})}
        path = args.output / (name + "-" + split + ".config.json")
        path.write_text(json.dumps(config))
        completed = subprocess.run([str(args.evaluator), "--experiment-config", str(path)], capture_output=True, text=True)
        if completed.returncode:
            raise RuntimeError(completed.stderr)
        report = json.loads(completed.stdout)
        (args.output / (name + "-" + split + ".json")).write_text(json.dumps(report, indent=2) + "\n")
        return report

    # Fit/select using development only; all representation inputs are frozen.
    development = {"baseline": evaluate("baseline", "development")}
    thresholds = {}
    options = {"baseline": {}, "intent_only": {"query_mode": "intent_only"}, "context_keys": {"context_keys_file": str(keys_path)}, "candidate_32": {"candidate_limit": 32}}
    for name in ["intent_only", "context_keys", "candidate_32"]:
        development[name] = evaluate(name, "development", options[name])
    tuning = []
    for name, contextual in [("semantic_only", False), ("contextual_hybrid", True), ("contextual_gate", True)]:
        path = vectors("development", contextual)
        best = None
        for threshold in protocol["thresholds"]:
            config = {"vectors_file": str(path), "threshold": threshold, "semantic_only": not contextual}
            if name == "contextual_gate":
                config.update(rerank_lexical=True, candidate_limit=32)
            if contextual:
                config["context_keys_file"] = str(keys_path)
            report = evaluate(f"{name}-{threshold:.2f}", "development", config)
            tuning.append({"variant": name, "threshold": threshold, "metrics": report["groups"]["development"]})
            if first.admissible(report, "development", {"maximum_no_match_injection_rate": 0.05}, development["baseline"]):
                candidate = (report["groups"]["development"]["recall_at_8"], threshold, report)
                if best is None or candidate[:2] > best[:2]:
                    best = candidate
        if best:
            thresholds[name] = best[1]
            development[name] = best[2]
            options[name] = {"semantic_only": not contextual, "threshold": best[1]}
            if name == "contextual_gate":
                options[name].update(rerank_lexical=True, candidate_limit=32)
            if contextual:
                options[name]["context_keys_file"] = str(keys_path)
    validation = {}
    for name, config in options.items():
        config = dict(config)
        if name in thresholds:
            config["vectors_file"] = str(vectors("heldout", name != "semantic_only"))
        validation[name] = evaluate(name, "heldout", config)
    structure = json.loads(subprocess.check_output([str(args.structure_evaluator)], text=True))

    def compact(report):
        return {"groups": report["groups"], "model": report["model"], "cases": report["cases"]}

    summary = {
        "protocol": protocol, "protocol_sha256": protocol_hash, "runner_sha256": runner_hash,
        "evaluator_sha256": sha(args.evaluator), "structure_evaluator_sha256": sha(args.structure_evaluator),
        "development": {name: compact(report) for name, report in development.items()},
        "validation": {name: compact(report) for name, report in validation.items()},
        "selected_thresholds": thresholds, "threshold_trials": tuning, "structure": structure,
        "unavailable_variants": [name for name in ["semantic_only", "contextual_hybrid", "contextual_gate"] if name not in thresholds],
        "versions": {name: importlib.metadata.version(name) for name in ("spacy", "en-core-web-md", "numpy", "scikit-learn")},
        "model_vectors_sha256": sha(Path(nlp.path)/"vocab/vectors"),
        "encoder_cost": {"model_load_seconds": load_seconds, "warm_query_p50_ms": float(np.percentile(query_times, 50)), "warm_query_p95_ms": float(np.percentile(query_times, 95)), "samples": len(query_times), "excludes": "worker IPC, SQLite retrieval/indexing, model reply and 10000-record scale; these are in-process encoder measurements"},
        "learned_cross_encoder_tested": False, "automatic_extractor_quality_tested": False,
        "real_agent_task_quality_tested": False, "production_defaults_changed": False,
    }
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({name: {"development": report["groups"], "validation": validation[name]["groups"]} for name, report in development.items()}, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evaluator", type=Path, required=True)
    parser.add_argument("--structure-evaluator", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    main(parser.parse_args())
