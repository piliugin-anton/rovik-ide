// Component tests: the chat panel in a window with several folders
// (docs/design/workspaces-plan.md, 3.8, D2). Its chats are each folder's
// and the workspace's, listed together, each row saying its folder; a
// folder resolving keeps the open chat and what's typed in it, and a
// crash's unanswered send comes back in whichever folder's chat it was.
// New chat asks which folder the chat works in, an empty chat is reused
// for the folder picked (unless its draft names its folder's files), and
// with no chat the panel asks the same, the input waiting for the chat
// started; deleting the open chat shows the next, never the menu meanwhile.
// A chat names files as paths in its folder, and another folder's file
// isn't sent. Implement on a folder's plan uses that folder's chat. The
// panel as the app mounts it, the app answered here, command by command.
// Run with: npm test
import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import ChatPanel from './ChatPanel';
import { ConfirmDialogHost } from './ConfirmDialog';
import type { MessageRow, UnsettledSend } from '../lib/chatApi';
import type { ActiveFileContext } from '../lib/activeContext';
import type { ChatOwners, OpenFolder } from '../lib/openFolders';
import { addToChat } from '../lib/editorBridge';
import { requestImplementPlan } from '../lib/plans';
import type { DialogSummary } from '../types';

const SETTINGS = {
  version: 2,
  providers: [
    {
      id: 'p1',
      name: 'Test',
      baseUrl: 'https://api.example.com/v1',
      apiKey: 'k',
      stream: true,
      models: [{ id: 'm1', enabled: true }],
    },
  ],
  active: { providerId: 'p1', modelId: 'm1' },
};

/** The database: every chat (listed by its owners), and their messages. */
let dialogs: DialogSummary[] = [];
let rows: Record<string, MessageRow[]> = {};
/** The sends a crash left unanswered, by project. */
let unsettled: Record<string, UnsettledSend[]> = {};
/** The commands the panel sent, in order. */
let calls: { cmd: string; args: any }[] = [];
/** A chat's deletion in the database: held while a test looks (null: done at once). */
let deletion: Promise<void> | null = null;

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case 'load_ai_settings':
        return JSON.stringify(SETTINGS);
      case 'chat_list_dialogs': {
        // Copies: the store changes what it's given.
        const items = dialogs.filter((d) => args.owners?.includes(d.projectId));
        return { items: items.map((d) => ({ ...d })), nextCursor: null };
      }
      case 'chat_get_messages':
        return { messages: [...(rows[args.dialogId] ?? [])], hasOlder: false, hasNewer: false };
      case 'chat_unsettled_sends':
        return unsettled[args.projectId] ?? [];
      case 'agent_status':
        return { runs: [], approvals: [], questions: [] };
      case 'background_tasks':
      case 'chat_messages_answered':
      case 'chats_at_work_elsewhere':
        return [];
      case 'git_head':
        // Not on its main branch: no branch is offered.
        return { branch: 'feature', main_branch: 'main' };
      case 'agent_start':
        return 'run-1';
      case 'chat_delete_dialog':
        return deletion;
      case 'send_chat_message':
        // The chat's title: no model here, so its first words.
        throw new Error('no model here');
      default:
        // Everything else the panel asks for: nothing there.
        return null;
    }
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

const flush = async () => {
  for (let i = 0; i < 10; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};
const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

/** A folder of the workspace, resolved as its own project (`<name>-project`). */
const folder = (name: string, resolved = true): OpenFolder => ({
  path: `/w/${name}`,
  name,
  missing: false,
  ...(resolved
    ? { project: { id: `${name}-project`, name, path: `/w/${name}`, idFile: true } }
    : {}),
});
const api = folder('api');
const web = folder('web');

const chat = (id: string, projectId: string, updatedAt: number): DialogSummary => ({
  id,
  title: id,
  createdAt: updatedAt,
  updatedAt,
  messageCount: 2,
  preview: 'Done.',
  projectId,
});

const row = (dialogId: string, id: string, seq: number, role: 'user' | 'assistant') => ({
  id,
  dialogId,
  seq,
  role,
  content: role === 'user' ? 'Do it' : 'Done.',
  bodyExternal: false,
  bodyBytes: 5,
  toolCalls: null,
  toolCallId: null,
  meta: null,
  error: null,
  failed: false,
  finalized: true,
  attachments: [],
  createdAt: seq,
});

/** Two messages in each chat: none of them is empty. */
function talked(...chats: DialogSummary[]) {
  dialogs = chats;
  rows = Object.fromEntries(
    chats.map((d) => [
      d.id,
      [row(d.id, `${d.id}-1`, 1, 'user'), row(d.id, `${d.id}-2`, 2, 'assistant')],
    ])
  );
}

/** The panels mounted by the test running now: unmounted after it, failing or not. */
const mounted: (() => void)[] = [];
afterEach(() => {
  for (const dispose of mounted.splice(0)) dispose();
});

interface Mounted {
  root: HTMLElement;
  dispose: () => void;
  setFolders: (folders: OpenFolder[]) => void;
  setOwners: (owners: ChatOwners) => void;
  setContext: (context: ActiveFileContext | null) => void;
}

/**
 * The panel on the window's `folders`, `active` the one worked in, its chats
 * those of the workspace `target` and the folders resolved. Each test has a
 * workspace of its own: the store is the app's, kept between mounts.
 */
async function mount(
  target: string,
  folders: OpenFolder[],
  active: OpenFolder | null = folders[0] ?? null
): Promise<Mounted> {
  calls = [];
  localStorage.clear();
  const [shown, setFolders] = createSignal(folders);
  const ownersOf = (list: OpenFolder[]): ChatOwners => ({
    target,
    projects: [target, ...list.flatMap((f) => (f.project ? [f.project.id] : []))],
  });
  const [owners, setOwners] = createSignal<ChatOwners>(ownersOf(folders));
  const [context, setContext] = createSignal<ActiveFileContext | null>(null);
  const root = container();
  const dispose = render(
    () => (
      <ChatPanel
        folders={shown}
        activeFolder={() => active}
        activeFileContext={context}
        files={() => []}
        owners={owners}
      />
    ),
    root
  );
  mounted.push(dispose);
  await flush();
  return { root, dispose, setFolders, setOwners, setContext };
}

const boxes = (root: ParentNode) => [
  ...root.querySelectorAll<HTMLElement>('[role="menuitemcheckbox"]'),
];
const box = (root: ParentNode, name: string) => {
  const found = boxes(root).find((b) => b.textContent === name);
  assert.ok(found, `no box for ${name}`);
  return found;
};
const startItem = (root: ParentNode) =>
  [...root.querySelectorAll<HTMLElement>('[role="menuitem"]')].find((item) =>
    item.textContent?.startsWith('Start chat')
  );
const input = (root: ParentNode) =>
  root.querySelector('.chat-input-contenteditable') as HTMLElement;
const label = (root: ParentNode) => root.querySelector('.chat-folders-label');
const note = (root: ParentNode) =>
  root.querySelector('.chat-folder-note')?.textContent?.replace(/\s+/g, ' ').trim();

async function type(root: HTMLElement, text: string) {
  input(root).textContent = text;
  input(root).dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

async function sendText(root: HTMLElement, text: string) {
  await type(root, text);
  (root.querySelector('.chat-send-button') as HTMLButtonElement).click();
  await flush();
}

/** ＋ in the chat's header: New chat. */
async function plus(root: HTMLElement) {
  (root.querySelector('.chat-controls .icon-button') as HTMLButtonElement).click();
  await flush();
}

/** What Add to chat gives for a file's first lines. */
const reference = (path: string) => ({
  id: path,
  path,
  name: path.split('/').pop()!,
  relativePath: path,
  lines: { start: 1, end: 2 },
});

/**
 * Deletes the chat titled `title` from the history, saying yes when asked:
 * whether the folder menu showed while the database deleted it. The deletion
 * ends either way: the store is the app's, kept for the next test.
 */
async function menuWhileDeleting(root: HTMLElement, title: string): Promise<boolean> {
  let done = () => {};
  deletion = new Promise((resolve) => (done = resolve));
  try {
    const confirm = render(() => <ConfirmDialogHost />, container());
    mounted.push(confirm);
    (root.querySelector('.chat-history-button') as HTMLButtonElement).click();
    await flush();
    const row = [...root.querySelectorAll('.chat-history-item')].find(
      (item) => item.querySelector('.chat-history-item-title')?.textContent === title
    );
    (row!.querySelector('.chat-history-delete') as HTMLButtonElement).click();
    await flush();
    const yes = [...document.querySelectorAll('button')].find((b) => b.textContent === 'Delete');
    yes!.click();
    await flush();
    return root.querySelector('.chat-start') !== null;
  } finally {
    done();
    deletion = null;
    await flush();
  }
}

test('with two folders, New chat lists them with boxes, the active one checked, and Start chat makes a folder chat in it', async () => {
  talked(chat('api-1', 'api-project', 2));
  const { root, dispose } = await mount('workspace:boxes', [api, web], web);
  await plus(root);
  assert.deepEqual(
    boxes(root).map((b) => [b.textContent, b.getAttribute('aria-checked')]),
    [
      ['api', 'false'],
      ['web', 'true'],
    ]
  );
  assert.equal(startItem(root)?.textContent, 'Start chat in web');
  startItem(root)!.click();
  await flush();
  assert.equal(boxes(root).length, 0, 'the menu closes');
  assert.deepEqual(
    sent('chat_create_dialog').map((a) => a.dialog.projectId),
    ['web-project'],
    'a folder chat of web'
  );
  assert.equal(label(root)?.textContent, 'in web');
  dispose();
});

test('a folder checked unchecks the other: a folder chat works in one', async () => {
  talked(chat('api-1', 'api-project', 2));
  const { root, dispose } = await mount('workspace:one-at-a-time', [api, web], api);
  await plus(root);
  box(root, 'web').click();
  await flush();
  assert.deepEqual(
    boxes(root).map((b) => b.getAttribute('aria-checked')),
    ['false', 'true']
  );
  box(root, 'web').click();
  await flush();
  assert.equal(startItem(root)?.getAttribute('aria-disabled'), 'true', 'none checked: no chat');
  startItem(root)!.click();
  await flush();
  assert.deepEqual(sent('chat_create_dialog'), []);
  dispose();
});

test('Escape makes no chat', async () => {
  talked(chat('api-1', 'api-project', 2));
  const { root, dispose } = await mount('workspace:escape', [api, web]);
  await plus(root);
  const menu = document.querySelector('.folder-menu') as HTMLElement;
  menu.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await flush();
  assert.equal(document.querySelector('.folder-menu'), null);
  assert.deepEqual(sent('chat_create_dialog'), []);
  dispose();
});

test('with one folder there is no menu: New chat starts a chat in it', async () => {
  const project = folder('solo');
  talked(chat('solo-1', 'solo-project', 2));
  const { root, dispose } = await mount('workspace:solo', [project]);
  await plus(root);
  assert.equal(boxes(root).length, 0);
  assert.deepEqual(
    sent('chat_create_dialog').map((a) => a.dialog.projectId),
    ['solo-project']
  );
  assert.equal(label(root), null, 'and no label: there is one folder');
  dispose();
});

test('with no chat, the panel shows the menu rather than guess a folder', async () => {
  talked();
  const { root, dispose } = await mount('workspace:empty', [api, web], web);
  assert.ok(root.querySelector('.chat-start'), 'the menu, in place');
  assert.deepEqual(sent('chat_create_dialog'), [], 'no empty chat in a guessed folder');
  assert.equal(box(root, 'web').getAttribute('aria-checked'), 'true');
  startItem(root)!.click();
  await flush();
  assert.deepEqual(
    sent('chat_create_dialog').map((a) => a.dialog.projectId),
    ['web-project']
  );
  assert.equal(root.querySelector('.chat-start'), null);
  dispose();
});

test('an empty chat is reused, moved to the folder picked', async () => {
  talked();
  const { root, dispose } = await mount('workspace:reuse', [api, web], web);
  startItem(root)!.click();
  await flush();
  const made = sent('chat_create_dialog')[0].dialog.id;
  await plus(root);
  box(root, 'api').click();
  await flush();
  startItem(root)!.click();
  await flush();
  assert.equal(sent('chat_create_dialog').length, 1, 'no second empty chat');
  assert.deepEqual(sent('chat_move_empty_dialog'), [
    { dialogId: made, projectId: 'api-project', folders: null },
  ]);
  assert.equal(label(root)?.textContent, 'in api');
  dispose();
});

test('an empty chat whose draft names its folder’s file stays, and the other folder gets a chat', async () => {
  // Moved, the chat would send api's file by its path in web.
  talked();
  const { root, dispose } = await mount('workspace:reuse-draft', [api, web], api);
  startItem(root)!.click();
  await flush();
  addToChat(reference('/w/api/src/index.ts'));
  await flush();
  assert.equal(root.querySelectorAll('.chat-file-reference').length, 1);
  await plus(root);
  box(root, 'web').click();
  await flush();
  startItem(root)!.click();
  await flush();
  assert.deepEqual(sent('chat_move_empty_dialog'), [], 'api’s chat keeps its draft');
  assert.deepEqual(
    sent('chat_create_dialog').map((a) => a.dialog.projectId),
    ['api-project', 'web-project']
  );
  assert.equal(root.querySelectorAll('.chat-file-reference').length, 0, 'web’s starts empty');
  assert.equal(label(root)?.textContent, 'in web');
  dispose();
});

test('with no chat the input waits for one, and what Add to chat gave goes in the chat started', async () => {
  talked();
  const { root, dispose } = await mount('workspace:no-chat-yet', [api, web], web);
  assert.equal(input(root).getAttribute('contenteditable'), 'false');
  assert.equal(input(root).getAttribute('data-placeholder'), 'Start a chat above to write to it');
  addToChat(reference('/w/web/src/cart.ts'));
  await flush();
  assert.equal(root.querySelectorAll('.chat-file-reference').length, 0, 'no chat to take it yet');
  assert.equal(note(root), undefined, 'nor is it turned away');
  startItem(root)!.click();
  await flush();
  assert.equal(input(root).getAttribute('contenteditable'), 'true');
  const chip = root.querySelector<HTMLElement>('.chat-file-reference');
  assert.equal(JSON.parse(chip!.dataset.file!).relativePath, 'src/cart.ts');
  dispose();
});

test('deleting the open chat shows the next one, never the folder menu meanwhile', async () => {
  // One folder: a new chat in it once the last one goes, as a project had.
  talked(chat('one-1', 'one-project', 2));
  const project = await mount('workspace:delete-one', [folder('one')]);
  assert.equal(await menuWhileDeleting(project.root, 'one-1'), false, 'no menu while it goes');
  assert.equal(project.root.querySelector('.chat-start'), null);
  assert.deepEqual(
    sent('chat_create_dialog').map((a) => a.dialog.projectId),
    ['one-project']
  );
  project.dispose();

  // Two folders and another chat: that one opens.
  talked(chat('api-2', 'api-project', 3), chat('web-1', 'web-project', 2));
  const { root, dispose } = await mount('workspace:delete-two', [api, web]);
  assert.equal(await menuWhileDeleting(root, 'api-2'), false, 'there is a chat to show');
  assert.equal(root.querySelector('.chat-start'), null);
  assert.ok(
    sent('chat_get_messages').some((a) => a.dialogId === 'web-1'),
    'web-1 opens'
  );
  dispose();
});

test('a missing folder is listed but can’t be checked', async () => {
  talked();
  const gone = { ...folder('web', false), missing: true };
  const { root, dispose } = await mount('workspace:missing', [api, gone], gone);
  const row = box(root, 'web');
  assert.equal(row.getAttribute('aria-disabled'), 'true');
  assert.match(row.title, /isn’t there/);
  row.click();
  await flush();
  assert.equal(row.getAttribute('aria-checked'), 'false');
  assert.equal(startItem(root)?.textContent, 'Start chat in api', 'the folder that is there');
  dispose();
});

test('a copy that still has its original’s chats can’t be checked: its chat would work in the original', async () => {
  talked();
  const copy: OpenFolder = { ...folder('api-copy'), project: api.project };
  const { root, dispose } = await mount('workspace:copy', [api, copy], copy);
  const item = box(root, 'api-copy');
  assert.equal(item.getAttribute('aria-disabled'), 'true');
  assert.equal(
    item.title,
    'api-copy shares its chats with api until the copy’s history is separated'
  );
  assert.equal(box(root, 'api').getAttribute('aria-checked'), 'true', 'the original instead');
  dispose();
});

test('the menu in place is reached with Tab; the floating one has the focus as it opens', async () => {
  talked();
  const empty = await mount('workspace:tab-in-place', [api, web], web);
  assert.deepEqual(
    boxes(empty.root).map((b) => b.tabIndex),
    [0, -1]
  );
  empty.dispose();

  talked(chat('api-1', 'api-project', 2));
  const { root, dispose } = await mount('workspace:tab-floating', [api, web]);
  await plus(root);
  assert.deepEqual(
    boxes(root).map((b) => b.tabIndex),
    [-1, -1]
  );
  dispose();
});

test('a folder chat’s label shows its folder and isn’t a control', async () => {
  talked(chat('web-1', 'web-project', 2));
  const { root, dispose } = await mount('workspace:label', [api, web]);
  const shown = label(root)!;
  assert.equal(shown.textContent, 'in web');
  assert.equal(shown.tagName, 'SPAN');
  assert.equal(shown.querySelector('button'), null);
  assert.equal(shown.getAttribute('title'), 'This chat works in /w/web');
  dispose();
});

test('a row shows its chat’s folders', async () => {
  talked(chat('web-1', 'web-project', 3), chat('api-1', 'api-project', 2));
  const { root, dispose } = await mount('workspace:rows', [api, web]);
  (root.querySelector('.chat-history-button') as HTMLButtonElement).click();
  await flush();
  const listed = [...root.querySelectorAll('.chat-history-item')].map((item) => [
    item.querySelector('.chat-history-item-title')?.textContent,
    item.querySelector('.chat-history-item-folders')?.textContent,
  ]);
  assert.deepEqual(listed, [
    ['web-1', 'web'],
    ['api-1', 'api'],
  ]);
  dispose();
});

test('adding a folder keeps the open chat and its draft', async () => {
  talked(chat('api-1', 'api-project', 2), chat('web-1', 'web-project', 3));
  const opening = folder('web', false);
  const { root, dispose, setFolders, setOwners } = await mount('workspace:added', [api, opening]);
  assert.equal(label(root)?.textContent, 'in api', 'web isn’t resolved: its chats aren’t listed');
  await type(root, 'half a thought');
  // web resolves: its chats join the list; the chat open and the text typed stay.
  calls = [];
  setFolders([api, web]);
  setOwners({
    target: 'workspace:added',
    projects: ['workspace:added', 'api-project', 'web-project'],
  });
  await flush();
  assert.equal(label(root)?.textContent, 'in api');
  assert.equal(input(root).textContent, 'half a thought');
  assert.deepEqual(sent('chat_unsettled_sends'), [{ projectId: 'web-project' }], 'web is new here');
  dispose();
});

test('after a crash, an unanswered send in the second folder’s chat comes back', async () => {
  talked(chat('web-1', 'web-project', 3), chat('api-1', 'api-project', 2));
  rows['web-1'] = [row('web-1', 'web-1-1', 1, 'user')];
  unsettled = {
    'web-project': [{ dialogId: 'web-1', messageId: 'web-1-1', content: 'Style the cart' }],
  };
  try {
    const { root, dispose } = await mount('workspace:crash', [api, web]);
    assert.equal(label(root)?.textContent, 'in web');
    assert.equal(input(root).textContent, 'Style the cart');
    assert.deepEqual(
      sent('chat_unsettled_sends').map((a) => a.projectId),
      ['workspace:crash', 'api-project', 'web-project'],
      'each owner is asked'
    );
    dispose();
  } finally {
    unsettled = {};
  }
});

test('the editor context names the file as the chat does, and another folder’s isn’t sent', async () => {
  talked(chat('web-1', 'web-project', 2));
  const { root, dispose, setContext } = await mount('workspace:context', [api, web]);
  const context = (path: string): ActiveFileContext => ({
    path,
    language: 'typescript',
    line: 3,
    column: 1,
    enclosingSymbol: null,
    selection: null,
  });
  setContext(context('/w/web/src/cart.ts'));
  await sendText(root, 'Why is this slow?');
  const [first] = sent('agent_start');
  assert.match(first.request.currentUserContent, /file src\/cart\.ts \(typescript\)/);
  assert.doesNotMatch(first.request.currentUserContent, /\/w\/web/, 'not by its full path');
  assert.equal(note(root), undefined);

  // api's file in web's chat: not named to it, and the input says why.
  setContext(context('/w/api/src/routes.ts'));
  await flush();
  assert.equal(
    note(root),
    'api/src/routes.ts is in api; this chat works in web. It isn’t named to the chat.'
  );
  dispose();
});

test('another folder’s file isn’t added to the chat, and the input says why', async () => {
  talked(chat('web-1', 'web-project', 2));
  const { root, dispose } = await mount('workspace:add-to-chat', [api, web]);
  addToChat(reference('/w/web/src/cart.ts'));
  await flush();
  const chip = root.querySelector<HTMLElement>('.chat-file-reference');
  assert.equal(JSON.parse(chip!.dataset.file!).relativePath, 'src/cart.ts');

  addToChat(reference('/w/api/src/routes.ts'));
  await flush();
  assert.equal(root.querySelectorAll('.chat-file-reference').length, 1, 'api’s file stays out');
  assert.equal(note(root), 'api/src/routes.ts is in api; this chat works in web.');
  dispose();
});

test('Implement on another folder’s plan starts a chat there, and on the open chat’s folder’s plan uses it', async () => {
  talked(chat('api-1', 'api-project', 2));
  const { dispose } = await mount('workspace:implement', [api, web]);
  requestImplementPlan({ path: '/w/web/.rovik/plans/cart.md' });
  await flush();
  assert.deepEqual(
    sent('chat_create_dialog').map((a) => a.dialog.projectId),
    ['web-project'],
    'a new chat in web'
  );
  const [there] = sent('agent_start');
  assert.equal(there.request.dialogId, sent('chat_create_dialog')[0].dialog.id);
  assert.equal(there.request.root, '/w/web');
  assert.match(there.request.start.message.content, /`\.rovik\/plans\/cart\.md`/);
  dispose();

  // The open chat is web's own: it takes web's plan.
  talked(chat('web-1', 'web-project', 2));
  const again = await mount('workspace:implement-here', [api, web]);
  requestImplementPlan({ path: '/w/web/.rovik/plans/cart.md' });
  await flush();
  assert.deepEqual(sent('chat_create_dialog'), [], 'no new chat');
  assert.equal(sent('agent_start')[0]?.request.dialogId, 'web-1');
  again.dispose();
});
