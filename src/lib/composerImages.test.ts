// composerImages.ts tests (node:test): how big images are scaled, and what
// a chip's payload reads back as.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fittedSize, MAX_IMAGE_SIDE, parseComposerImage } from './composerImages';

test('big images are scaled to the longest side, small ones kept', () => {
  assert.deepEqual(fittedSize(1280, 800), { width: 1280, height: 800 });
  assert.deepEqual(fittedSize(MAX_IMAGE_SIDE, 10), { width: MAX_IMAGE_SIDE, height: 10 });
  assert.deepEqual(fittedSize(3840, 2160), { width: 2048, height: 1152 });
  assert.deepEqual(fittedSize(1000, 4096), { width: 500, height: 2048 });
  // A sliver keeps at least a pixel.
  assert.deepEqual(fittedSize(100_000, 10), { width: 2048, height: 1 });
});

test('a chip payload reads back only as a whole image', () => {
  const id = 'a'.repeat(64);
  assert.deepEqual(parseComposerImage(JSON.stringify({ id, width: 10, height: 20 })), {
    id,
    width: 10,
    height: 20,
  });
  // Copied from a sent message: no size known.
  assert.deepEqual(parseComposerImage(JSON.stringify({ id })), { id });
  assert.equal(parseComposerImage(undefined), null);
  assert.equal(parseComposerImage('not json'), null);
  assert.equal(parseComposerImage('null'), null);
  assert.equal(parseComposerImage(JSON.stringify({ id: 'short', width: 1, height: 1 })), null);
  assert.equal(parseComposerImage(JSON.stringify({ id: '../x'.padEnd(64, 'a') })), null);
  assert.equal(parseComposerImage(JSON.stringify({ id, width: '10', height: 20 })), null);
  assert.equal(parseComposerImage(JSON.stringify({ id, width: -1, height: 20 })), null);
});
