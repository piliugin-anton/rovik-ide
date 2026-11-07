"""Compare automatic concept and sentence encoders using production retrieval.

Calibrate on development only. Require preservation of every baseline-delivered
answer and no newly injected no-match case. Only the selected improving variant
is evaluated on heldout. These synthetic cases do not measure agent task success.
Model downloads are an explicit operator step; this runner is entirely offline.
"""
import argparse
from collections import Counter
import hashlib
import importlib.metadata
import importlib.util
import json
import math
from pathlib import Path
import re
import subprocess
import tarfile
import time


ROOT = Path(__file__).resolve().parents[2]


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def changes(before, after):
    """Compare individual cases; aggregate recall cannot hide a lost answer."""
    old = {case["id"]: case for case in before["cases"]}
    new_ids = {case["id"] for case in after["cases"]}
    if len(old) != len(before["cases"]) or len(new_ids) != len(after["cases"]):
        raise ValueError("duplicate case IDs")
    if set(old) != new_ids:
        raise ValueError("case sets differ")
    result = {"gains": [], "losses": [], "new_injections": []}
    for case in after["cases"]:
        previous = old[case["id"]]
        if previous["answerable"] != case["answerable"]:
            raise ValueError("case labels changed")
        if case["prohibited"] or case["packet_tokens"] > 1000:
            raise ValueError("scope or packet allowance violation")
        if case["answerable"]:
            if case["answer_delivered"] and not previous["answer_delivered"]:
                result["gains"].append(case["id"])
            if previous["answer_delivered"] and not case["answer_delivered"]:
                result["losses"].append(case["id"])
        elif case["returned"] and not previous["returned"]:
            result["new_injections"].append(case["id"])
    return result


def concept_encoder(documents, all_senses, nltk_data):
    import nltk
    import numpy as np
    from nltk.corpus import wordnet
    from nltk.stem import PorterStemmer

    nltk.data.path.insert(0, str(nltk_data))
    source = (ROOT / "src-tauri/crates/verified-code-memory/src/retrieval.rs").read_text()
    stop = set(re.findall(r'"([a-z]+)"', source.split("const GENERIC_TERMS:")[1].split("];")[0]))
    stemmer = PorterStemmer()

    def features(text):
        result = Counter()
        for token in re.findall(r"[A-Za-z][A-Za-z0-9_-]*", text.lower()):
            if token in stop or len(token) < 3:
                continue
            result["word:" + stemmer.stem(token)] += 1.0
            senses = wordnet.synsets(token)
            if not all_senses:
                senses = senses[:1]
            if senses:
                weight = 0.75 / math.sqrt(len(senses))
                for sense in senses:
                    result["concept:" + sense.name()] += weight
        return result

    frequencies = Counter()
    for text in documents:
        frequencies.update(features(text).keys())
    vocabulary = {feature: at for at, feature in enumerate(sorted(frequencies))}
    idf = {feature: math.log((len(documents) + 1) / (count + 1)) + 1
           for feature, count in frequencies.items()}

    def encode(text):
        vector = np.zeros(len(vocabulary), dtype=np.float32)
        for feature, value in features(text).items():
            if feature in vocabulary:
                vector[vocabulary[feature]] = value * idf[feature]
        norm = np.linalg.norm(vector)
        if norm:
            vector /= norm
        return vector.tolist()

    return encode, len(vocabulary), {
        "wordnet": wordnet.get_version(), "features": "stem + IDF-weighted synset identity",
        "archive_sha256": sha(nltk_data / "corpora/wordnet.zip"),
        "documents_sha256": hashlib.sha256(json.dumps(sorted(documents)).encode()).hexdigest(),
        "stop_terms_sha256": hashlib.sha256(json.dumps(sorted(stop)).encode()).hexdigest(),
    }


def sentence_encoder(model_path, claim_only):
    worker_path = ROOT / "src-tauri/crates/verified-code-memory/src/minilm.py"
    spec = importlib.util.spec_from_file_location("memory_minilm", worker_path)
    worker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(worker)
    encoder = worker.MiniLM(model_path)

    def encode(text):
        if claim_only and "\n" in text and text.split("\n", 1)[1]:
            text = text.split("\n", 1)[1]
        return encoder.encode(text)

    return encode, 384, {"model_sha256": sha(model_path / "model.onnx"), "tokenizer_sha256": sha(model_path / "tokenizer.json"), "pooling": "attention-weighted mean, L2 normalized", "worker_sha256": sha(worker_path)}


def main(args):
    protocol_path = ROOT / "scripts/eval" / ("memory-concept-protocol.json" if args.backend == "wordnet" else "memory-semantic-protocol.json")
    protocol = json.loads(protocol_path.read_text())
    fresh_path = ROOT / "scripts/eval/memory-semantic-fresh-cases.json"
    if sha(fresh_path) != protocol.get("fresh_fixture_sha256", protocol.get("fresh_sha256")):
        raise ValueError("fresh cases changed after registration")
    packages = {"numpy": "2.2.6"}
    packages.update({"nltk": "3.9.2"} if args.backend == "wordnet" else {"onnxruntime": "1.23.2", "tokenizers": "0.22.1"})
    for package, expected in packages.items():
        if importlib.metadata.version(package) != expected:
            raise ValueError(f"expected {package}=={expected}")
    if args.backend == "onnx":
        if not args.model_path or not args.model_archive:
            raise ValueError("ONNX requires --model-path and --model-archive")
        if sha(args.model_archive) != protocol["model_archive_sha256"]:
            raise ValueError("model archive differs from the published checksum")
        # Bind the extracted files to the checked archive too: a nearby valid
        # archive is not evidence about a different model directory.
        with tarfile.open(args.model_archive) as archive:
            for name in ["model.onnx", "tokenizer.json"]:
                member = archive.extractfile("onnx/" + name)
                if member is None or hashlib.sha256(member.read()).hexdigest() != sha(args.model_path / name):
                    raise ValueError(f"extracted {name} differs from the checked archive")
    else:
        if not args.nltk_data:
            raise ValueError("WordNet requires --nltk-data")
        if sha(args.nltk_data / "corpora/wordnet.zip") != protocol["corpus_sha256"]:
            raise ValueError("WordNet corpus changed after registration")
    args.output.mkdir(parents=True, exist_ok=True)
    datasets = {
        "original": json.loads((ROOT / "scripts/eval/memory-cases.json").read_text()),
        "fresh": json.loads(fresh_path.read_text()),
    }
    for name, dataset in datasets.items():
        seen, incidents = set(), {}
        for case in dataset["cases"]:
            if case["id"] in seen:
                raise ValueError(f"duplicate ID in {name}")
            seen.add(case["id"])
            split = incidents.setdefault(case["incident"], case["split"])
            if split != case["split"] or split not in {"development", "heldout"}:
                raise ValueError(f"incident split leakage in {name}")
    controls_path = ROOT / "scripts/eval/memory-recall-controls.json"
    controls = json.loads(controls_path.read_text())["cases"]
    documents = {case["title"] + "\n" + case["content"]
                 for dataset in datasets.values() for case in dataset["cases"]
                 if case["category"] == "identifier"}

    def evaluate(name, corpus, split, options=None):
        config = {"split": split, "deterministic": True, "memory_budget": 1000}
        config.update({"dataset_file": str(fresh_path)} if corpus == "fresh"
                      else {"controls_file": str(controls_path)})
        config.update(options or {})
        path = args.output / f"{name}-{corpus}-{split}.config.json"
        path.write_text(json.dumps(config))
        completed = subprocess.run([str(args.evaluator), "--experiment-config", str(path)], capture_output=True, text=True, check=True)
        report = json.loads(completed.stdout)
        if any(case["prohibited"] or case["packet_tokens"] > 1000 for case in report["cases"]):
            raise ValueError("scope or packet allowance violation")
        (args.output / f"{name}-{corpus}-{split}.json").write_text(json.dumps(report, indent=2) + "\n")
        return report

    def vectors(encode, dimensions, revision, split):
        texts = documents | {case["task"] + "\n" for dataset in datasets.values()
                             for case in dataset["cases"] if case["split"] == split}
        texts |= {case["task"] + "\n" for case in controls if case["split"] == split}
        started = time.perf_counter()
        encoded = {text: encode(text) for text in sorted(texts)}
        path = args.output / f"{revision}-{split}.vectors.json"
        path.write_text(json.dumps({"revision": revision, "dimensions": dimensions, "vectors": encoded}))
        return path, {"texts": len(texts), "seconds": time.perf_counter() - started}

    baseline = {corpus: evaluate("baseline", corpus, "development") for corpus in datasets}
    representations = ["first-sense", "all-senses"] if args.backend == "wordnet" else ["title-claim", "claim-only"]
    thresholds = protocol["thresholds"]
    trials, models, best = [], {}, None
    for representation in representations:
        encode, dimensions, identity = (concept_encoder(documents, representation == "all-senses", args.nltk_data)
                                        if args.backend == "wordnet" else sentence_encoder(args.model_path, representation == "claim-only"))
        identity_hash = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()[:12]
        revision = args.backend + "-" + representation + "-" + identity_hash
        path, encoding_time = vectors(encode, dimensions, revision, "development")
        models[representation] = (encode, dimensions, identity, encoding_time, revision)
        for threshold in thresholds:
            reports = {corpus: evaluate(f"{revision}-{threshold:.2f}", corpus, "development",
                                       {"vectors_file": str(path), "threshold": threshold}) for corpus in datasets}
            per_case = {corpus: changes(baseline[corpus], reports[corpus]) for corpus in datasets}
            gains = sum(len(item["gains"]) for item in per_case.values())
            safe = all(not item["losses"] and not item["new_injections"] for item in per_case.values())
            trials.append({"representation": representation, "threshold": threshold, "changes": per_case,
                           "metrics": {corpus: report["groups"]["development"] for corpus, report in reports.items()}})
            candidate = (gains, threshold, representation)
            if safe and gains and (best is None or candidate > best):
                best = candidate
        print(revision, "finished development", flush=True)

    result = {"backend": args.backend, "protocol_sha256": sha(protocol_path), "fresh_sha256": sha(fresh_path),
              "runner_sha256": sha(__file__), "baseline": {key: report["groups"] for key, report in baseline.items()},
              "evaluator_sha256": sha(args.evaluator),
              "dataset_sha256": sha(ROOT / "scripts/eval/memory-cases.json"),
              "controls_sha256": sha(controls_path),
              "packages": packages,
              "models": {key: {"identity": model[2], "dimensions": model[1], "encoding": model[3]} for key, model in models.items()},
              "trials": trials, "selected": best, "heldout": None, "answer_quality_evaluated": False}
    if best:
        _, threshold, representation = best
        encode, dimensions, _, _, revision = models[representation]
        path, _ = vectors(encode, dimensions, revision, "heldout")
        result["heldout"] = {}
        for corpus in datasets:
            control = evaluate("baseline", corpus, "heldout")
            selected = evaluate("selected", corpus, "heldout", {"vectors_file": str(path), "threshold": threshold})
            result["heldout"][corpus] = {"baseline": control, "selected": selected, "changes": changes(control, selected)}
    (args.output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print("Selected:", best, flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=["wordnet", "onnx"], required=True)
    parser.add_argument("--evaluator", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--nltk-data", type=Path)
    parser.add_argument("--model-path", type=Path)
    parser.add_argument("--model-archive", type=Path)
    main(parser.parse_args())
