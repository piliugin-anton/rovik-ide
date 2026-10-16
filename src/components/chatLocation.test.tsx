// Component tests: where the open chat is and its menu (ChatLocation), the
// move dialog's text under different settings and the move as it goes
// (MoveChatDialog), and a moving chat's messages offering nothing to do
// (MessageList, read-only). The app is answered here, command by command.
// Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createStore } from 'solid-js/store';
import ChatLocation from './ChatLocation';
import MoveChatDialog, { KEYS_NOTICE } from './MoveChatDialog';
import MessageList from './MessageList';
import { appSettingsPage, closeAppSettings } from '../lib/appSettings';
import { chatApi } from '../lib/chatApi';
import { initCloud, refreshHosts, type HostSummary, type MovePreflight } from '../lib/cloud';
import type { StartRequest } from '../lib/agentRuns';
import type { ChatMessage, ChatPlace, DialogSummary } from '../types';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = callbacks.size + 1;
    callbacks.set(id, callback);
    return id;
  },
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') {
      listeners.push({ event: args.event, handler: args.handler });
      return args.handler;
    }
    if (cmd === 'plugin:event|unlisten') return null;
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

/** The app sends `cloud-event`. */
function emit(payload: unknown) {
  for (const l of listeners.filter((l) => l.event === 'cloud-event')) {
    callbacks.get(l.handler)!({ event: 'cloud-event', id: 1, payload });
  }
}

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};
const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

const HOSTS: HostSummary[] = [
  { id: 'h1', name: 'Build', deployed: true, state: 'ready' },
  { id: 'h2', name: 'Spare', deployed: true, state: 'offline' },
  { id: 'h3', name: 'New', deployed: false },
];

/** What rendered last: gone when the next one renders, its dialogs with it. */
let disposeLast = () => {};
// The list's clock would keep the test running.
after(() => disposeLast());

/** The app's hosts are `hosts`, and the app answers as `answers` says. */
async function app(hosts: HostSummary[], answers: typeof backend = {}) {
  disposeLast();
  calls = [];
  backend = { hosts_summaries: () => hosts, ...answers };
  await initCloud();
  await refreshHosts();
}

function chat(place?: ChatPlace): DialogSummary {
  return { id: 'c1', title: 'Fix the build', createdAt: 1, updatedAt: 1, messageCount: 2, place };
}

const START = { dialogId: 'c1' } as StartRequest;

/** Renders the chip for `dialog`; `resynced` hears "Re-sync this chat". */
function chip(dialog: DialogSummary, resynced: string[] = []) {
  disposeLast();
  const root = container();
  disposeLast = render(
    () => (
      <ChatLocation
        dialog={dialog}
        startFor={async () => START}
        onResynced={(id) => resynced.push(id)}
      />
    ),
    root
  );
  return root;
}

/** The chip's menu, opened: its items' labels, and whether each is disabled. */
async function openMenu(root: HTMLElement) {
  (root.querySelector('.chat-location') as HTMLButtonElement).click();
  await flush();
  const items = [
    ...document.querySelectorAll<HTMLElement>('.chat-location-menu [role="menuitem"]'),
  ];
  const label = (i: HTMLElement) => i.querySelector('.context-menu-label')!.textContent!;
  return {
    labels: items.map(label),
    disabled: items.filter((i) => i.classList.contains('disabled')).map(label),
    item: (text: string) => items.find((i) => label(i) === text)!,
  };
}

test('no hosts, and the chat is here: no chip at all', async () => {
  await app([]);
  const root = chip(chat());
  assert.equal(root.querySelector('.chat-location'), null);
});

test('a chat here: “This computer”, and a move to each deployed host, if it’s there', async () => {
  await app(HOSTS);
  const root = chip(chat());
  assert.equal(root.querySelector('.chat-location-label')?.textContent, 'This computer');
  const menu = await openMenu(root);
  assert.deepEqual(menu.labels, ['Move to Build…', 'Move to Spare… (offline)']);
  assert.deepEqual(menu.disabled, ['Move to Spare… (offline)']);
});

test('no deployed host: the menu offers to set one up', async () => {
  await app([HOSTS[2]]);
  const root = chip(chat());
  const menu = await openMenu(root);
  assert.deepEqual(menu.labels, ['Set up a cloud host…']);
  menu.item('Set up a cloud host…').click();
  assert.equal(appSettingsPage(), 'hosts');
  closeAppSettings();
});

test('a chat on a host: its name, Move back and Re-sync, which loads the chat again', async () => {
  await app(HOSTS, { chat_resync_dialog: () => null });
  const resynced: string[] = [];
  const root = chip(chat({ kind: 'host', host: 'h1' }), resynced);
  const label = root.querySelector('.chat-location')!;
  assert.equal(label.querySelector('.chat-location-label')?.textContent, 'Build');
  assert.ok(label.classList.contains('ok'));
  const menu = await openMenu(root);
  assert.deepEqual(menu.labels, ['Move back to this computer…', 'Re-sync this chat']);
  menu.item('Re-sync this chat').click();
  await flush();
  assert.deepEqual(sent('chat_resync_dialog'), [{ dialogId: 'c1' }]);
  assert.deepEqual(resynced, ['c1']);
});

test('a chat on an offline host: the chip says so, and neither Move back nor Re-sync works', async () => {
  await app(HOSTS);
  const root = chip(chat({ kind: 'host', host: 'h2' }));
  assert.equal(root.querySelector('.chat-location-label')?.textContent, 'Spare (offline)');
  const menu = await openMenu(root);
  assert.deepEqual(menu.disabled, menu.labels);
});

test('a moving chat: the chip says where to, and the menu offers Cancel move', async () => {
  await app(HOSTS, { cloud_cancel_move: () => null });
  const root = chip(chat({ kind: 'moving', host: 'h1' }));
  assert.equal(root.querySelector('.chat-location-label')?.textContent, 'Moving to Build…');
  const menu = await openMenu(root);
  assert.deepEqual(menu.labels, ['Cancel move']);
  menu.item('Cancel move').click();
  await flush();
  assert.deepEqual(sent('cloud_cancel_move'), [{ dialogId: 'c1' }]);
});

const PREFLIGHT: MovePreflight = {
  waits: [],
  trimming: false,
  notes: [],
  redoLost: false,
  files: 0,
  fileBytes: 0,
};

test('“Move to Build…” opens the move dialog, which asks what the move takes', async () => {
  await app(HOSTS, { cloud_move_preflight: () => PREFLIGHT });
  const root = chip(chat());
  (await openMenu(root)).item('Move to Build…').click();
  await flush();
  const dialog = document.querySelector('.move-chat-dialog')!;
  assert.equal(dialog.getAttribute('aria-label'), 'Move “Fix the build” to Build');
  assert.deepEqual(sent('cloud_move_preflight'), [{ dialogId: 'c1', host: 'h1' }]);
});

/** Renders the move dialog for chat c1; `closed` counts its closings. */
async function moveDialog(direction: 'there' | 'back', closed = { count: 0 }) {
  disposeLast();
  const root = container();
  disposeLast = render(
    () => (
      <MoveChatDialog
        dialogId="c1"
        chatTitle="Fix the build"
        direction={direction}
        host="h1"
        startFor={async () => START}
        onClose={() => closed.count++}
      />
    ),
    root
  );
  await flush();
  const dialog = document.querySelector('.move-chat-dialog') as HTMLElement;
  const button = (label: string) =>
    [...dialog.querySelectorAll('button')].find((b) => b.textContent?.trim() === label) as
      HTMLButtonElement | undefined;
  return { dialog, button, text: () => dialog.textContent! };
}

test('the dialog with “wait for me”: what waits, CLM becoming trimming, the files and notes', async () => {
  await app(HOSTS, {
    cloud_move_preflight: () => ({
      waits: [
        'Commands wait there for you to approve them.',
        'Edits wait there for you to approve them.',
      ],
      trimming: true,
      notes: ['assets/big.bin (120 MB) goes up with the project.'],
      redoLost: true,
      files: 3,
      fileBytes: 2 * 1024 * 1024,
    }),
  });
  const { dialog, button, text } = await moveDialog('there');
  const waits = [...dialog.querySelectorAll('.move-chat-list')][0];
  assert.deepEqual(
    [...waits.querySelectorAll('li')].map((li) => li.textContent),
    ['Commands wait there for you to approve them.', 'Edits wait there for you to approve them.']
  );
  assert.match(text(), /as do 3 files its messages name \(2 MB\)/);
  assert.match(text(), /Its context becomes trimming there/);
  assert.match(text(), /“Redo sequentially” for its parallel group won’t be offered/);
  assert.match(text(), /assets\/big\.bin \(120 MB\) goes up with the project\./);
  assert.ok(text().includes(KEYS_NOTICE));
  assert.equal(button('Move')?.disabled, false);
});

test('the dialog with “the AI reviewer, then me”: nothing waits, and no CLM or files to say', async () => {
  await app(HOSTS, { cloud_move_preflight: () => PREFLIGHT });
  const { text } = await moveDialog('there');
  assert.match(text(), /What will wait for you thereNothing: with your permissions/);
  assert.doesNotMatch(text(), /trimming/);
  assert.doesNotMatch(text(), /files its messages name/);
  assert.doesNotMatch(text(), /About the project/);
  assert.ok(text().includes(KEYS_NOTICE), 'the keys notice, always');
});

test('a move that can’t happen now says why, and offers no Move', async () => {
  await app(HOSTS, {
    cloud_move_preflight: () => ({ ...PREFLIGHT, blocked: 'Build isn’t connected.' }),
  });
  const { button, text } = await moveDialog('there');
  assert.match(text(), /Build isn’t connected\./);
  assert.doesNotMatch(text(), /What will wait/);
  assert.equal(button('Move')?.disabled, true);
});

test('a host too old to take chats: its administrator upgrades it for everyone from the dialog', async () => {
  let upgraded = false;
  await app(HOSTS, {
    cloud_move_preflight: () =>
      upgraded
        ? PREFLIGHT
        : {
            ...PREFLIGHT,
            blocked:
              'Build runs Rovik 0.1.0, which can’t take chats: upgrade it for everyone first.',
            upgrade: true,
          },
    hosts_upgrade: () => {
      upgraded = true;
      return null;
    },
  });
  const { button, text } = await moveDialog('there');
  assert.match(text(), /upgrade it for everyone first/);
  assert.equal(button('Move')?.disabled, true);
  button('Upgrade for everyone')!.click();
  await flush();
  assert.deepEqual(sent('hosts_upgrade'), [{ id: 'h1' }]);
  // Its daemons start again on the new Rovik: once it's back, the dialog
  // asks again.
  emit({ type: 'host', host: 'h1', state: 'connecting' });
  emit({ type: 'host', host: 'h1', state: 'ready' });
  await flush();
  assert.match(text(), /What will wait for you there/);
  assert.equal(button('Move')?.disabled, false);
  assert.equal(button('Upgrade for everyone'), undefined);
});

test('a host too old to take chats, for anyone else: whom to ask, and nothing to do', async () => {
  await app(HOSTS, {
    cloud_move_preflight: () => ({
      ...PREFLIGHT,
      blocked: 'Build runs Rovik 0.1.0, which can’t take chats: ask ann or bo to upgrade it.',
    }),
  });
  const { button, text } = await moveDialog('there');
  assert.match(text(), /ask ann or bo to upgrade it\./);
  assert.equal(button('Upgrade for everyone'), undefined);
  assert.equal(button('Move')?.disabled, true);
});

test('the move: its steps as they come, Cancel move, and how it ended', async () => {
  let finish = () => {};
  await app(HOSTS, {
    cloud_move_preflight: () => PREFLIGHT,
    cloud_move_chat: () => new Promise<void>((resolve) => (finish = resolve)),
    cloud_cancel_move: () => null,
    chat_get_dialog: () => null,
  });
  const closed = { count: 0 };
  const { button, text } = await moveDialog('there', closed);
  button('Move')!.click();
  await flush();
  assert.deepEqual(sent('cloud_move_chat'), [{ dialogId: 'c1', host: 'h1', start: START }]);
  assert.match(text(), /Starting…/);
  emit({ type: 'moving', dialogId: 'c1', step: 'pausing' });
  await flush();
  assert.match(text(), /Pausing its run at a safe point…/);
  emit({ type: 'moving', dialogId: 'c1', step: 'pushing' });
  emit({ type: 'pushing', dialogId: 'c1', project: 'p', sent: 2 * 1024 * 1024 });
  await flush();
  assert.match(text(), /Sending the project… 2 MB sent/);
  assert.match(text(), /the chat is read-only until it ends/);
  button('Cancel move')!.click();
  await flush();
  assert.deepEqual(sent('cloud_cancel_move'), [{ dialogId: 'c1' }]);
  emit({ type: 'moving', dialogId: 'c1', step: 'cancelled' });
  finish();
  await flush();
  assert.match(text(), /The move was cancelled: the chat is still on this computer\./);
  assert.equal(button('Cancel move'), undefined);
  button('Close')!.click();
  assert.equal(closed.count, 1);
});

test('a move that fails says why', async () => {
  await app(HOSTS, {
    cloud_move_preflight: () => PREFLIGHT,
    cloud_move_chat: () => {
      emit({
        type: 'moving',
        dialogId: 'c1',
        step: 'failed',
        message: 'The server ran out of disk.',
      });
      throw 'The server ran out of disk.';
    },
    chat_get_dialog: () => null,
  });
  const { button, text } = await moveDialog('there');
  button('Move')!.click();
  await flush();
  assert.match(text(), /The move failed\./);
  assert.match(text(), /The server ran out of disk\./);
  assert.ok(button('Close'));
});

test('moving back: what comes back, then Move back', async () => {
  await app(HOSTS, { cloud_move_back: () => null, chat_get_dialog: () => null });
  const { button, text } = await moveDialog('back');
  assert.match(text(), /Move “Fix the build” back to this computer/);
  assert.match(text(), /comes back with its messages/);
  assert.match(text(), /the move can’t be cancelled/);
  assert.deepEqual(sent('cloud_move_preflight'), [], 'no preflight for a move back');
  button('Move back')!.click();
  await flush();
  assert.deepEqual(sent('cloud_move_back'), [{ dialogId: 'c1', start: START }]);
});

/** A paused run's Continue, in a chat that can or can't change. */
function pausedChat(readOnly: boolean): HTMLElement {
  const messages = [
    { id: 'u1', role: 'user', content: 'Fix it', createdAt: 1 },
    {
      id: 'a1',
      role: 'assistant',
      content: '',
      createdAt: 2,
      meta: { paused: { kind: 'checkpoint', toolCalls: 30 } },
    },
  ] as ChatMessage[];
  const [open] = createStore({
    messages,
    hasOlder: false,
    hasNewer: false,
    loadingOlder: false,
    lastUsed: 0,
  });
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  api.getMessages = () => Promise.resolve({ messages: [], hasOlder: false, hasNewer: false });
  disposeLast();
  const root = container();
  const noop = async () => 0;
  disposeLast = render(
    () => (
      <MessageList
        dialogId="c1"
        chat={open}
        sending={false}
        focusMessageId={null}
        onLoadOlder={noop}
        onLoadNewer={noop}
        onReloadLatest={async () => {}}
        onRetry={() => {}}
        onContinue={() => {}}
        onContinueRun={() => {}}
        onRunChecks={() => {}}
        onOpenProjectChecks={() => {}}
        readOnly={readOnly}
      />
    ),
    root
  );
  return root;
}

test('a moving chat is read-only: its paused run offers no Continue', async () => {
  const continues = (root: HTMLElement) =>
    [...root.querySelectorAll('button')].filter((b) => b.textContent?.trim() === 'Continue');
  assert.equal(continues(pausedChat(false)).length, 1);
  assert.equal(continues(pausedChat(true)).length, 0);
});
