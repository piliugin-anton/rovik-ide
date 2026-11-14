import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  backendProject,
  reconnectBackend,
  reconnectUpdatedBackend,
  restartBackend,
  onBackendLost,
  onBackendRestored,
  reportBackendFailure,
} from './backendSession';
import { spawnTerminal, writeToTerminal, type TerminalEvent } from './terminal';
import { subscribeVirtualFiles } from './virtualFiles';
import { notifications, dismiss } from './notifications';

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

test('Windows offers shared backend recovery and invalidates old session handles', async () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  let lost = false;
  const remove = onBackendLost(() => {
    lost = true;
  });
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: { platform: 'Win32' },
  });
  let attached = false;
  backend = async (command) => {
    assert.equal(command, 'desktop_reconnect');
    attached = true;
  };
  try {
    reportBackendFailure(new Error('connection closed'));
    const notice = notifications().find((item) => item.id === 'backend-session');
    assert.equal(notice?.actions?.[0].label, 'Reconnect');
    await reconnectBackend();
    assert.equal(lost, true);
    assert.equal(attached, true);
  } finally {
    remove();
    dismiss('backend-session');
    if (original) Object.defineProperty(globalThis, 'navigator', original);
    else Reflect.deleteProperty(globalThis, 'navigator');
  }
});

test('terminal input keeps call order without blocking another terminal', async () => {
  const calls: string[] = [];
  let finish!: () => void;
  backend = async (command, args) => {
    assert.equal(command, 'write_to_terminal');
    calls.push(`${args.id}:${args.data}`);
    if (args.id === 'a' && args.data === 'first')
      await new Promise<void>((resolve) => {
        finish = resolve;
      });
  };
  const first = writeToTerminal('a', 'first');
  const second = writeToTerminal('a', 'second');
  await writeToTerminal('b', 'independent');
  assert.deepEqual(calls, ['a:first', 'b:independent']);
  finish();
  await Promise.all([first, second]);
  assert.deepEqual(calls, ['a:first', 'b:independent', 'a:second']);
});

test('a failed terminal write rejects the queued rest of the command', async () => {
  const calls: string[] = [];
  let fail!: (error: Error) => void;
  backend = async (command, args) => {
    assert.equal(command, 'write_to_terminal');
    calls.push(args.data);
    await new Promise<void>((_, reject) => {
      fail = reject;
    });
  };
  const first = assert.rejects(writeToTerminal('failed', 'first'), /write failed/);
  const second = assert.rejects(writeToTerminal('failed', 'second'), /write failed/);
  await Promise.resolve();
  fail(new Error('write failed'));
  await Promise.all([first, second]);
  assert.deepEqual(calls, ['first']);
});

test('queued terminal input is never sent into a replacement backend', async () => {
  const calls: string[] = [];
  let finish!: () => void;
  backend = async (command, args) => {
    calls.push(command === 'write_to_terminal' ? args.data : command);
    if (command === 'write_to_terminal')
      await new Promise<void>((resolve) => {
        finish = resolve;
      });
    else assert.equal(command, 'desktop_reconnect');
  };
  const first = writeToTerminal('old-input', 'accepted');
  const queued = assert.rejects(writeToTerminal('old-input', 'queued'), /session ended/);
  await Promise.resolve();
  await reconnectBackend();
  finish();
  await Promise.all([first, queued]);
  assert.deepEqual(calls, ['accepted', 'desktop_reconnect']);
});

test('backend replacement sends the reviewed identity and restores the latest project without replay', async () => {
  const target = {
    expectedBoot: 'reviewed-boot',
    expectedVersion: '0.2.16',
    expectedProtocol: 1,
    expectedBuild: 'reviewed-build',
  };
  let project = { root: '/first', projectId: 'first' };
  const clearProject = backendProject(() => project);
  const calls: unknown[] = [];
  let channel: any;
  let finish!: () => void;
  const restore = onBackendRestored(() => {
    calls.push('restore');
  });
  backend = async (command, args) => {
    if (command === 'spawn_terminal') {
      channel = args.onEvent;
      return { id: 'old', shell: 'sh' };
    }
    calls.push({ command, args });
    if (command === 'desktop_restart_backend')
      await new Promise<void>((resolve) => {
        finish = resolve;
      });
    else assert.equal(command, 'desktop_reconnect');
  };
  const events: TerminalEvent[] = [];
  try {
    await spawnTerminal(null, 80, 24, (event) => events.push(event));
    const restarting = restartBackend(target);
    assert.equal(restartBackend(target), restarting);
    const reconnecting = reconnectBackend();
    assert.deepEqual(events, [{ type: 'exit', code: null }]);
    project = { root: '/second', projectId: 'second' };
    finish();
    await Promise.all([restarting, reconnecting]);
    channel.onmessage({ type: 'output', data: 'old output' });
    assert.deepEqual(events, [{ type: 'exit', code: null }]);
    assert.deepEqual(calls, [
      {
        command: 'desktop_restart_backend',
        args: { ...target, root: '/first', projectId: 'first' },
      },
      { command: 'desktop_reconnect', args: { root: '/second', projectId: 'second' } },
      'restore',
    ]);
  } finally {
    clearProject();
    restore();
  }
});

test('an incompatible startup backend can be replaced before the selected folder has an identity', async () => {
  let project: { root: string | null; projectId: string | null } = {
    root: '/startup-folder',
    projectId: null,
  };
  const calls: unknown[] = [];
  const clearProject = backendProject(
    () => project,
    async () => {
      calls.push('resolve-folder');
      project = { ...project, projectId: 'resolved-project' };
    }
  );
  const restore = onBackendRestored(() => {
    calls.push('load-history-and-subscriptions');
  });
  const target = {
    expectedBoot: 'incompatible',
    expectedVersion: '0.2.16',
    expectedProtocol: 1,
    expectedBuild: 'old',
  };
  backend = async (command, args) => {
    calls.push({ command, args });
  };
  try {
    await assert.rejects(reconnectBackend(), /project to finish opening/);
    assert.deepEqual(calls, []);
    await restartBackend(target);
    assert.deepEqual(calls, [
      {
        command: 'desktop_restart_backend',
        args: { ...target, root: null, projectId: null },
      },
      'resolve-folder',
      {
        command: 'desktop_reconnect',
        args: { root: '/startup-folder', projectId: 'resolved-project' },
      },
      'load-history-and-subscriptions',
    ]);
  } finally {
    clearProject();
    restore();
  }
});

test('a project resolution failure after replacement retries setup without stopping the new backend', async () => {
  let project: { root: string | null; projectId: string | null } = {
    root: '/startup-folder',
    projectId: null,
  };
  let attempts = 0;
  const clearProject = backendProject(
    () => project,
    async () => {
      if (++attempts === 1) throw new Error('Folder could not resolve');
      project = { ...project, projectId: 'resolved-project' };
    }
  );
  const commands: string[] = [];
  backend = async (command) => {
    commands.push(command);
  };
  try {
    await assert.rejects(
      restartBackend({
        expectedBoot: 'incompatible',
        expectedVersion: '0.2.16',
        expectedProtocol: 1,
        expectedBuild: 'old',
      }),
      /Folder could not resolve/
    );
    await reconnectUpdatedBackend();
    assert.deepEqual(commands, [
      'desktop_restart_backend',
      'desktop_reconnect',
      'desktop_reconnect',
    ]);
  } finally {
    clearProject();
  }
});
