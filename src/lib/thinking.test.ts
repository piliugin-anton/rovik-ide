// How thinking reads in the chat (lib/thinking.ts). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { latestLine, thoughtLabel } from './thinking';

test('a thought says it’s thinking, then how long it took, in whole seconds of at least one', () => {
  assert.equal(thoughtLabel(true, 3_400), 'Thinking…');
  assert.equal(thoughtLabel(false, 3_400), 'Thought for 3 s');
  assert.equal(thoughtLabel(false, 120), 'Thought for 1 s');
  assert.equal(thoughtLabel(false, undefined), 'Thought', 'its time not known');
  assert.equal(thoughtLabel(false, null), 'Thought');
});

test('its latest line is the last one it wrote, the end of a long one', () => {
  assert.equal(latestLine('Read the file.\nThen the tests.\n\n'), 'Then the tests.');
  assert.equal(latestLine(''), '');
  const long = 'x'.repeat(200) + 'END';
  assert.equal(latestLine(long), `…${long.slice(-160)}`);
});
