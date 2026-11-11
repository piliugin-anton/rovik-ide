import { test } from 'node:test';
import assert from 'node:assert/strict';
import { sessionRecovery } from './sessionRecovery';

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test('reconnect clicks coalesce and restore only after setup completes', async () => {
  const opened = deferred();
  const events: string[] = [];
  const recover = sessionRecovery({
    context: () => 'project',
    open: async () => {
      events.push('open');
      await opened.promise;
    },
    lost: () => events.push('lost'),
    restore: async () => {
      events.push('restore');
    },
    busy: (value) => events.push(`busy:${value}`),
  });
  const first = recover();
  assert.equal(recover(), first);
  assert.deepEqual(events, ['busy:true', 'lost', 'open']);
  opened.resolve();
  await first;
  assert.deepEqual(events, ['busy:true', 'lost', 'open', 'restore', 'busy:false']);
});

test('a project changed during attachment is registered before any UI restoration', async () => {
  let context = 'a';
  const opened: string[] = [];
  const recover = sessionRecovery({
    context: () => context,
    open: async (project) => {
      opened.push(project);
      context = 'b';
    },
    lost: () => {},
    restore: async () => {
      assert.equal(opened[opened.length - 1], 'b');
    },
    busy: () => {},
  });
  await recover();
  assert.deepEqual(opened, ['a', 'b']);
});

test('failed attachment does not restore resources and a later click can recover', async () => {
  let calls = 0;
  let restored = 0;
  let busy = false;
  const recover = sessionRecovery({
    context: () => null,
    open: async () => {
      if (++calls === 1) throw new Error('host unavailable');
    },
    lost: () => {},
    restore: async () => {
      restored++;
    },
    busy: (value) => {
      busy = value;
    },
  });
  await assert.rejects(recover(), /host unavailable/);
  assert.equal(restored, 0);
  assert.equal(busy, false);
  await recover();
  assert.equal(restored, 1);
});

test('an incomplete project does not invalidate working handles', async () => {
  const recover = sessionRecovery({
    context: () => {
      throw new Error('project opening');
    },
    open: async () => {
      assert.fail('must not attach');
    },
    lost: () => assert.fail('must not invalidate'),
    restore: async () => {
      assert.fail('must not restore');
    },
    busy: () => {},
  });
  await assert.rejects(recover(), /project opening/);
});

test('failed UI restoration remains a failure and permits a fresh attempt', async () => {
  let attempts = 0;
  const recover = sessionRecovery({
    context: () => null,
    open: async () => {},
    lost: () => {},
    restore: async () => {
      if (++attempts === 1) throw new Error('watch unavailable');
    },
    busy: () => {},
  });
  await assert.rejects(recover(), /watch unavailable/);
  await recover();
  assert.equal(attempts, 2);
});
