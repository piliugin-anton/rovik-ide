// What a model's replies taught about its reasoning fields (reasoning.ts):
// nothing new, nothing saved. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EFFORTS, effectiveEffort, mergeLearned, REASONING_FIELDS } from './reasoning';

const [field, other] = REASONING_FIELDS;

test('a field known to work, working again, changes nothing', () => {
  const learned = mergeLearned(undefined, [field], [], [], 1000);
  assert.deepEqual(learned.accepted, [field]);
  // The same object: the settings aren't saved again after every reply.
  assert.equal(mergeLearned(learned, [field], [], [], 2000), learned);
  assert.equal(mergeLearned(learned, [], [], [], 2000), learned);
});

test('what is new is learned', () => {
  const learned = mergeLearned(undefined, [field], [], [], 1000);
  const rejected = mergeLearned(learned, [], [other], [], 2000);
  assert.notEqual(rejected, learned);
  assert.deepEqual(rejected.rejected, [{ field: other, at: 2000 }]);
  // Rejected again later: the new time is kept (it's tried again after a while).
  const again = mergeLearned(rejected, [], [other], [], 3000);
  assert.deepEqual(again.rejected, [{ field: other, at: 3000 }]);
  // Accepted after all: the rejection goes.
  assert.deepEqual(mergeLearned(again, [other], [], [], 4000).rejected, []);
});

test('extra high and maximum are distinct effort choices above high', () => {
  assert.deepEqual(
    EFFORTS.slice(-3).map((e) => e.id),
    ['high', 'xhigh', 'max']
  );
});

test('a probed fallback updates upper effort selections and preserves existing high', () => {
  assert.equal(effectiveEffort('max', 'xhigh'), 'xhigh');
  assert.equal(effectiveEffort('xhigh', 'high'), 'high');
  assert.equal(effectiveEffort('high', 'max'), 'high');
  assert.equal(effectiveEffort('max', undefined), 'max');
  assert.equal(effectiveEffort('max', 'unknown'), 'max');
});
