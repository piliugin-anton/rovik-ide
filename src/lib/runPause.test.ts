import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { RunPause } from '../types';
import { pauseText } from './runPause';

test('a move names where the chat went, not a stuck agent', () => {
  const text = pauseText({ kind: 'move', toolCalls: 7, host: 'build-server' });
  assert.equal(text, 'Moved to build-server.');
});

test('a move back names no host', () => {
  assert.equal(pauseText({ kind: 'move', toolCalls: 7 }), 'Moved back to this computer.');
});

test('a pause while the server’s Rovik restarted says so', () => {
  assert.equal(
    pauseText({ kind: 'drain', toolCalls: 3 }),
    'Paused while Rovik restarted on the server.'
  );
});

test('a checkpoint and a loop keep their own text', () => {
  assert.equal(pauseText({ kind: 'checkpoint', toolCalls: 50 }), 'Paused after 50 tool calls.');
  assert.equal(
    pauseText({ kind: 'loop', toolCalls: 12, reason: 'it ran the same command three times' }),
    'Paused after 12 tool calls: the agent seems stuck, it ran the same command three times.'
  );
});

test('a kind this app does not know reads as a plain pause', () => {
  const newer = { kind: 'later', toolCalls: 4 } as unknown as RunPause;
  assert.equal(pauseText(newer), 'Paused after 4 tool calls.');
});
