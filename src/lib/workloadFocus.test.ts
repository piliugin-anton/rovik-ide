import { test } from 'node:test';
import assert from 'node:assert/strict';
import { workloadFocus } from './workloadFocus';
const settle = () => new Promise((resolve) => setImmediate(resolve));
test('focus reports are ordered, coalesced, and cleared after disposal', async () => {
  let chat: string | null = 'first';
  const calls: (string | null)[] = [];
  const done: (() => void)[] = [];
  const focus = workloadFocus({
    chat: () => chat,
    send: (id) => {
      calls.push(id);
      return new Promise<void>((resolve) => done.push(resolve));
    },
    failed: assert.fail,
  });
  focus.refresh();
  chat = 'skipped';
  focus.refresh();
  chat = 'latest';
  focus.refresh();
  assert.deepEqual(calls, ['first']);
  done.shift()!();
  await settle();
  assert.deepEqual(calls, ['first', 'latest']);
  focus.stop();
  focus.refresh();
  done.shift()!();
  await settle();
  assert.deepEqual(calls, ['first', 'latest', null]);
  done.shift()!();
  await settle();
});
test('returning to the same chat promotes it again and failures do not block release', async () => {
  const calls: (string | null)[] = [];
  const errors: unknown[] = [];
  const focus = workloadFocus({
    chat: () => 'chat',
    send: async (id) => {
      calls.push(id);
      if (calls.length === 1) throw new Error('disconnected');
    },
    failed: (error) => errors.push(error),
  });
  focus.refresh();
  await settle();
  focus.refresh();
  await settle();
  focus.stop();
  await settle();
  assert.deepEqual(calls, ['chat', 'chat', null]);
  assert.equal(errors.length, 1);
});
