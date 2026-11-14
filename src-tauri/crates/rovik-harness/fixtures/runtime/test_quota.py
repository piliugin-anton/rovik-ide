import json
from pathlib import Path

import pytest
from fixture_identity import identity
from implementations.quota.quota import reserve

CASES = json.loads(Path("/oracle/cases.json").read_text())["cases"]


@pytest.mark.parametrize("example", CASES, ids=[case["id"] for case in CASES])
def test_reserve(example):
    result = reserve(identity(int(example["before"]["remaining"])), int(example["request"]["amount"]))
    assert {"status": result["status"], "remaining": str(result["remaining"])} == example["expected"]
