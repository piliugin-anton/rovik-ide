import { test } from 'node:test';
import assert from 'node:assert/strict';
import { memoryGate } from './memory-gates.mjs';

const report = () => ({
  groups: { heldout: { recall_at_8: 0.95, no_match_injection_rate: 0 } },
  cases: [{ prohibited: false }],
});
const tasks = () =>
  Array.from({ length: 30 }, (_, n) => ({
    id: `incident-${n}`,
    split: 'heldout',
    memoryRelevant: n >= 5,
    memoryOff: { passed: n < 20, model: 'pinned-v1', configuration: 'tools-budget-checks-v1' },
    memoryOn: { passed: n < 25, model: 'pinned-v1', configuration: 'tools-budget-checks-v1' },
  }));
test('retrieval success alone cannot pass the agent quality gate', () =>
  assert.equal(memoryGate(report()).passed, false));
test('paired held-out improvement can pass the gate', () =>
  assert.equal(memoryGate(report(), tasks()).passed, true));
test('scope leakage fails even when task scores improve', () => {
  const r = report();
  r.cases[0].prohibited = true;
  assert.equal(memoryGate(r, tasks()).passed, false);
});
test('different models cannot manufacture a memory improvement', () => {
  const t = tasks();
  t[0].memoryOn.model = 'other';
  assert.equal(memoryGate(report(), t).passed, false);
});
test('duplicate tasks cannot inflate evidence', () => {
  const t = tasks();
  t[1].id = t[0].id;
  assert.equal(memoryGate(report(), t).passed, false);
});
test('different tool and budget configurations cannot pass a paired comparison', () => {
  const t = tasks();
  t[0].memoryOn.configuration = 'larger-budget';
  assert.equal(memoryGate(report(), t).passed, false);
});
test('missing control tasks cannot establish absence of irrelevant-task regression', () => {
  const t = tasks().map((task) => ({ ...task, memoryRelevant: true }));
  assert.equal(memoryGate(report(), t).passed, false);
});
test('a regression on an irrelevant task blocks release', () => {
  const t = tasks();
  t[0].memoryRelevant = false;
  t[0].memoryOn.passed = false;
  assert.equal(memoryGate(report(), t).passed, false);
});
