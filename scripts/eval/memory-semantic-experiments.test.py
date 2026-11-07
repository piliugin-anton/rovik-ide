"""The semantic experiment's per-case acceptance checks cannot hide regressions."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("experiment", Path(__file__).with_name("memory-semantic-experiments.py"))
experiment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(experiment)


def case(name, delivered=False, returned=0, answerable=True, **kwargs):
    return {"id": name, "answer_delivered": delivered, "returned": returned,
            "answerable": answerable, "prohibited": False, "packet_tokens": 100, **kwargs}


class Acceptance(unittest.TestCase):
    def test_an_aggregate_gain_does_not_hide_an_existing_answer_loss(self):
        before = {"cases": [case("known", True), case("new-a"), case("new-b")]}
        after = {"cases": [case("known"), case("new-a", True), case("new-b", True)]}
        changes = experiment.changes(before, after)
        self.assertEqual(changes["gains"], ["new-a", "new-b"])
        self.assertEqual(changes["losses"], ["known"])

    def test_new_false_injections_are_checked_per_case(self):
        before = {"cases": [case("a", returned=1, answerable=False), case("b", answerable=False)]}
        after = {"cases": [case("a", answerable=False), case("b", returned=1, answerable=False)]}
        self.assertEqual(experiment.changes(before, after)["new_injections"], ["b"])

    def test_scope_budget_and_missing_cases_are_rejected(self):
        before = {"cases": [case("a")]}
        for after in [{"cases": [case("a", prohibited=True)]},
                      {"cases": [case("a", packet_tokens=1001)]}, {"cases": []}]:
            with self.subTest(after=after), self.assertRaises(ValueError):
                experiment.changes(before, after)

    def test_duplicate_ids_and_changed_answer_labels_are_rejected(self):
        before = {"cases": [case("a")]}
        for after in [{"cases": [case("a"), case("a")]},
                      {"cases": [case("a", answerable=False)]}]:
            with self.subTest(after=after), self.assertRaises(ValueError):
                experiment.changes(before, after)


if __name__ == "__main__":
    unittest.main()
