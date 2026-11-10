import { test } from 'node:test';
import assert from 'node:assert/strict';
import { reconnectBackend, onBackendLost } from './backendSession';
import { spawnTerminal, type TerminalEvent } from './terminal';
import { subscribeVirtualFiles } from './virtualFiles';

/* eslint-disable @typescript-eslint/no-explicit-any */
let backend: (command: string, args: any) => Promise<any>;
let next = 0;
(globalThis as any).window = {
  setTimeout,
  clearTimeout,
  __TAURI_INTERNALS__: {
    transformCallback: () => ++next,
    invoke: (command: string, args: any) => backend(command, args),
  },
};

test('reconnect ends terminal views once and never repeats their spawn', async () => {
  let channel: any;
  const calls: string[] = [];
  backend = async (command, args) => {
    calls.push(command);
    if (command === 'spawn_terminal') {
      channel = args.onEvent;
      return { id: 'old', shell: 'sh' };
    }
    assert.equal(command, 'desktop_reconnect');
  };
  const events: TerminalEvent[] = [];
  await spawnTerminal(null, 80, 24, (event) => events.push(event));
  channel.onmessage({ type: 'output', data: 'before' });
  await reconnectBackend();
  channel.onmessage({ type: 'output', data: 'late' });
  channel.onmessage({ type: 'exit', code: 0 });
  assert.deepEqual(events, [
    { type: 'output', data: 'before' },
    { type: 'exit', code: null },
  ]);
  assert.deepEqual(calls, ['spawn_terminal', 'desktop_reconnect']);
});

test('a terminal startup reply from an abandoned session cannot revive its handle', async () => {
  let finish!: (value: unknown) => void;
  backend = async (command) => {
    if (command === 'spawn_terminal')
      return new Promise((resolve) => {
        finish = resolve;
      });
    assert.equal(command, 'desktop_reconnect');
  };
  const events: TerminalEvent[] = [];
  const terminal = spawnTerminal(null, 80, 24, (event) => events.push(event));
  const rejected = assert.rejects(terminal, /session ended during reconnect/);
  await reconnectBackend();
  finish({ id: 'late', shell: 'sh' });
  await rejected;
  assert.deepEqual(events, [{ type: 'exit', code: null }]);
});

test('late VFS subscription replies cannot unsubscribe a reused ID in the new session', async () => {
  let finish!: (value: number) => void;
  let subscriptions = 0;
  const unsubscribed: number[] = [];
  const channels: any[] = [];
  const batches: unknown[] = [];
  backend = async (command, args) => {
    if (command === 'vfs_subscribe') {
      channels.push(args.onEvents);
      if (++subscriptions === 1)
        return new Promise((resolve) => {
          finish = resolve;
        });
      return 1;
    }
    if (command === 'vfs_unsubscribe') {
      unsubscribed.push(args.id);
      return true;
    }
    assert.equal(command, 'desktop_reconnect');
  };
  const subscribing = subscribeVirtualFiles((events, dropped) => batches.push({ events, dropped }));
  await reconnectBackend();
  finish(1);
  const stop = await subscribing;
  channels[0].onmessage({ events: ['old'], dropped: 0 });
  assert.deepEqual(unsubscribed, []);
  assert.deepEqual(batches, [{ events: [], dropped: 1 }]);
  await stop();
  channels[1].onmessage({ events: ['late'], dropped: 0 });
  assert.deepEqual(unsubscribed, [1]);
  assert.equal(batches.length, 1);
});

test('a failed view callback cannot prevent native session cleanup', async () => {
  let attached = false;
  let logged = false;
  const original = console.error;
  const remove = onBackendLost(() => {
    throw new Error('disposed view');
  });
  console.error = () => {
    logged = true;
  };
  backend = async (command) => {
    assert.equal(command, 'desktop_reconnect');
    attached = true;
  };
  try {
    await reconnectBackend();
    assert.equal(attached, true);
    assert.equal(logged, true);
  } finally {
    remove();
    console.error = original;
  }
});

test('embedded Windows recovery cannot invalidate live handles or invoke Unix attachment', async () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  let lost = false;
  const remove = onBackendLost(() => {
    lost = true;
  });
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: { platform: 'Win32' },
  });
  backend = async () => assert.fail('embedded backend cannot be replaced');
  try {
    await assert.rejects(reconnectBackend(), /embedded backend/);
    assert.equal(lost, false);
  } finally {
    remove();
    if (original) Object.defineProperty(globalThis, 'navigator', original);
    else Reflect.deleteProperty(globalThis, 'navigator');
  }
});
