"""Offline protocol checks; no model or network dependencies."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("experiments", Path(__file__).with_name("memory-experiments.py"))
experiments = importlib.util.module_from_spec(spec)
spec.loader.exec_module(experiments)


class ExperimentProtocolTests(unittest.TestCase):
    def test_success_does_not_reset_distinct_failed_hypotheses(self):
        trials = [
            {"id": "one", "improved": False},
            {"id": "winner", "improved": True},
            {"id": "two", "improved": False},
            {"id": "one", "improved": False},
        ]
        self.assertEqual(experiments.non_improvements(trials), 2)
        trials.append({"id": "three", "improved": False})
        self.assertEqual(experiments.non_improvements(trials), 3)
        trials.append({"id": "unavailable-model", "improved": None})
        self.assertEqual(experiments.non_improvements(trials), 3)

    def test_recall_cannot_be_improved_by_returning_everything(self):
        protocol = {"maximum_no_match_injection_rate": 0.05}
        def report(recall, false_positive=0, prohibited=False):
            return {"groups": {"development": {"recall_at_8": recall, "no_match_injection_rate": false_positive}}, "cases": [{"prohibited": prohibited}]}
        baseline = report(0.8)
        self.assertTrue(experiments.improves(report(0.9), baseline, "development", protocol))
        self.assertFalse(experiments.improves(report(1, 1), baseline, "development", protocol))
        self.assertFalse(experiments.improves(report(1, prohibited=True), baseline, "development", protocol))
        self.assertFalse(experiments.improves(report(0.8), baseline, "development", protocol))
        # A baseline already over the proposed release target must not make
        # every additive hybrid inadmissible; research still forbids regression.
        weak_baseline = report(0.8, 0.1)
        self.assertTrue(experiments.improves(report(0.9, 0.1), weak_baseline, "development", protocol))
        self.assertFalse(experiments.improves(report(0.9, 0.2), weak_baseline, "development", protocol))


if __name__ == "__main__":
    unittest.main()
