import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import identity from "/build/project/node_modules/fixture-identity/index.mjs";
import { reserve } from "CANDIDATE_MODULE";

const cases = JSON.parse(readFileSync("/oracle/cases.json", "utf8")).cases;
for (const example of cases) {
  test(example.id, () => {
    const result = reserve(identity(BigInt(example.before.remaining)), BigInt(example.request.amount));
    assert.deepEqual({ status: result.status, remaining: result.remaining.toString() }, example.expected);
  });
}
