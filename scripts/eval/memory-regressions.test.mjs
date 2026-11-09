import { test } from 'node:test';
import assert from 'node:assert/strict';
import { auditMemoryResults } from './memory-regressions.mjs';

function row() {
  return {
    id: 'case',
    answerable: true,
    primary_id: 'primary',
    primary_claim_hash: 'primary-claim',
    primary_delivered: false,
    rank: null,
    returned: 2,
    returned_ids: ['a', 'b'],
    rendered_ids: ['a'],
    full_text_ids: ['a'],
    packet_tokens: 80,
    required_count: 3,
    required_delivered: 1,
    answer_delivered: false,
    required_facts: [
      { id: 'a', claim_hash: 'claim-a', title: 'Same title', rank: 1, delivered: true },
      { id: 'b', claim_hash: 'claim-b', title: 'Same title', rank: 2, delivered: false },
    ],
    prohibited: false,
    embedding_fallback: false,
  };
}
const result = (c) => ({ cases: [c] });
test('same-title delivery swaps fail even with unchanged counts, ranks and incomplete answers', () => {
  const before = row(),
    after = structuredClone(before);
  after.required_facts[0].delivered = false;
  after.required_facts[1].delivered = true;
  after.rendered_ids = ['b'];
  after.full_text_ids = ['b'];
  const audit = auditMemoryResults(result(before), result(after));
  assert.equal(audit.safe, false);
  assert.ok(audit.errors.some((e) => e.kind === 'required-delivery-loss' && e.fact === 'a'));
});
test('a negative full claim changing to a fetch hint is detected despite identical returned ids', () => {
  const before = row();
  before.answerable = false;
  const after = structuredClone(before);
  after.full_text_ids = [];
  after.required_facts[0].delivered = false;
  after.required_delivered = 0;
  const audit = auditMemoryResults(result(before), result(after));
  assert.ok(audit.errors.some((e) => e.kind === 'negative-full_text_ids'));
});
test('missing evidence, duplicate cases, lost facts, scope violations and overruns fail closed', () => {
  for (const mutate of [
    (c) => delete c.required_facts,
    (c) => c.required_facts.pop(),
    (c) => (c.prohibited = true),
    (c) => delete c.prohibited,
    (c) => delete c.packet_tokens,
    (c) => (c.required_facts[0].claim_hash = 'changed-claim'),
    (c) => (c.packet_tokens = 1001),
    (c) => c.rendered_ids.push('foreign'),
  ]) {
    const before = row(),
      after = structuredClone(before);
    mutate(after);
    assert.equal(auditMemoryResults(result(before), result(after)).safe, false);
  }
  assert.equal(auditMemoryResults(result(row()), { cases: [row(), row()] }).safe, false);
  assert.equal(auditMemoryResults(result(row()), { cases: [] }).safe, false);
  assert.throws(() => auditMemoryResults(result(row()), result(row()), { memoryBudget: NaN }));
});
test('unchanged complete and incomplete packet evidence passes', () => {
  assert.equal(auditMemoryResults(result(row()), result(row())).safe, true);
});
