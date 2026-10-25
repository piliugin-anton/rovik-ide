// connectorUpdates.ts tests (node:test): a newer version of a connector's
// program, found by the check as the app starts, is offered in one
// notification, once per version whichever window finds it, and updated on
// "Update". The program's updater and the check after it are the test's;
// the windows' shared settings are a file of the test's.
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

const { CONNECTOR_UPDATES_NOTE, createConnectorUpdater, outdated } =
  await import('./connectorUpdates');
const { authStateOf, newConnector } = await import('./connectors');
const { notifications, dismiss } = await import('./notifications');
const { sharedMap } = await import('./sharedState');
type Provider = import('../types').Provider;
type CheckedState = import('./connectors').CheckedState;
type Entries = import('./sharedState').Entries;
type SharedBackend = import('./sharedState').SharedBackend;

const CLAUDE = '/home/me/.local/bin/claude';

const connector = (id: string, name = 'Claude Code'): Provider => ({
  ...newConnector('claude-code'),
  id,
  name,
});

/** What a check finds: `version` of the program at `program`, and `update` when a newer one is out. */
const found = (version: string, update: string | null = null, program = CLAUDE): CheckedState =>
  authStateOf({ probe: { program, version, models: [], auth: { loggedIn: true }, update } });

/** The windows' shared file. */
function sharedFile(): SharedBackend {
  let saved: Entries | null = null;
  return {
    read: async () => saved,
    start: async (_, entries) => (saved ??= { ...entries }),
    merge: async (_, patch) => (saved = { ...saved, ...patch }),
  };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

async function until(check: () => boolean) {
  for (let i = 0; i < 100 && !check(); i++) await settle();
  assert.ok(check(), 'timed out');
}

/**
 * A window's updater: its program updates as `runUpdate` says, and the
 * check after it finds what `after` says.
 */
function harness(
  options: {
    file?: SharedBackend;
    runUpdate?: (c: Provider) => Promise<void>;
    after?: (c: Provider) => CheckedState;
  } = {}
) {
  dismiss(CONNECTOR_UPDATES_NOTE);
  let opened = 0;
  const updater = createConnectorUpdater({
    prefs: sharedMap('connector-updates', {
      key: 'k',
      storage: null,
      backend: options.file ?? sharedFile(),
    }),
    runUpdate: options.runUpdate ?? (async () => {}),
    check: async (c) => (options.after ?? (() => found('2.2.0 (Claude Code)')))(c),
    openConnectors: () => opened++,
  });
  const note = () => notifications().find((n) => n.id === CONNECTOR_UPDATES_NOTE);
  const click = async (label: string) => {
    const action = note()?.actions?.find((a) => a.label === label);
    assert.ok(action, `no "${label}" in ${JSON.stringify(note())}`);
    dismiss(CONNECTOR_UPDATES_NOTE);
    action.run();
    await settle();
  };
  return { updater, note, click, opened: () => opened };
}

test('a program is offered once per newer version, with the first connector that runs it', () => {
  const work = connector('c2', 'Claude Code (work)');
  const other = connector('c3', 'Other');
  const providers = [connector('c1'), work, other];
  const atStart = {
    c1: found('2.1.310 (Claude Code)', '2.2.0'),
    c2: found('2.1.310 (Claude Code)', '2.2.0'),
    c3: found('1.0.0', null, '/usr/bin/other'),
  };
  assert.deepEqual(
    outdated(providers, atStart, {}).map((o) => [o.connector.id, o.program, o.update]),
    [['c1', CLAUDE, '2.2.0']]
  );
  // Offered already: not again, until a newer version still is out.
  assert.deepEqual(outdated(providers, atStart, { [CLAUDE]: '2.2.0' }), []);
  const newer = { ...atStart, c1: found('2.1.310 (Claude Code)', '2.3.0') };
  assert.equal(outdated(providers, newer, { [CLAUDE]: '2.2.0' })[0]?.update, '2.3.0');
  // A connector whose check failed has nothing to update.
  assert.deepEqual(outdated(providers, { c1: authStateOf({ error: 'timed out' }) }, {}), []);
});

test('a newer version found as the app starts is offered once, whichever window finds it', async () => {
  const file = sharedFile();
  const providers = [connector('c1')];
  const atStart = { c1: found('2.1.310 (Claude Code)', '2.2.0') };
  const first = harness({ file });
  await first.updater.offer(providers, atStart);
  assert.equal(first.note()?.kind, 'info');
  assert.equal(first.note()?.title, 'Claude Code 2.2.0 is available');
  assert.equal(first.note()?.message, 'You have 2.1.310 (Claude Code).');
  assert.deepEqual(
    first.note()?.actions?.map((a) => a.label),
    ['Later', 'Update']
  );
  await first.click('Later');

  // Another window, or the next start: the same version isn't offered again.
  const second = harness({ file });
  await second.updater.offer(providers, atStart);
  assert.equal(second.note(), undefined);
  // A newer one is.
  await second.updater.offer(providers, { c1: found('2.1.310 (Claude Code)', '2.3.0') });
  assert.equal(second.note()?.title, 'Claude Code 2.3.0 is available');
  dismiss(CONNECTOR_UPDATES_NOTE);
});

test('Update runs the program’s updater, checks it again, and says it’s updated', async () => {
  const ran: string[] = [];
  const h = harness({
    runUpdate: async (c) => {
      ran.push(c.id);
      // While it runs, the notification says so.
      assert.equal(h.note()?.title, 'Updating Claude Code…');
      assert.equal(h.note()?.progress, 'busy');
    },
  });
  await h.updater.offer([connector('c1')], { c1: found('2.1.310 (Claude Code)', '2.2.0') });
  await h.click('Update');
  await until(() => h.note()?.kind === 'success');
  assert.deepEqual(ran, ['c1']);
  assert.equal(h.note()?.title, 'Claude Code updated');
  assert.equal(h.note()?.message, 'Now 2.2.0 (Claude Code).');
  dismiss(CONNECTOR_UPDATES_NOTE);
});

test('an update that fails, or leaves it behind, says why, with a way to Settings', async () => {
  const providers = [connector('c1'), connector('c2', 'Other')];
  const atStart = {
    c1: found('2.1.310 (Claude Code)', '2.2.0'),
    c2: found('1.0.0', '1.1.0', '/usr/bin/other'),
  };
  const h = harness({
    runUpdate: async (c) => {
      if (c.id === 'c2') throw new Error('`other update` needs the administrator.');
    },
  });
  await h.updater.offer(providers, atStart);
  assert.equal(h.note()?.title, 'Newer versions of 2 connectors are available');
  assert.equal(
    h.note()?.message,
    'Claude Code: 2.1.310 (Claude Code) → 2.2.0\nOther: 1.0.0 → 1.1.0'
  );
  await h.click('Update');
  await until(() => h.note()?.kind === 'warning');
  assert.equal(h.note()?.title, 'Other couldn’t be updated');
  assert.equal(
    h.note()?.message,
    'Claude Code: updated to 2.2.0 (Claude Code).\nOther: `other update` needs the administrator.'
  );
  await h.click('Open Connectors');
  assert.equal(h.opened(), 1);

  // The updater ran, but the check after it still finds it behind, or fails.
  for (const [after, why] of [
    [
      found('2.1.310 (Claude Code)', '2.2.0'),
      'Still 2.1.310 (Claude Code): it didn’t update to 2.2.0.',
    ],
    [authStateOf({ error: 'claude --version timed out' }), 'claude --version timed out'],
  ] as const) {
    const again = harness({ after: () => after });
    await again.updater.offer([connector('c1')], { c1: found('2.1.310 (Claude Code)', '2.2.0') });
    await again.click('Update');
    await until(() => again.note()?.kind === 'warning');
    assert.equal(again.note()?.title, 'Claude Code couldn’t be updated');
    assert.equal(again.note()?.message, `Claude Code: ${why}`);
  }
  dismiss(CONNECTOR_UPDATES_NOTE);
});
