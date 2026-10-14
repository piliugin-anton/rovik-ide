// composerImages.ts tests (node:test): what a chip's payload reads back as.
// How an image is stored: composerAttachments.test.ts.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseComposerImage } from './composerImages';

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
