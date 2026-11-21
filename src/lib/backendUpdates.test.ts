import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  BACKEND_UPDATE_NOTE,
  createBackendUpdater,
  type BackendUpdateStatus,
} from './backendUpdates';
import { dismiss, notifications } from './notifications';

(globalThis as unknown as { window: unknown }).window = {
  setTimeout: (run: () => void, ms: number) => {
    const timer = setTimeout(run, ms);
    timer.unref();
    return timer;
  },
  clearTimeout,
};

const outdated: BackendUpdateStatus = {
  restartRequired: true,
  backendBoot: 'old-boot',
  backendVersion: '0.2.16',
  appVersion: '0.2.17',
  backendProtocol: 1,
  appProtocol: 2,
  backendBuild: 'old-build',
  appBuild: 'new-build',
};
const note = () => notifications().find((entry) => entry.id === BACKEND_UPDATE_NOTE);
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function harness() {
  dismiss(BACKEND_UPDATE_NOTE);
  let status: BackendUpdateStatus | null = { ...outdated };
  let focus = () => {};
  let reading = async () => status;
  let replacing = async (_status: BackendUpdateStatus) => {};
  let reattaching = async () => {};
  let reads = 0;
  let reconnects = 0;
  const restarts: BackendUpdateStatus[] = [];
  const updater = createBackendUpdater({
    status: async () => {
      reads++;
      return reading();
    },
    restart: async (current) => {
      restarts.push(current);
      await replacing(current);
    },
    reconnect: async () => {
      reconnects++;
      await reattaching();
    },
    onFocus: (check) => {
      focus = check;
      return () => {
        focus = () => {};
      };
    },
  });
  return {
    updater,
    restarts,
    reads: () => reads,
    reconnects: () => reconnects,
    focus: () => focus(),
    status: (value: BackendUpdateStatus | null) => {
      status = value;
    },
    read: (run: typeof reading) => {
      reading = run;
    },
    replace: (run: typeof replacing) => {
      replacing = run;
    },
    reconnect: (run: typeof reattaching) => {
      reattaching = run;
    },
  };
}

test('launch and focus quietly check current or absent backends without stopping work', async () => {
  const h = harness();
  h.status({ ...outdated, restartRequired: false });
  const stop = h.updater.start();
  await h.updater.check();
  assert.equal(note(), undefined);
  h.status(null);
  h.focus();
  await h.updater.check();
  assert.equal(h.reads(), 2);
  assert.equal(note(), undefined);
  assert.deepEqual(h.restarts, []);
  assert.equal(h.reconnects(), 0);
  stop();
  h.focus();
  assert.equal(h.reads(), 2);
});

test('an incompatible or rebuilt backend is offered persistently even at the same app version', async () => {
  const h = harness();
  h.status({ ...outdated, backendVersion: outdated.appVersion });
  const stop = h.updater.start();
  await h.updater.check();
  assert.equal(note()?.title, 'Backend update ready');
  assert.equal(note()?.timeout, undefined);
  assert.match(note()?.message ?? '', /every Rovik window/);
  assert.match(note()?.message ?? '', /Unsaved editor text and chat drafts stay open/);
  assert.equal(note()?.actions?.[0].label, 'Restart backend');
  assert.equal(h.updater.available(), true);
  assert.deepEqual(h.restarts, []);
  dismiss(BACKEND_UPDATE_NOTE);
  h.focus();
  await h.updater.check();
  assert.equal(
    note(),
    undefined,
    'focusing does not repeat a dismissed offer for the same backend'
  );
  stop();
});

test('a later check offers an available backend after an earlier current result', async () => {
  const h = harness();
  h.status({ ...outdated, restartRequired: false });
  await h.updater.check();
  h.status(outdated);
  await h.updater.check();
  assert.equal(note()?.title, 'Backend update ready');
  assert.deepEqual(h.restarts, []);
});

test('restart clicks coalesce and the existing notification shows progress then success', async () => {
  const h = harness();
  let finish!: () => void;
  h.replace(async (status) => {
    assert.deepEqual(status, outdated);
    await new Promise<void>((resolve) => {
      finish = resolve;
    });
  });
  await h.updater.check();
  const click = note()!.actions![0].run;
  click();
  click();
  const pending = h.updater.restart();
  assert.equal(h.updater.restart(), pending);
  assert.equal(note()?.progress, 'busy');
  assert.equal(note()?.actions, undefined);
  await settle();
  assert.equal(h.restarts.length, 1);
  finish();
  await pending;
  assert.equal(note()?.kind, 'success');
  assert.equal(note()?.title, 'Backend updated');
  assert.equal(h.reconnects(), 0);
});

test('a failed restoration stays actionable across focus and retries by reconnecting the new backend', async () => {
  const h = harness();
  h.replace(async () => {
    h.status({ ...outdated, restartRequired: false, backendBoot: 'new-boot' });
    throw new Error('Project watch could not be restored');
  });
  const stop = h.updater.start();
  await h.updater.check();
  await h.updater.restart();
  assert.equal(note()?.kind, 'error');
  assert.match(note()?.message ?? '', /Project watch could not be restored/);
  assert.equal(note()?.actions?.[0].label, 'Try again');
  h.focus();
  await h.updater.check();
  assert.equal(note()?.kind, 'error');
  note()!.actions![0].run();
  await h.updater.restart();
  assert.equal(h.restarts.length, 1);
  assert.equal(h.reconnects(), 1);
  assert.equal(note()?.kind, 'success');
  stop();
});

test('a replacement by another window is reconnected without stopping it again', async () => {
  const h = harness();
  await h.updater.check();
  h.status({ ...outdated, restartRequired: false, backendBoot: 'already-updated' });
  await h.updater.restart();
  assert.deepEqual(h.restarts, []);
  assert.equal(h.reconnects(), 1);
  assert.equal(note()?.kind, 'success');
});

test('a late focus check cannot overwrite replacement progress or its result', async () => {
  const h = harness();
  await h.updater.check();
  let finish!: (status: BackendUpdateStatus) => void;
  h.read(() => new Promise((resolve) => (finish = resolve)));
  const checking = h.updater.check();
  h.read(async () => outdated);
  await h.updater.restart();
  assert.equal(note()?.kind, 'success');
  finish(outdated);
  await checking;
  assert.equal(note()?.kind, 'success');
});

test('status errors provide a retry and recover without logging or automatic replacement', async () => {
  const h = harness();
  h.read(async () => {
    throw new Error('Owner metadata is unavailable');
  });
  await h.updater.check();
  assert.equal(h.updater.available(), false, 'a failed check is not a confirmed backend update');
  assert.match(note()?.message ?? '', /Owner metadata is unavailable/);
  assert.equal(note()?.actions?.[0].label, 'Check again');
  h.read(async () => outdated);
  note()!.actions![0].run();
  await h.updater.check();
  assert.equal(note()?.title, 'Backend update ready');
  assert.deepEqual(h.restarts, []);
});

test('installing the app suppresses backend actions until the updated UI relaunches', async () => {
  const h = harness();
  await h.updater.check();
  const staleClick = note()!.actions![0].run;
  const reads = h.reads();
  h.updater.deferUntilRelaunch();
  assert.equal(note(), undefined);
  assert.equal(h.updater.available(), false);
  staleClick();
  await h.updater.check();
  await h.updater.restart();
  assert.equal(h.reads(), reads);
  assert.deepEqual(h.restarts, []);
  assert.equal(h.reconnects(), 0);
});

test('an app installation fences a status read already awaiting a restart click', async () => {
  const h = harness();
  await h.updater.check();
  let finish!: (status: BackendUpdateStatus) => void;
  h.read(() => new Promise((resolve) => (finish = resolve)));
  const pending = h.updater.restart();
  h.updater.deferUntilRelaunch();
  finish(outdated);
  await pending;
  assert.deepEqual(h.restarts, []);
  assert.equal(note(), undefined);
});
