// cloud.ts tests (node:test): which events refresh a chat's isolation bar
// and carry its move's steps; what the chats show of where they are (U02);
// and the store `cloud-event` keeps, the chats' places with it. The app is
// answered here: `listen` hands over the event handler.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  barChanged,
  cloud,
  hostName,
  initCloud,
  moveEnded,
  moveStep,
  moveStepLabel,
  placeLabel,
  readOnlyReason,
  refreshHosts,
  startMove,
  taskEffort,
  worksOnItsOwn,
  type CloudEvent,
  type HostSummary,
  isWsl,
} from './cloud';
import { chatStore } from './chatStore';
import { wslSummary, type WslStatus } from './wsl';
import type { DialogSummary } from '../types';

const callbacks = new Map<number, (event: unknown) => void>();
let listener: number | null = null;
/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    transformCallback: (callback: (event: unknown) => void) => {
      const id = callbacks.size + 1;
      callbacks.set(id, callback);
      return id;
    },
    invoke: async (cmd: string, args: any) => {
      if (cmd === 'plugin:event|listen') {
        if (args.event === 'cloud-event') listener = args.handler;
        return 1;
      }
      calls.push({ cmd, args });
      const answer = backend[cmd];
      if (!answer) return null;
      return answer(args);
    },
  },
};

/** The app sends a `cloud-event`. */
function emit(payload: unknown) {
  assert.ok(listener !== null, 'listening');
  callbacks.get(listener!)!({ event: 'cloud-event', id: 1, payload });
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

const changed: CloudEvent = { type: 'changed', dialogId: 'chat-1' };

test("a remote chat's changes looking different refresh its own bar", () => {
  assert.equal(barChanged(changed, 'chat-1'), true);
  assert.equal(barChanged(changed, 'chat-2'), false);
  assert.equal(barChanged(changed, null), false);
});

test('transfers leave the bar alone', () => {
  const pushing: CloudEvent = { type: 'pushing', dialogId: 'chat-1', project: 'p', sent: 10 };
  const uploading: CloudEvent = {
    type: 'uploading',
    dialogId: 'chat-1',
    messageId: 'm',
    sent: 1,
    total: 2,
  };
  assert.equal(barChanged(pushing, 'chat-1'), false);
  assert.equal(barChanged(uploading, 'chat-1'), false);
});

test("a move's steps, there or back, are its chat's alone, and leave the bar alone", () => {
  const moving: CloudEvent = { type: 'moving', dialogId: 'chat-1', step: 'importing' };
  assert.equal(moveStep(moving, 'chat-1'), 'importing');
  assert.equal(moveStep(moving, 'chat-2'), null);
  const back: CloudEvent = { type: 'movingBack', dialogId: 'chat-1', step: 'pulling' };
  assert.equal(moveStep(back, 'chat-1'), 'pulling');
  assert.equal(barChanged(back, 'chat-1'), false);
  assert.equal(moveStep(changed, 'chat-1'), null);
  assert.equal(barChanged(moving, 'chat-1'), false);
});

const HOSTS: HostSummary[] = [
  { id: 'h1', name: 'Build', deployed: true, state: 'ready' },
  { id: 'h2', name: 'Spare', deployed: true, state: 'offline' },
  { id: 'h3', name: 'New', deployed: false },
];

test('the chip: this computer, a host by name and whether it’s there, or the move going', () => {
  assert.deepEqual(placeLabel(undefined, HOSTS), {
    label: 'This computer',
    tone: 'muted',
    title: 'This chat runs on this computer.',
  });
  const on = (host: string) => placeLabel({ kind: 'host', host }, HOSTS);
  assert.equal(on('h1').label, 'Build');
  assert.equal(on('h1').tone, 'ok');
  assert.equal(on('h2').label, 'Spare (offline)');
  assert.equal(on('h2').tone, 'error');
  assert.match(on('h2').title, /can’t be reached now/);
  assert.equal(on('gone').label, 'gone', 'a host this app doesn’t know goes by its id');
  assert.equal(placeLabel({ kind: 'moving', host: 'h1' }, HOSTS).label, 'Moving to Build…');
  assert.equal(
    placeLabel({ kind: 'movingBack', host: 'h2' }, HOSTS).label,
    'Moving back from Spare…'
  );
  assert.equal(hostName('h3', HOSTS), 'New');
});

test('a chat in WSL is this computer’s: no host name, and its troubles point at its page', () => {
  const hosts: HostSummary[] = [...HOSTS, { id: 'wsl', name: 'WSL', deployed: true, wsl: true }];
  assert.equal(isWsl('wsl', hosts), true);
  assert.equal(isWsl('h1', hosts), false);
  assert.equal(placeLabel({ kind: 'host', host: 'wsl' }, hosts).label, 'This computer (WSL)');
  assert.equal(
    placeLabel({ kind: 'moving', host: 'wsl' }, hosts).label,
    'Moving to This computer (WSL)…'
  );
  const inState = (state: HostSummary['state']): HostSummary[] => [
    { id: 'wsl', name: 'WSL', deployed: true, wsl: true, state },
  ];
  assert.equal(readOnlyReason({ kind: 'host', host: 'wsl' }, inState('ready')), null);
  assert.match(
    readOnlyReason({ kind: 'host', host: 'wsl' }, inState('offline'))!,
    /^WSL isn’t running: .*Settings → This computer → WSL/
  );
  assert.match(
    readOnlyReason({ kind: 'host', host: 'wsl' }, inState('refused'))!,
    /^WSL couldn’t start: .*Settings → This computer → WSL says what’s wrong/
  );
  assert.match(
    readOnlyReason({ kind: 'host', host: 'wsl' }, inState('version-mismatch'))!,
    /Rovik in WSL is newer than this app/
  );
  assert.match(
    readOnlyReason({ kind: 'moving', host: 'wsl' }, hosts)!,
    /moving to WSL, where it runs/
  );
  // The Settings menu's line.
  const status = (over: Partial<WslStatus>): WslStatus => ({
    available: true,
    unsupported: false,
    distros: [],
    deployed: false,
    outdated: false,
    appVersion: '1.0.0',
    ...over,
  });
  assert.equal(wslSummary(status({ available: false })).text, 'WSL isn’t installed');
  assert.equal(
    wslSummary(status({ available: false, unsupported: true })).text,
    'WSL2 can’t run on this Windows'
  );
  assert.equal(
    wslSummary(status({ deployed: true, state: 'ready', distro: 'Ubuntu' })).text,
    'Ubuntu, connected'
  );
  assert.equal(
    wslSummary(status({ deployed: true, state: 'refused', distro: 'Ubuntu' })).warning,
    true
  );
});

test('a chat on a host that works on its own, and only there, goes in Autopilot (H02)', () => {
  const hosts: HostSummary[] = [
    ...HOSTS,
    { id: 'own', name: 'Own', deployed: true, state: 'ready', autonomous: true },
  ];
  assert.equal(worksOnItsOwn({ kind: 'host', host: 'own' }, hosts), true);
  assert.equal(worksOnItsOwn({ kind: 'host', host: 'h1' }, hosts), false);
  assert.equal(worksOnItsOwn({ kind: 'moving', host: 'own' }, hosts), false, 'not there yet');
  assert.equal(worksOnItsOwn(undefined, hosts), false);
  assert.equal(worksOnItsOwn({ kind: 'host', host: 'gone' }, hosts), false);
});

test('a task takes its host’s effort when the host works on its own and sets one (H01)', () => {
  const hosts: HostSummary[] = [
    ...HOSTS,
    { id: 'own', name: 'Own', deployed: true, autonomous: true, effort: 'thorough' },
    { id: 'plain', name: 'Plain', deployed: true, autonomous: true },
  ];
  assert.equal(taskEffort({ kind: 'host', host: 'own' }, hosts, 'balanced'), 'thorough');
  assert.equal(taskEffort({ kind: 'host', host: 'plain' }, hosts, 'balanced'), 'balanced');
  assert.equal(taskEffort({ kind: 'host', host: 'h1' }, hosts, 'thorough'), 'thorough');
  assert.equal(taskEffort(undefined, hosts, 'thorough'), 'thorough', 'this computer’s chat');
  assert.equal(taskEffort({ kind: 'moving', host: 'own' }, hosts, 'balanced'), 'balanced');
});

test('a moving chat is read-only, there and back; one on a host isn’t', () => {
  assert.equal(readOnlyReason(undefined, HOSTS), null);
  assert.equal(readOnlyReason({ kind: 'host', host: 'h1' }, HOSTS), null);
  assert.match(
    readOnlyReason({ kind: 'moving', host: 'h1' }, HOSTS)!,
    /moving to Build: it’s read-only until the move ends/
  );
  assert.match(
    readOnlyReason({ kind: 'movingBack', host: 'h1' }, HOSTS)!,
    /moving back from Build: it’s read-only/
  );
});

test('a move’s steps in words, either way', () => {
  assert.equal(moveStepLabel({ direction: 'there', step: 'pushing' }), 'Sending the project…');
  assert.equal(moveStepLabel({ direction: 'back', step: 'moved' }), 'Back on this computer.');
  assert.equal(moveStepLabel({ direction: 'back', step: 'later' }), 'later', 'one this app lacks');
  assert.equal(moveEnded(undefined), true);
  assert.equal(moveEnded({ direction: 'there', step: 'importing' }), false);
  assert.equal(moveEnded({ direction: 'there', step: 'cancelled' }), true);
});

test('the store follows the hosts’ connections and a move’s steps, and the chat’s place', async () => {
  calls = [];
  backend = {
    hosts_summaries: () => HOSTS,
    chat_get_dialog: ({ dialogId }: { dialogId: string }) => ({ id: dialogId, place: placed }),
  };
  let placed: DialogSummary['place'] = { kind: 'moving', host: 'h1' };
  await initCloud();
  assert.deepEqual(
    cloud.hosts.map((h) => h.name),
    ['Build', 'Spare', 'New']
  );
  emit({ type: 'host', host: 'h2', state: 'ready' });
  assert.equal(cloud.hosts[1].state, 'ready');

  const chat = chatStore.createDialog('Moving chat', null);
  emit({ type: 'moving', dialogId: chat.id, step: 'pausing' });
  await flush();
  assert.deepEqual(sent('chat_get_dialog'), [{ dialogId: chat.id }], 'where it is, as it starts');
  assert.deepEqual(chatStore.state.active?.place, { kind: 'moving', host: 'h1' });
  emit({ type: 'moving', dialogId: chat.id, step: 'pushing' });
  emit({ type: 'pushing', dialogId: chat.id, project: 'p', sent: 4096 });
  assert.equal(cloud.moves[chat.id].pushed, 4096);
  placed = { kind: 'host', host: 'h1' };
  emit({ type: 'moving', dialogId: chat.id, step: 'moved' });
  await flush();
  assert.equal(moveEnded(cloud.moves[chat.id]), true);
  assert.deepEqual(chatStore.state.active?.place, { kind: 'host', host: 'h1' });
  // A push after the move, before a message, isn't the move's.
  emit({ type: 'pushing', dialogId: chat.id, project: 'p', sent: 9999 });
  assert.equal(cloud.moves[chat.id].pushed, 4096);
});

test('starting a move: read-only at once, where it went once it ends, or why not', async () => {
  calls = [];
  let placed: DialogSummary['place'] = undefined;
  backend = {
    hosts_summaries: () => HOSTS,
    chat_get_dialog: ({ dialogId }: { dialogId: string }) => ({ id: dialogId, place: placed }),
    cloud_move_chat: async () => {
      assert.deepEqual(chatStore.state.active?.place, { kind: 'moving', host: 'h1' });
      placed = { kind: 'host', host: 'h1' };
    },
  };
  await refreshHosts();
  const chat = chatStore.createDialog('Chat to move', null);
  await startMove(chat.id, 'there', 'h1', null);
  assert.deepEqual(sent('cloud_move_chat'), [{ dialogId: chat.id, host: 'h1', start: null }]);
  assert.deepEqual(chatStore.state.active?.place, { kind: 'host', host: 'h1' });

  // Refused before it began: no step says why, so the store does.
  backend.cloud_move_back = () => {
    throw 'Build isn’t connected.';
  };
  await assert.rejects(startMove(chat.id, 'back', 'h1', null));
  assert.deepEqual(cloud.moves[chat.id], {
    direction: 'back',
    host: 'h1',
    step: 'failed',
    message: 'Build isn’t connected.',
    pushed: undefined,
  });
  assert.deepEqual(chatStore.state.active?.place, { kind: 'host', host: 'h1' }, 'still there');
});

test('a chat on an offline host is read-only, and its input says why (6.10)', () => {
  // Its own hosts: the store took the ones above, and changed them.
  const away: HostSummary[] = [{ id: 'h2', name: 'Spare', deployed: true, state: 'offline' }];
  assert.equal(
    readOnlyReason({ kind: 'host', host: 'h2' }, away),
    'Spare is offline: this chat is read-only until it’s back.'
  );
  const old: HostSummary[] = [
    { id: 'h1', name: 'Build', deployed: true, state: 'version-mismatch' },
  ];
  assert.match(readOnlyReason({ kind: 'host', host: 'h1' }, old)!, /newer versions of this app/);
  // A host that turned this computer away isn't tried again: the reason is
  // on the hosts page.
  const refused: HostSummary[] = [{ id: 'h1', name: 'Build', deployed: true, state: 'refused' }];
  assert.match(
    readOnlyReason({ kind: 'host', host: 'h1' }, refused)!,
    /turned this computer away: .*Settings → Cloud hosts/
  );
  const connecting: HostSummary[] = [
    { id: 'h1', name: 'Build', deployed: true, state: 'connecting' },
  ];
  assert.equal(readOnlyReason({ kind: 'host', host: 'h1' }, connecting), null);
});

test('a host has caught up once it says so, and has to again after it was away (K23)', async () => {
  await initCloud();
  emit({ type: 'host', host: 'h1', state: 'ready' });
  assert.equal(cloud.caughtUp.h1, undefined, 'connected isn’t caught up');
  emit({ type: 'caughtUp', host: 'h1' });
  assert.equal(cloud.caughtUp.h1, true);
  emit({ type: 'host', host: 'h1', state: 'offline' });
  assert.equal(cloud.caughtUp.h1, undefined);
  emit({ type: 'host', host: 'h1', state: 'ready' });
  emit({ type: 'caughtUp', host: 'h1' });
  assert.equal(cloud.caughtUp.h1, true);
});
