// Component test: a remote chat's isolation bar (U05). It lists what the
// chat changed in its worktree while a run goes, its result's pending
// reason and conflicts, a newer result held back while they're resolved,
// a sync that waits with Hand to the chat and Keep the older copy, Sync
// now, and how many changes Land landed. A local chat's bar has no sync,
// and a chat that can't change now has its buttons off, saying why. Run
// with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import IsolationBar from './IsolationBar';
import { ConfirmDialogHost } from './ConfirmDialog';
import { notifications } from '../lib/notifications';
import type { ChatSync, OverlayStatus } from '../lib/overlay';

/** What `overlay_status` answers now, and what the other commands do. */
let status: OverlayStatus;
let answers: Record<string, (args: any) => unknown> = {};
const calls: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    if (cmd === 'overlay_status') return structuredClone(status);
    return answers[cmd]?.(args) ?? null;
  },
};

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
/** Lets clicks, the commands they call and the status read after them finish. */
async function settle() {
  for (let i = 0; i < 6; i++) await flush();
}
const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

const FILES: OverlayStatus['files'] = [
  { path: 'src/main.rs', change: 'changed' },
  { path: 'src/new.rs', change: 'added' },
];

const remoteStatus = (patch: Partial<OverlayStatus> = {}): OverlayStatus => ({
  isolated: true,
  peerConflicts: [],
  mergedIn: [],
  files: FILES,
  used: 0,
  limit: 0,
  byProducts: 0,
  ...patch,
});

const WAITING: ChatSync = { dialogId: 'c1', state: 'conflicts', files: ['src/main.rs'] };

interface Options {
  remote?: boolean;
  running?: boolean;
  readOnly?: string;
  canAskChat?: boolean;
  /** Whether asking the chat starts its run. */
  askStarts?: boolean;
}

let disposeLast = () => {};
after(() => disposeLast());

/** Renders chat c1's bar, with the panel's side of it. */
async function bar(options: Options = {}) {
  disposeLast();
  calls.length = 0;
  const root = container();
  const asked: { dialogId: string; request: string; shown?: string }[] = [];
  const [running, setRunning] = createSignal(options.running ?? false);
  const [revision, setRevision] = createSignal(0);
  disposeLast = render(
    () => (
      <>
        <IsolationBar
          dialogId="c1"
          revision={revision()}
          running={running()}
          canAskChat={options.canAskChat ?? !running()}
          onAskChat={async (dialogId, request, shown) => {
            asked.push({ dialogId, request, shown });
            return options.askStarts ?? true;
          }}
          remote={options.remote ?? true}
          readOnly={options.readOnly}
        />
        <ConfirmDialogHost />
      </>
    ),
    root
  );
  await settle();
  /** The panel tells the bar to look again (a run went on, a cloud event). */
  const again = async () => {
    setRevision((n) => n + 1);
    await settle();
  };
  return { root, asked, setRunning, again };
}

const button = (root: ParentNode, label: string) =>
  [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === label) as
    HTMLButtonElement | undefined;
const text = (root: ParentNode, selector: string) =>
  root.querySelector(selector)?.textContent?.replace(/\s+/g, ' ').trim();
/** A line of the bar: what it says, and its buttons. */
function line(root: ParentNode, selector: string) {
  const el = root.querySelector(selector);
  if (!el) return undefined;
  const nodes = [...el.childNodes];
  const says = nodes
    .filter((n) => n.nodeName === '#text')
    .map((n) => n.textContent)
    .join('')
    .replace(/\s+/g, ' ')
    .trim();
  const buttons = [...el.querySelectorAll('button')].map((b) => b.textContent!.trim());
  return { says, buttons };
}
const listed = (root: HTMLElement) =>
  [...root.querySelectorAll('.isolation-bar-path')].map((p) => p.textContent);
const shown = (from: number) =>
  notifications()
    .slice(from)
    .map((n) => `${n.kind}: ${n.title}`);

test('while a run goes, the bar lists what the chat changed there, its buttons off', async () => {
  status = remoteStatus();
  const { root, setRunning, again } = await bar({ running: true });
  assert.match(text(root, '.isolation-bar-title')!, /^2 changed files in this chat/);
  (root.querySelector('.isolation-bar-toggle') as HTMLButtonElement).click();
  await flush();
  assert.deepEqual(listed(root), ['src/main.rs', 'src/new.rs']);
  for (const label of ['Sync now', 'Discard', 'Land']) {
    assert.equal(button(root, label)!.disabled, true, label);
  }
  assert.equal(button(root, 'Sync now')!.title, 'Stop the run first');

  // The run writes another file: the bar shows it when it looks again.
  status = remoteStatus({ files: [...FILES, { path: 'src/more.rs', change: 'added' }] });
  await again();
  assert.deepEqual(listed(root), ['src/main.rs', 'src/new.rs', 'src/more.rs']);

  // The run ends: the buttons work.
  setRunning(false);
  await flush();
  assert.equal(button(root, 'Land')!.disabled, false);
  assert.equal(button(root, 'Sync now')!.disabled, false);
});

test('a result’s conflicts wait in the bar, and a newer result can come in their place', async () => {
  status = remoteStatus({
    pending: {
      reason: 'Not landed: the project changed the same lines.',
      conflicts: [{ path: 'src/main.rs', reason: 'changed on both sides' }],
    },
    newerResult: true,
  });
  answers = {
    overlay_take_newer: () => {
      status = remoteStatus({
        pending: { reason: 'Ready to land: it lands once its project is open.', conflicts: [] },
      });
    },
  };
  const { root } = await bar();
  assert.equal(
    text(root, '.isolation-bar-reason'),
    'Not landed: the project changed the same lines.'
  );
  for (const label of ['Resolve…', 'Ask the chat', 'Land anyway']) assert.ok(button(root, label));
  const conflicted = [...root.querySelectorAll('.isolation-bar-files li')]
    .filter((li) => li.querySelector('.isolation-bar-conflict'))
    .map((li) => li.querySelector('.isolation-bar-path')!.textContent);
  assert.deepEqual(conflicted, ['src/main.rs']);
  assert.deepEqual(line(root, '.isolation-bar-newer'), {
    says: 'A newer result of this chat is ready. It comes in once these conflicts are settled, or now, in their place.',
    buttons: ['Take the newer result'],
  });

  button(root, 'Take the newer result')!.click();
  await settle();
  const confirm = document.querySelector(
    '.confirm-dialog-actions button.primary'
  ) as HTMLButtonElement;
  assert.equal(confirm.textContent, 'Take the newer result');
  confirm.click();
  await settle();
  assert.deepEqual(sent('overlay_take_newer'), [{ dialogId: 'c1' }]);
  assert.equal(root.querySelector('.isolation-bar-newer'), null);
  assert.equal(
    text(root, '.isolation-bar-reason'),
    'Ready to land: it lands once its project is open.'
  );
  assert.ok(button(root, 'Land'), 'no conflicts left');
});

test('a sync that waits says why: the user keeps the older copy, or hands the conflicts over', async () => {
  status = remoteStatus({ sync: WAITING });
  answers = {
    overlay_keep_older: () => {
      status = remoteStatus();
    },
  };
  const { root, asked, again } = await bar();
  assert.deepEqual(line(root, '.isolation-bar-sync'), {
    says: 'Sync waits: 1 file changed both here and in the cloud chat, on the same lines: src/main.rs. The chat goes on with its older copy.',
    buttons: ['Hand to the chat', 'Keep the older copy'],
  });
  button(root, 'Keep the older copy')!.click();
  await settle();
  assert.deepEqual(sent('overlay_keep_older'), [{ dialogId: 'c1' }]);
  assert.equal(root.querySelector('.isolation-bar-sync'), null, 'the bar stops asking');

  // The next sync waits again: handed to the chat this time.
  status = remoteStatus({ sync: WAITING });
  const request =
    'Your changes conflict with changes made in the project meanwhile, in: `src/main.rs`.';
  answers.overlay_sync_to_chat = () => {
    status = remoteStatus();
    return { sync: { ...WAITING, state: 'marked' }, request };
  };
  await again();
  button(root, 'Hand to the chat')!.click();
  await settle();
  assert.deepEqual(sent('overlay_sync_to_chat'), [{ dialogId: 'c1' }]);
  assert.deepEqual(asked, [
    { dialogId: 'c1', request, shown: 'Settle the conflicts with the project’s newer changes.' },
  ]);
  assert.deepEqual(sent('overlay_unmark'), [], 'the chat took them');
  assert.equal(root.querySelector('.isolation-bar-sync'), null);
});

test('conflicts handed to a chat that doesn’t start come back out', async () => {
  status = remoteStatus({ sync: WAITING });
  answers = {
    overlay_sync_to_chat: () => ({ sync: { ...WAITING, state: 'marked' }, request: 'Settle' }),
  };
  const { root, asked } = await bar({ askStarts: false });
  button(root, 'Hand to the chat')!.click();
  await settle();
  assert.equal(asked.length, 1);
  assert.deepEqual(sent('overlay_unmark'), [{ dialogId: 'c1', files: ['src/main.rs'] }]);

  // A chat that can't take them now isn't handed anything.
  const before = notifications().length;
  const busy = await bar({ canAskChat: false });
  button(busy.root, 'Hand to the chat')!.click();
  await settle();
  assert.deepEqual(sent('overlay_sync_to_chat'), []);
  assert.deepEqual(shown(before), ['warning: The chat can’t take it now']);
});

test('files that can’t take markers can only wait', async () => {
  status = remoteStatus({ sync: { dialogId: 'c1', state: 'unmergeable', files: ['logo.png'] } });
  const { root } = await bar();
  assert.deepEqual(line(root, '.isolation-bar-sync'), {
    says: 'Sync waits: 1 file changed both here and in the cloud chat can’t be merged (binary, or deleted on one side): logo.png. The chat goes on with its older copy.',
    buttons: ['Keep the older copy'],
  });
});

test('Sync now brings the project into the chat’s copy, or says it has it', async () => {
  status = remoteStatus();
  let next: ChatSync | null = null;
  answers = { overlay_sync_now: () => next };
  const { root } = await bar();
  const before = notifications().length;
  button(root, 'Sync now')!.click();
  await settle();
  assert.deepEqual(sent('overlay_sync_now'), [{ dialogId: 'c1' }]);
  assert.deepEqual(shown(before), ['info: Already in sync']);

  next = { dialogId: 'c1', state: 'synced' };
  button(root, 'Sync now')!.click();
  await settle();
  assert.deepEqual(shown(before), ['info: Already in sync', 'success: Synced']);

  // One that waits says why in the bar, not in a notification.
  answers.overlay_sync_now = () => {
    status = remoteStatus({ sync: WAITING });
    return WAITING;
  };
  button(root, 'Sync now')!.click();
  await settle();
  assert.equal(shown(before).length, 2);
  assert.match(line(root, '.isolation-bar-sync')!.says, /^Sync waits: 1 file/);
});

test('a local chat’s bar doesn’t sync; one that can’t change has its buttons off, saying why', async () => {
  status = remoteStatus();
  const local = await bar({ remote: false });
  assert.equal(button(local.root, 'Sync now'), undefined);
  assert.ok(button(local.root, 'Land'));

  const offline = 'Build is offline: this chat is read-only until it’s back.';
  status = remoteStatus({ sync: WAITING });
  const { root } = await bar({ readOnly: offline });
  const buttons = ['Sync now', 'Discard', 'Land', 'Hand to the chat', 'Keep the older copy'];
  for (const label of buttons) {
    assert.equal(button(root, label)!.disabled, true, label);
    assert.equal(button(root, label)!.title, offline, label);
  }
});

test('Land says how many of the chat’s changes are in the project', async () => {
  status = remoteStatus();
  let paths = ['src/main.rs'];
  answers = { overlay_land: () => ({ landed: { id: 'l1', paths }, conflicts: [] }) };
  const { root } = await bar();
  const before = notifications().length;
  button(root, 'Land')!.click();
  await settle();
  paths = ['src/main.rs', 'src/new.rs'];
  button(root, 'Land')!.click();
  await settle();
  assert.deepEqual(
    notifications()
      .slice(before)
      .map((n) => n.message),
    ['1 change from this chat is in the project.', '2 changes from this chat are in the project.']
  );
});
