import { test } from 'node:test';
import assert from 'node:assert/strict';
import { followEvents, type EventBatch, type EventScope } from './eventReplay';

const batch = (cursor: number, reset = false): EventBatch => ({
  cursor,
  reset,
  events: reset ? [] : [{ channel: 'agent', payload: cursor }],
});
const scope: EventScope = { projectId: 'a', chats: [] };

test('a failed read retries its checkpoint and never refreshes or repeats applied events', async () => {
  let calls = 0;
  const cursors: unknown[] = [];
  const applied: unknown[] = [];
  const reader = followEvents({
    scope: () => scope,
    poll: async (_, cursor) => {
      cursors.push(cursor);
      if (++calls === 2) throw new Error('disconnected');
      return batch(calls);
    },
    refresh: async () => assert.fail('unexpected reset'),
    apply: (event) => {
      applied.push(event.payload);
      if (applied.length === 2) reader.stop();
    },
    failed: () => {},
    recovered: () => {},
    pause: async () => {},
  });
  await reader.done;
  assert.deepEqual(cursors, [null, 1, 1]);
  assert.deepEqual(applied, [1, 3]);
});

test('a reset checkpoint is not accepted until authoritative recovery succeeds', async () => {
  let refreshes = 0;
  const cursors: unknown[] = [];
  const reader = followEvents({
    scope: () => scope,
    poll: async (_, cursor) => {
      cursors.push(cursor);
      return cursor === null ? batch(10, true) : batch(11);
    },
    refresh: async () => {
      if (++refreshes === 1) throw new Error('snapshot unavailable');
    },
    apply: () => reader.stop(),
    failed: () => {},
    recovered: () => {},
    pause: async () => {},
  });
  await reader.done;
  assert.deepEqual(cursors, [null, null, 10]);
});

test('a project switch discards an old poll reply and starts a fresh scope', async () => {
  let current = scope;
  let calls = 0;
  const reader = followEvents({
    scope: () => current,
    poll: async (_, cursor) => {
      assert.equal(cursor, null);
      if (++calls === 1) {
        current = { projectId: 'b', chats: [] };
        return batch(1);
      }
      return batch(2, true);
    },
    refresh: async (scope) => {
      assert.equal(scope.projectId, 'b');
      reader.stop();
    },
    apply: () => assert.fail('old project event applied'),
    failed: () => {},
    recovered: () => {},
    pause: async () => {},
  });
  await reader.done;
});

test('disposing a reader rejects its late response', async () => {
  let finish!: (batch: EventBatch) => void;
  const reader = followEvents({
    scope: () => scope,
    poll: () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
    refresh: async () => assert.fail('late refresh'),
    apply: () => assert.fail('late event'),
    failed: () => {},
    recovered: () => {},
    pause: async () => {},
  });
  reader.stop();
  finish(batch(1));
  await reader.done;
});

test('a handler failure forces a snapshot instead of replaying partially applied side effects', async () => {
  const cursors: unknown[] = [];
  let refreshes = 0;
  const reader = followEvents({
    scope: () => scope,
    poll: async (_, cursor) => {
      cursors.push(cursor);
      return cursors.length === 1 ? batch(1) : batch(2, true);
    },
    apply: () => {
      throw new Error('UI handler failed');
    },
    refresh: async () => {
      refreshes++;
      reader.stop();
    },
    failed: () => {},
    recovered: () => {},
    pause: async () => {},
  });
  await reader.done;
  assert.deepEqual(cursors, [null, null]);
  assert.equal(refreshes, 1);
});

test('a replaced command session discards an old snapshot even in the same project', async () => {
  let version = 0;
  let calls = 0;
  const reader = followEvents({
    scope: () => scope,
    version: () => version,
    poll: async (_, cursor) => {
      assert.equal(cursor, null);
      return batch(++calls, true);
    },
    refresh: async (_, current) => {
      if (calls === 1) {
        version++;
        assert.equal(current(), false);
      } else reader.stop();
    },
    apply: () => assert.fail('old snapshot events applied'),
    failed: () => assert.fail('unexpected failure'),
    recovered: () => assert.fail('obsolete checkpoint accepted'),
    pause: async () => {},
  });
  await reader.done;
  assert.equal(calls, 2);
});
