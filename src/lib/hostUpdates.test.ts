// hostUpdates.ts tests (node:test): once Rovik runs a new version, the cloud
// hosts with an older one are looked at, offered the new version in one
// notification, and updated, those the user administers, on "Update
// hosts". The app is answered here; the windows' shared settings are a map
// of the test's.
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

const { HOSTS_NOTE, behindAs, createHostUpdater, hostsBehindIn, hostsNotice, namesOf } =
  await import('./hostUpdates');
const { notifications, dismiss } = await import('./notifications');
const { sharedMap } = await import('./sharedState');
type HostRow = import('./hosts').HostRow;
type HostDetails = import('./hosts').HostDetails;
type HostsList = import('./hosts').HostsList;

function row(id: string, version: string | null, state?: HostRow['state']): HostRow {
  return {
    id,
    name: id,
    host: `${id}.example.com`,
    port: 22,
    user: 'ann',
    auth: 'key',
    approvalPolicy: 'waitForMe',
    deploy: version === null ? { state: 'notDeployed' } : { state: 'deployed', version },
    state,
    chats: 0,
    passwordSaved: false,
    passphraseSaved: false,
  };
}

function details(version: string, admin: boolean, admins: string[] = []): HostDetails {
  return {
    info: {
      version,
      protocol: 1,
      minProtocol: 1,
      os: 'linux',
      arch: 'x86_64',
      user: 'ann',
      virtualFiles: true,
      chrome: undefined,
      userNamespaces: true,
      install: { admins },
      otherDaemons: 0,
    },
    admin,
    upgrade: true,
    appVersion: '2.0.0',
    cleanUp: true,
  };
}

const list = (hosts: HostRow[]): HostsList => ({ hosts, store: { kind: 'keychain' } });

test('the deployed hosts with an older Rovik are the ones behind', () => {
  const hosts = list([
    row('old', '1.9.0', 'ready'),
    row('same', '2.0.0', 'ready'),
    row('newer', '2.0.1', 'version-mismatch'),
    row('none', null),
  ]);
  assert.deepEqual(
    hostsBehindIn(hosts, '2.0.0').map((h) => h.id),
    ['old']
  );
  // Its daemon says what it runs now: one upgraded meanwhile isn't behind.
  assert.equal(
    behindAs(row('old', '1.9.0', 'ready'), { ...details('2.0.0', true), upgrade: false }),
    null
  );
  assert.deepEqual(behindAs(row('old', '1.9.0', 'ready'), details('1.9.5', false, ['bob'])), {
    id: 'old',
    name: 'old',
    version: '1.9.5',
    admin: false,
    admins: ['bob'],
  });
  // Not connected: as last known, who may update it not known.
  assert.deepEqual(behindAs(row('away', '1.9.0', 'offline'), null), {
    id: 'away',
    name: 'away',
    version: '1.9.0',
  });
  assert.equal(namesOf([{ name: 'a' }, { name: 'b' }, { name: 'c' }]), 'a, b and c');
});

test('one notice says which hosts can be updated, whom to ask for the others, and which aren’t there', () => {
  assert.equal(hostsNotice([], '2.0.0'), null);
  const one = hostsNotice([{ id: 'b', name: 'Build', version: '1.9.0', admin: true }], '2.0.0')!;
  assert.equal(one.title, 'Build runs an older Rovik (1.9.0)');
  assert.equal(
    one.message,
    'Build can be updated to 2.0.0, for everyone there: runs pause while Rovik restarts, then go on.'
  );
  assert.deepEqual(
    one.canUpdate.map((h) => h.id),
    ['b']
  );
  const many = hostsNotice(
    [
      { id: 'b', name: 'Build', version: '1.9.0', admin: true },
      { id: 's', name: 'Shared', version: '1.8.0', admin: false, admins: ['bob', 'eve'] },
      { id: 'a', name: 'Away', version: '1.9.0' },
    ],
    '2.0.0'
  )!;
  assert.equal(many.title, '3 cloud hosts run an older Rovik');
  assert.deepEqual(many.message.split('\n'), [
    'Build can be updated to 2.0.0, for everyone there: runs pause while Rovik restarts, then go on.',
    'Shared runs 1.8.0: ask bob or eve to update it.',
    'Away isn’t connected: update it in Settings → Cloud hosts once it’s back.',
  ]);
});

/** A host updater against a scripted app. */
function harness(hosts: () => HostRow[], answers: Record<string, HostDetails | Error> = {}) {
  dismiss(HOSTS_NOTE);
  const upgraded: string[] = [];
  const prefs = sharedMap('host-updates-test', { key: 'k', storage: null, backend: null });
  let clock = 0;
  const updater = createHostUpdater({
    appInfo: async () => ({ version: '2.0.0', installable: true, periodic: true }),
    listHosts: async () => list(hosts()),
    hostDetails: async (id) => {
      const answer = answers[id];
      if (!answer || answer instanceof Error) throw answer ?? new Error('not connected');
      return answer;
    },
    upgradeHost: async (id) => {
      if (id === 'fails') throw { kind: 'askAdmins', admins: ['eve'], message: 'Ask eve.' };
      upgraded.push(id);
    },
    prefs,
    now: () => clock,
    sleep: async (ms) => {
      clock += ms;
    },
  });
  const note = () => notifications().find((n) => n.id === HOSTS_NOTE);
  const click = async (label: string) => {
    const action = note()?.actions?.find((a) => a.label === label);
    assert.ok(action, `no "${label}" in ${JSON.stringify(note())}`);
    dismiss(HOSTS_NOTE);
    action.run();
    await new Promise((resolve) => setTimeout(resolve, 0));
  };
  return { updater, prefs, upgraded, note, click, elapsed: () => clock };
}

test('the hosts are looked at once they’ve connected, and offered the version once', async () => {
  let looks = 0;
  const h = harness(
    () => {
      looks++;
      // Still connecting for the first two looks.
      return [
        row('build', '1.9.0', looks < 3 ? 'connecting' : 'ready'),
        row('same', '2.0.0', 'ready'),
      ];
    },
    { build: details('1.9.0', true) }
  );
  await h.updater.start();
  assert.equal(looks, 3);
  assert.equal(h.note()?.title, 'build runs an older Rovik (1.9.0)');
  assert.deepEqual(
    h.note()?.actions?.map((a) => a.label),
    ['Show hosts', 'Later', 'Update host']
  );
  assert.equal((h.prefs.all() as any).lookedFor, '2.0.0');
  // Another start of this version (another window, say) says nothing.
  dismiss(HOSTS_NOTE);
  const again = createHostUpdater({
    appInfo: async () => ({ version: '2.0.0', installable: true, periodic: true }),
    listHosts: async () => assert.fail('looked again'),
    hostDetails: async () => assert.fail('looked again'),
    upgradeHost: async () => {},
    prefs: h.prefs,
    now: () => 0,
    sleep: async () => {},
  });
  await again.start();
  assert.equal(h.note(), undefined);
});

test('hosts that never connect are told of after a while; none behind, nothing is said', async () => {
  const h = harness(() => [row('away', '1.9.0', 'connecting')]);
  await h.updater.start();
  assert.ok(h.elapsed() >= 90_000, 'waited for it');
  assert.match(h.note()?.message ?? '', /away isn’t connected/);
  assert.deepEqual(
    h.note()?.actions?.map((a) => a.label),
    ['Show hosts', 'Later'],
    'nothing to update from here'
  );
  const none = harness(() => [row('same', '2.0.0', 'ready')]);
  await none.updater.start();
  assert.equal(none.note(), undefined);
  assert.equal((none.prefs.all() as any).lookedFor, '2.0.0');
});

test('"Update hosts" updates the hosts one at a time and says which couldn’t be', async () => {
  const h = harness(() => [row('build', '1.9.0', 'ready'), row('fails', '1.9.0', 'ready')], {
    build: details('1.9.0', true),
    fails: details('1.9.0', true),
  });
  await h.updater.start();
  assert.equal(h.note()?.title, '2 cloud hosts run an older Rovik');
  await h.click('Update hosts');
  assert.deepEqual(h.upgraded, ['build']);
  assert.equal(h.note()?.kind, 'warning');
  assert.equal(h.note()?.title, 'fails couldn’t be updated');
  assert.deepEqual(h.note()?.message?.split('\n'), ['build: updated.', 'fails: Ask eve.']);

  const ok = harness(() => [row('build', '1.9.0', 'ready')], { build: details('1.9.0', true) });
  await ok.updater.start();
  await ok.click('Update host');
  assert.equal(ok.note()?.kind, 'success');
  assert.equal(ok.note()?.title, 'build runs Rovik 2.0.0');
});
