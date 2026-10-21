// updates.ts tests (node:test): when Rovik looks for a new version, what it
// offers, and what Update does, step by step: the download, what starting
// again would stop, the install, and the restart. The app is answered here,
// command by command; the windows' shared settings are a map of the test's.
import { test } from 'node:test';
import assert from 'node:assert/strict';

(globalThis as any).window = {
  setTimeout: (run: () => void, ms: number) => {
    const timer = setTimeout(run, ms);
    timer.unref();
    return timer;
  },
  clearTimeout: (timer: ReturnType<typeof setTimeout>) => clearTimeout(timer),
};

const {
  CHECK_EVERY_MS,
  UPDATE_NOTE,
  agoText,
  createUpdater,
  hostsLine,
  isNewer,
  pollStep,
  restartWarning,
  statusText,
  updatesSummary,
} = await import('./updates');
const { notifications, dismiss } = await import('./notifications');
const { sharedMap } = await import('./sharedState');
type Available = import('./updates').Available;
type AtWork = import('./updates').AtWork;
type Progress = import('./updates').Progress;

const NOW = 1_800_000_000_000;
const release = (version: string) => ({
  version,
  page: `https://github.com/o/r/releases/tag/v${version}`,
});
const found = (version: string, over: Partial<Available> = {}): Available => ({
  ...release(version),
  current: '1.0.0',
  installable: true,
  ...over,
});
const idle: AtWork = { here: 0, elsewhere: 0, otherWindows: false, closesAll: false };

test('versions compare number by number, a pre-release before its release', () => {
  assert.ok(isNewer('1.2.10', '1.2.9'));
  assert.ok(isNewer('2.0.0', '1.99.99'));
  assert.ok(isNewer('1.2.3', '1.2.3-4'));
  assert.ok(isNewer('1.2.3-5', '1.2.3-4'));
  assert.ok(isNewer('v1.2.4', '1.2.3'));
  assert.ok(!isNewer('1.2.3', '1.2.3'));
  assert.ok(!isNewer('1.2.3-4', '1.2.3'));
  assert.ok(!isNewer('0.9.0', '0.10.0'));
});

test('a window looks every 30 minutes, any window’s look counting', () => {
  const v = '1.0.0';
  assert.equal(pollStep({}, v, NOW), 'check');
  assert.equal(pollStep({ lastCheck: NOW - 60_000 }, v, NOW), 'none');
  assert.equal(pollStep({ lastCheck: NOW - CHECK_EVERY_MS }, v, NOW), 'check');
  // A clock set back.
  assert.equal(pollStep({ lastCheck: NOW + 3_600_000 }, v, NOW), 'check');
  // What another window's look found is offered without a look of its own.
  const latest = release('1.1.0');
  assert.equal(pollStep({ lastCheck: NOW - 60_000, latest }, v, NOW), 'offer');
  assert.equal(pollStep({ lastCheck: NOW - 60_000, latest }, '1.1.0', NOW), 'none');
  // Turned off: no looks, and nothing offered.
  assert.equal(pollStep({ checkForUpdates: false }, v, NOW), 'off');
  assert.equal(pollStep({ checkForUpdates: false, latest }, v, NOW), 'off');
  // Installed by another window: this one starts again on it, looks or not.
  assert.equal(pollStep({ installed: '1.1.0', checkForUpdates: false }, v, NOW), 'restart');
  assert.equal(pollStep({ installed: '1.1.0' }, '1.1.0', NOW), 'check');
});

test('what starting again stops is said; nothing at stake asks nothing', () => {
  assert.equal(restartWarning(idle), null);
  assert.equal(
    restartWarning({ ...idle, here: 1 }),
    '1 chat in this window is at work: it stops, and you can continue it once Rovik is back.'
  );
  assert.match(
    restartWarning({ ...idle, here: 2 })!,
    /2 chats in this window are at work: they stop/
  );
  // Elsewhere only the installer closes the other windows.
  assert.equal(restartWarning({ ...idle, otherWindows: true, elsewhere: 2 }), null);
  assert.equal(
    restartWarning({ here: 0, elsewhere: 0, otherWindows: true, closesAll: true }),
    'The installer also closes Rovik’s other windows: save what’s open in them first.'
  );
  assert.equal(
    restartWarning({ here: 0, elsewhere: 1, otherWindows: true, closesAll: true }),
    'The installer also closes Rovik’s other windows, and the 1 chat at work there stops: save what’s open in them first.'
  );
});

test('the page and the menu say where the update stands', () => {
  const info = { version: '1.0.0', installable: true, periodic: true };
  assert.equal(statusText({ kind: 'idle' }, NOW), null);
  assert.equal(
    statusText({ kind: 'upToDate', at: NOW - 5 * 60_000 }, NOW)?.text,
    'This is the latest version (looked 5 min ago).'
  );
  assert.equal(
    statusText(
      { kind: 'downloading', release: release('1.1.0'), done: 5 << 20, total: 20 << 20 },
      NOW
    )?.text,
    'Downloading Rovik 1.1.0: 5 MB of 20 MB.'
  );
  assert.deepEqual(statusText({ kind: 'failed', message: 'offline' }, NOW), {
    text: 'offline',
    warning: true,
  });
  assert.equal(
    updatesSummary(info, { kind: 'idle' }, true),
    'Rovik 1.0.0 · looks for updates every 30 minutes'
  );
  assert.equal(
    updatesSummary(info, { kind: 'idle' }, false),
    'Rovik 1.0.0 · doesn’t look for updates'
  );
  assert.equal(
    updatesSummary(info, { kind: 'available', release: release('1.1.0') }, true),
    'Rovik 1.0.0 · 1.1.0 is available'
  );
  assert.equal(
    updatesSummary(null, { kind: 'installing', release: release('1.1.0') }, true),
    'Updating to 1.1.0'
  );
  assert.equal(agoText(NOW - 30_000, NOW), 'just now');
  assert.equal(agoText(NOW - 3 * 3_600_000, NOW), '3 h ago');
  assert.equal(agoText(NOW - 50 * 3_600_000, NOW), '2 days ago');
});

/** An updater against a scripted app, and what it was asked and showed. */
function harness(
  answers: Record<string, (args: any) => unknown>,
  options: {
    confirm?: boolean;
    settle?: boolean;
    installable?: boolean;
    periodic?: boolean;
    folder?: string;
    /** The cloud hosts an update leaves behind. */
    hosts?: string[];
  } = {}
) {
  dismiss(UPDATE_NOTE);
  const calls: { cmd: string; args: any }[] = [];
  const opened: string[] = [];
  const asked: string[] = [];
  let progress: ((p: Progress) => void) | null = null;
  const prefs = sharedMap('app-updates-test', { key: 'k', storage: null, backend: null });
  const backend: Record<string, (args: any) => unknown> = {
    update_info: () => ({
      version: '1.0.0',
      installable: options.installable ?? true,
      periodic: options.periodic ?? true,
    }),
    update_at_work: () => idle,
    ...answers,
  };
  const updater = createUpdater({
    invoke: async <T>(cmd: string, args?: Record<string, unknown>) => {
      calls.push({ cmd, args });
      const answer = backend[cmd];
      if (!answer) throw new Error(`no app: ${cmd}`);
      return (await answer({ ...args, progress })) as T;
    },
    onProgress: async (fn) => {
      progress = fn;
      return () => (progress = null);
    },
    prefs,
    now: () => NOW,
    openPage: (url) => opened.push(url),
    hostsBehind: async () => options.hosts ?? [],
  });
  const hooks = {
    folder: () => options.folder ?? null,
    settle: async () => options.settle ?? true,
    confirm: async ({ message }: { message: string }) => {
      asked.push(message);
      return options.confirm ?? true;
    },
  };
  const note = () => notifications().find((n) => n.id === UPDATE_NOTE);
  const click = (label: string) => {
    const action = note()?.actions?.find((a) => a.label === label);
    assert.ok(action, `no "${label}" in ${JSON.stringify(note())}`);
    if (!action.keepOpen) dismiss(UPDATE_NOTE);
    action.run();
  };
  const commands = () => calls.map((c) => c.cmd);
  return { updater, hooks, prefs, calls, commands, opened, asked, note, click };
}

/** Lets what a click started run to its end. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test('a newer release is offered once, with Update, What’s new and Later', async () => {
  const h = harness({ update_check: () => found('1.1.0') });
  await h.updater.start(h.hooks, false);
  await h.updater.poll();
  assert.equal(h.note()?.title, 'Rovik 1.1.0 is available');
  assert.equal(h.note()?.message, 'You have 1.0.0. Updating restarts Rovik.');
  assert.deepEqual(
    h.note()?.actions?.map((a) => a.label),
    ['What’s new', 'Later', 'Update']
  );
  h.click('What’s new');
  assert.deepEqual(h.opened, ['https://github.com/o/r/releases/tag/v1.1.0']);
  assert.ok(h.note(), 'What’s new leaves it open');
  // The look is every window's: when, and what it found.
  assert.equal((h.prefs.all() as any).lastCheck, NOW);
  assert.equal((h.prefs.all() as any).latest.version, '1.1.0');
  h.click('Later');
  await h.updater.check({ manual: false });
  assert.equal(h.note(), undefined, 'not offered again by this window');
  await h.updater.check({ manual: true });
  assert.equal(h.note()?.title, 'Rovik 1.1.0 is available', 'Check now says it again');
});

test('the offer says the cloud hosts are offered the new version after', async () => {
  assert.equal(hostsLine([]), '');
  assert.equal(hostsLine(['Build']), ', then offers Build the new version too');
  assert.equal(hostsLine(['a', 'b']), ', then offers your 2 cloud hosts the new version too');
  const h = harness({ update_check: () => found('1.1.0') }, { hosts: ['Build'] });
  await h.updater.start(h.hooks, false);
  await h.updater.check({ manual: false });
  assert.equal(
    h.note()?.message,
    'You have 1.0.0. Updating restarts Rovik, then offers Build the new version too.'
  );
});

test('nothing newer: quiet, unless asked; a failed look is quiet too, and waits for the next', async () => {
  const h = harness({ update_check: () => null });
  await h.updater.start(h.hooks, false);
  await h.updater.check({ manual: false });
  assert.equal(h.note(), undefined);
  assert.equal(h.updater.status().kind, 'upToDate');
  await h.updater.check({ manual: true });
  assert.equal(h.note()?.title, 'Rovik 1.0.0 is the latest version');

  const offline = harness({
    update_check: () => {
      throw new Error('Rovik’s releases couldn’t be reached');
    },
  });
  await offline.updater.start(offline.hooks, false);
  await offline.updater.check({ manual: false });
  assert.equal(offline.note(), undefined);
  assert.equal(offline.updater.status().kind, 'failed');
  assert.equal((offline.prefs.all() as any).lastCheck, NOW, 'not again until the next look');
  await offline.updater.check({ manual: true });
  assert.equal(offline.note()?.title, 'Couldn’t check for updates');
});

test('a development build looks only when asked', async () => {
  const h = harness(
    { update_check: () => found('1.1.0', { installable: false }) },
    { installable: false, periodic: false }
  );
  await h.updater.start(h.hooks, false);
  await h.updater.poll();
  h.updater.setCheckForUpdates(true);
  await settle();
  assert.ok(!h.commands().includes('update_check'));
  assert.equal(
    updatesSummary(h.updater.info(), h.updater.status(), true),
    'Rovik 1.0.0 · doesn’t look for updates'
  );
  await h.updater.check({ manual: true });
  assert.equal(h.note()?.title, 'Rovik 1.1.0 is available');
});

test('turned off, a window doesn’t look, and on again it looks at once', async () => {
  const h = harness({ update_check: () => found('1.1.0') });
  await h.updater.start(h.hooks, false);
  h.updater.setCheckForUpdates(false);
  assert.equal(h.updater.enabled(), false);
  await h.updater.poll();
  assert.ok(!h.commands().includes('update_check'));
  h.updater.setCheckForUpdates(true);
  await settle();
  assert.ok(h.commands().includes('update_check'));
  assert.equal((h.prefs.all() as any).checkForUpdates, undefined, 'on is the default, not kept');
});

test('Update downloads, installs, and starts the window again on its project', async () => {
  const h = harness(
    {
      update_check: () => found('1.1.0'),
      update_download: ({ version, progress }) => {
        assert.equal(version, '1.1.0');
        progress({ version, downloaded: 10 << 20, total: 40 << 20 });
      },
      update_install: ({ version }) => assert.equal(version, '1.1.0'),
      update_restart: () => null,
    },
    { folder: '/home/ann/app' }
  );
  await h.updater.start(h.hooks, false);
  await h.updater.poll();
  h.click('Update');
  await settle();
  assert.deepEqual(h.commands(), [
    'update_info',
    'update_check',
    'update_download',
    'update_at_work',
    'update_install',
    'update_restart',
  ]);
  assert.deepEqual(h.calls[h.calls.length - 1].args, { folder: '/home/ann/app' });
  assert.deepEqual(h.asked, [], 'nothing at stake: nothing asked');
  // The other windows start again on it, without a download of their own.
  assert.equal((h.prefs.all() as any).installed, '1.1.0');
  assert.deepEqual(h.updater.status(), { kind: 'installed', version: '1.1.0' });
});

test('chats at work are asked about; "Not now" keeps the download for later', async () => {
  let installs = 0;
  const h = harness(
    {
      update_check: () => found('1.1.0'),
      update_download: () => null,
      update_at_work: () => ({ ...idle, here: 2 }),
      update_install: () => void installs++,
      update_restart: () => null,
    },
    { confirm: false }
  );
  await h.updater.start(h.hooks, false);
  await h.updater.check({ manual: false });
  h.click('Update');
  await settle();
  assert.equal(installs, 0);
  assert.match(h.asked[0], /2 chats in this window are at work/);
  assert.equal(h.note()?.title, 'Rovik 1.1.0 is downloaded');
  assert.equal(h.updater.status().kind, 'ready');
  assert.deepEqual(
    h.note()?.actions?.map((a) => a.label),
    ['Later', 'Restart and update']
  );
});

test('unsaved tabs the user keeps open stop the restart too', async () => {
  const h = harness(
    {
      update_check: () => found('1.1.0'),
      update_download: () => null,
      update_install: () => assert.fail('installed with the tabs unsettled'),
    },
    { settle: false }
  );
  await h.updater.start(h.hooks, false);
  await h.updater.check({ manual: false });
  h.click('Update');
  await settle();
  assert.equal(h.note()?.title, 'Rovik 1.1.0 is downloaded');
});

test('a failed install says why, with Try again and a download by hand', async () => {
  const h = harness({
    update_check: () => found('1.1.0'),
    update_download: () => null,
    update_install: () => {
      throw 'Installing it takes an administrator’s password, and none was given.';
    },
  });
  await h.updater.start(h.hooks, false);
  await h.updater.check({ manual: false });
  h.click('Update');
  await settle();
  assert.equal(h.note()?.kind, 'error');
  assert.equal(h.note()?.title, 'Rovik 1.1.0 couldn’t be installed');
  assert.match(h.note()?.message ?? '', /administrator’s password/);
  assert.deepEqual(
    h.note()?.actions?.map((a) => a.label),
    ['Download it', 'Try again']
  );
  assert.equal(h.updater.status().kind, 'failed');
});

test('a copy not installed from a release is offered the download', async () => {
  const h = harness(
    { update_check: () => found('1.1.0', { installable: false }) },
    { installable: false }
  );
  await h.updater.start(h.hooks, false);
  await h.updater.check({ manual: false });
  assert.match(h.note()?.message ?? '', /wasn’t installed from a release/);
  assert.deepEqual(
    h.note()?.actions?.map((a) => a.label),
    ['Later', 'Download']
  );
  h.click('Download');
  assert.deepEqual(h.opened, ['https://github.com/o/r/releases/tag/v1.1.0']);
});

test('a version another window installed is offered as a restart, not a download', async () => {
  const h = harness({ update_check: () => found('1.1.0'), update_restart: () => null });
  await h.prefs.set({ installed: '1.1.0' });
  await h.updater.start(h.hooks, false);
  await h.updater.poll();
  assert.ok(!h.commands().includes('update_check'));
  assert.equal(h.note()?.title, 'Rovik 1.1.0 is installed');
  assert.match(h.note()?.message ?? '', /still runs 1\.0\.0/);
  h.click('Restart');
  await settle();
  assert.deepEqual(h.commands().slice(-2), ['update_at_work', 'update_restart']);
  // What another window's look found is offered as it is, without a look.
  const other = harness({ update_check: () => assert.fail('looked again') });
  await other.prefs.set({ lastCheck: NOW - 60_000, latest: release('1.2.0') });
  await other.updater.start(other.hooks, false);
  await other.updater.poll();
  assert.equal(other.note()?.title, 'Rovik 1.2.0 is available');
});
