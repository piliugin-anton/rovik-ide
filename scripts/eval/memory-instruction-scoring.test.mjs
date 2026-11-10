import assert from 'node:assert/strict';
import test from 'node:test';
import { instructionFeeds, instructionScore } from './memory-instruction-scoring.mjs';

class Tensor {
  constructor(type, data, dims) {
    Object.assign(this, { type, data, dims });
  }
}
const input = { id: 'pair', input_ids: [10, 20], yes_token_id: 1, no_token_id: 2 };
const metadata = [
  ...['input_ids', 'attention_mask', 'position_ids'].map((name) => ({
    name,
    type: 'int64',
    shape: ['batch', 'sequence'],
  })),
  ...['key', 'value'].map((kind) => ({
    name: `past_key_values.0.${kind}`,
    type: 'float32',
    shape: ['batch', 8, 'past', 128],
  })),
];

test('prefill uses all prompt tokens, real positions and an empty key/value cache', () => {
  const feeds = instructionFeeds({ Tensor }, metadata, input);
  assert.deepEqual([...feeds.input_ids.data], [10n, 20n]);
  assert.deepEqual([...feeds.attention_mask.data], [1n, 1n]);
  assert.deepEqual([...feeds.position_ids.data], [0n, 1n]);
  assert.deepEqual(feeds['past_key_values.0.key'].dims, [1, 8, 0, 128]);
  assert.equal(feeds['past_key_values.0.value'].data.length, 0);
  assert.throws(() => instructionFeeds({ Tensor }, [...metadata, { name: 'unknown' }], input));
  assert.throws(() => instructionFeeds({ Tensor }, metadata.slice(1), input));
  assert.throws(() => instructionFeeds({ Tensor }, metadata, { ...input, input_ids: [] }));
  assert.throws(() =>
    instructionFeeds({ Tensor }, metadata, { ...input, input_ids: Array(513).fill(1) })
  );
});

test('binary relevance uses only final-position label logits and remains stable at extreme odds', () => {
  const logits = new Tensor('float32', new Float32Array([0, -100, 100, 0, 0, 3, 1, 0]), [1, 2, 4]);
  const score = instructionScore(logits, input);
  assert.equal(score.log_odds, 2);
  assert(Math.abs(score.probability - 0.8807970779778823) < 1e-12);
  logits.data[5] = -1000;
  assert.equal(instructionScore(logits, input).probability, 0);
  logits.data[5] = 1000;
  assert.equal(instructionScore(logits, input).probability, 1);
});

test('cached prefixes retain full attention and position offsets without copying or truncating the prefix', () => {
  const cache = {
    length: 3,
    tensors: Object.fromEntries(
      ['key', 'value'].map((kind) => [
        `past_key_values.0.${kind}`,
        new Tensor('float32', new Float32Array(3 * 8 * 128), [1, 8, 3, 128]),
      ])
    ),
  };
  const feeds = instructionFeeds({ Tensor }, metadata, input, cache);
  assert.deepEqual([...feeds.position_ids.data], [3n, 4n]);
  assert.deepEqual([...feeds.attention_mask.data], [1n, 1n, 1n, 1n, 1n]);
  assert.equal(feeds['past_key_values.0.key'], cache.tensors['past_key_values.0.key']);
  assert.throws(() => instructionFeeds({ Tensor }, metadata, input, { length: 3, tensors: {} }));
  assert.throws(() => instructionFeeds({ Tensor }, metadata, input, { length: 511, tensors: {} }));
});

test('unexpected output layouts, invalid labels and non-finite decisions fail explicitly', () => {
  const good = new Tensor('float32', new Float32Array(8), [1, 2, 4]);
  assert.throws(() => instructionScore({ ...good, dims: [2, 1, 4] }, input));
  assert.throws(() => instructionScore({ ...good, type: 'float16' }, input));
  assert.throws(() => instructionScore(good, { ...input, yes_token_id: 4 }));
  assert.throws(() => instructionScore(good, { ...input, yes_token_id: 2 }));
  good.data[5] = NaN;
  assert.throws(() => instructionScore(good, input));
});
