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
// isn't sent. Implement on a folder's plan uses that folder's chat, and on
// a plan naming folders a workspace chat across them (D28). The panel as
// the app mounts it, the app answered here, command by command.
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
import { workspaceIdOf, type ChatOwners, type OpenFolder } from '../lib/openFolders';
import { addToChat } from '../lib/editorBridge';
import { notifications } from '../lib/notifications';
import { requestImplementPlan } from '../lib/plans';
import { appSettingsPage, closeAppSettings } from '../lib/appSettings';
import { noteBackgroundTasks } from '../lib/backgroundTasks';
import type { DialogSummary, GitHead } from '../types';
import { NewBranchDialogHost } from './NewBranchDialog';

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
/** The plans' files, by path. */
let plans: Record<string, string> = {};
/** The files `list_project_files` lists, by folder. */
let projectFiles: Record<string, string[]> = {};
/**
 * The virtual-files driver here: none unless a test installs it, so a chat
 * works in one folder (4.12).
 */
let driverInstalled = false;
/** Why `chat_set_folders` refuses now; null: it takes them. */
let foldersRefused: string | null = null;
/** Where HEAD is in each folder's repository; else on a branch of its own: no offer. */
let heads: Record<string, GitHead> = {};

/** The app's event handlers, by id, and what each listens to. */
const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = callbacks.size + 1;
    callbacks.set(id, callback);
    return id;
  },
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case 'plugin:event|listen':
        listeners.push({ event: args.event, handler: args.handler });
        return args.handler;
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
        // Not on its main branch unless a test says: no branch is offered.
        return heads[args.path] ?? { branch: 'feature', main_branch: 'main' };
      case 'git_repos':
        // Each folder a repository of its own.
        return {
          repos: args.folders.map((f: { path: string; name: string }) => ({
            key: `${f.path}/.git`,
            workdir: f.path,
            name: f.name,
            scopes: [''],
            folders: [f.path],
          })),
          without: [],
        };
      case 'agent_start':
        return 'run-1';
      case 'chat_delete_dialog':
        return deletion;
      case 'chat_read_plan':
        if (args.path in plans) return plans[args.path];
        throw new Error(`${args.path}: no such file`);
      case 'list_project_files':
        return { files: projectFiles[args.root] ?? [], truncated: false };
      case 'chat_set_folders':
        if (foldersRefused) throw foldersRefused;
        return null;
      case 'vfs_status':
        return {
          driver: {
            driver: 'FUSE',
            installed: driverInstalled,
            version: null,
            detail: null,
            canInstall: true,
          },
        };
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

/** The app sends `payload` as an agent event. */
function emit(payload: unknown) {
  for (const l of listeners.filter((l) => l.event === 'agent-event')) {
    callbacks.get(l.handler)!({ event: 'agent-event', id: 1, payload });
  }
}

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
  // The workspace's own chats are its id's, listed first, as `chatOwners` has them.
  const ownersOf = (list: OpenFolder[]): ChatOwners => ({
    target,
    projects: [
      target.replace(/^workspace:/, ''),
      ...list.flatMap((f) => (f.project ? [f.project.id] : [])),
    ],
  });
  const [owners, setOwners] = createSignal<ChatOwners>(ownersOf(folders));
  const [context, setContext] = createSignal<ActiveFileContext | null>(null);
  const root = container();
  const dispose = render(
    () => (
      <>
        <ChatPanel
          folders={shown}
          workspace={() => workspaceIdOf(target)}
          activeFolder={() => active}
          activeFileContext={context}
          files={() => []}
          owners={owners}
        />
        <NewBranchDialogHost />
      </>
    ),
    root
  );
  mounted.push(dispose);
  await flush();
  return { root, dispose, setFolders, setOwners, setContext };
}

/** The folders' boxes in the menu, without All folders. */
const boxes = (root: ParentNode) => [
  ...root.querySelectorAll<HTMLElement>('[role="menuitemcheckbox"]:not(.folder-menu-all)'),
];
/** The menu's All folders box. */
const allBox = (root: ParentNode) => root.querySelector<HTMLElement>('.folder-menu-all');
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
  assert.equal(allBox(empty.root)?.tabIndex, 0, 'its first item: All folders');
  assert.deepEqual(
    boxes(empty.root).map((b) => b.tabIndex),
    [-1, -1]
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
    projects: ['added', 'api-project', 'web-project'],
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
      ['crash', 'api-project', 'web-project'],
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

/** A workspace chat of `id`, working in `folders` (`"all"`, or names as JSON). */
const workspaceChat = (dialogId: string, id: string, folders: string): DialogSummary => ({
  ...chat(dialogId, id, 2),
  folders,
});

/** The implementation lines the open chat shows. */
const implementations = (root: ParentNode) =>
  [...root.querySelectorAll('.chat-message.user')]
    .map((m) => m.textContent ?? '')
    .filter((text) => text.includes('Implement the plan'));

test('Implement on a plan naming folders makes a workspace chat across them, even one, and uses the open one that works in them', async () => {
  talked(chat('api-1', 'api-project', 2));
  const { dispose } = await mount('workspace:plan-new', [api, web]);
  requestImplementPlan({ path: '/w/api/.rovik/plans/cart.md', folders: ['api'] });
  await flush();
  assert.deepEqual(
    sent('chat_create_dialog').map(({ dialog }) => [dialog.projectId, dialog.folders]),
    [['plan-new', '["api"]']],
    'a workspace chat in api, not api’s folder chat'
  );
  dispose();

  // The open chat is the workspace's, in all its folders: it takes the plan.
  talked(workspaceChat('ws-1', 'plan-open', 'all'));
  const again = await mount('workspace:plan-open', [api, web]);
  requestImplementPlan({ path: '/w/api/.rovik/plans/cart.md', folders: ['api', 'web'] });
  await flush();
  assert.deepEqual(sent('chat_create_dialog'), [], 'no new chat');
  assert.equal(implementations(again.root).length, 1);
  assert.match(
    implementations(again.root)[0],
    /the plan in `?api\/\.rovik\/plans\/cart\.md/,
    'by its chat path'
  );
  again.dispose();
});

test('an empty chat is reused for a plan naming folders, moved to the workspace and its folders', async () => {
  talked();
  const { root, dispose } = await mount('workspace:plan-reuse', [api, web], web);
  startItem(root)!.click();
  await flush();
  const made = sent('chat_create_dialog')[0].dialog.id;
  requestImplementPlan({ path: '/w/api/.rovik/plans/cart.md', folders: ['api', 'web'] });
  await flush();
  assert.equal(sent('chat_create_dialog').length, 1, 'no second chat');
  assert.deepEqual(sent('chat_move_empty_dialog'), [
    { dialogId: made, projectId: 'plan-reuse', folders: '["api","web"]' },
  ]);
  assert.match(implementations(root)[0] ?? '', /the plan in `?api\/\.rovik\/plans\/cart\.md/);
  dispose();

  // An empty workspace chat in api alone isn't one in api and web: it moves too.
  dialogs = [{ ...workspaceChat('ws-empty', 'plan-same', '["api"]'), messageCount: 0 }];
  rows = {};
  const same = await mount('workspace:plan-same', [api, web]);
  requestImplementPlan({ path: '/w/api/.rovik/plans/cart.md', folders: ['api', 'web'] });
  await flush();
  assert.deepEqual(sent('chat_create_dialog'), []);
  assert.deepEqual(sent('chat_move_empty_dialog'), [
    { dialogId: 'ws-empty', projectId: 'plan-same', folders: '["api","web"]' },
  ]);
  same.dispose();
});

test('Implement on a plan naming a folder this workspace lacks starts no chat, and says which', async () => {
  talked(chat('api-1', 'api-project', 2));
  const { root, dispose } = await mount('workspace:plan-lacking', [api, web]);
  const before = notifications().length;
  requestImplementPlan({ path: '/w/api/.rovik/plans/cart.md', folders: ['api', 'mobile'] });
  await flush();
  assert.deepEqual(sent('chat_create_dialog'), []);
  assert.deepEqual(implementations(root), [], 'nothing sent');
  const said = notifications()
    .slice(before)
    .map((n) => n.message);
  assert.deepEqual(said, ['The plan works in mobile, which this workspace doesn’t have.']);
  dispose();
});

test('Implement under a workspace chat’s plan carries it out there while the chat works in each folder it names, else in a new chat across them', async () => {
  const file = '/w/api/.rovik/plans/cart.md';
  plans[file] = '# Plan: cart\n\nFolders: api, web\n\nSummary.\n';
  const planned = (dialog: DialogSummary) => {
    talked(dialog);
    const [request, reply] = rows[dialog.id];
    request.meta = { mode: 'plan' };
    reply.meta = { planFile: file };
  };
  const implement = async (root: HTMLElement) => {
    const button = [...root.querySelectorAll<HTMLButtonElement>('.chat-plan-actions button')].find(
      (b) => b.textContent === 'Implement this plan'
    );
    assert.ok(button, 'Implement this plan');
    button.click();
    await flush();
  };

  planned(workspaceChat('ws-all', 'plan-here', 'all'));
  const here = await mount('workspace:plan-here', [api, web]);
  await implement(here.root);
  assert.deepEqual(sent('chat_create_dialog'), [], 'this chat');
  assert.match(implementations(here.root)[0] ?? '', /the plan in `?api\/\.rovik\/plans\/cart\.md/);
  here.dispose();

  // Since the plan, web was unchecked: a chat in both carries it out.
  planned(workspaceChat('ws-api', 'plan-moved', '["api"]'));
  const moved = await mount('workspace:plan-moved', [api, web]);
  await implement(moved.root);
  assert.deepEqual(
    sent('chat_create_dialog').map(({ dialog }) => [dialog.projectId, dialog.folders]),
    [['plan-moved', '["api","web"]']]
  );
  moved.dispose();
});

test('a workspace chat’s run is sent no root, and the editor context names its file under its folder’s name', async () => {
  talked(workspaceChat('ws-run', 'run', '["api","web"]'));
  const { root, dispose, setContext } = await mount('workspace:run', [api, web]);
  setContext({
    path: '/w/web/src/cart.ts',
    language: 'typescript',
    line: 3,
    column: 1,
    enclosingSymbol: null,
    selection: null,
  });
  await sendText(root, 'Why is this slow?');
  const [first] = sent('agent_start');
  assert.ok(first, 'the run started');
  assert.equal(first.request.root, null, 'the backend reads its folders itself');
  assert.match(first.request.currentUserContent, /file web\/src\/cart\.ts \(typescript\)/);
  dispose();
});

test('a workspace chat with one of its folders missing doesn’t run, and says which', async () => {
  talked(workspaceChat('ws-missing', 'missing', '["api","web"]'));
  const { root, dispose } = await mount('workspace:missing', [api, { ...web, missing: true }]);
  const before = notifications().length;
  await sendText(root, 'Add the cart');
  assert.deepEqual(sent('agent_start'), []);
  assert.deepEqual(
    notifications()
      .slice(before)
      .map((n) => n.message),
    ['web isn’t there: locate it, or remove it from the workspace, to go on with this chat.']
  );
  dispose();
});

test('@ lists the files of each of a workspace chat’s folders, under their names', async () => {
  talked(workspaceChat('ws-at', 'at', '["api","web"]'));
  projectFiles = {
    '/w/api': ['/w/api/routes.ts'],
    '/w/web': ['/w/web/src/cart.ts'],
    '/w/docs': ['/w/docs/readme.md'],
  };
  try {
    const { root, dispose } = await mount('workspace:at', [api, web, folder('docs')]);
    await type(root, '@');
    const shown = [...root.querySelectorAll('.file-suggestion-path')].map((p) => p.textContent);
    assert.deepEqual(shown, ['./api/routes.ts', './web/src/cart.ts']);
    assert.deepEqual(
      sent('list_project_files').map((a) => a.root),
      ['/w/api', '/w/web'],
      'not docs: the chat doesn’t work there'
    );
    dispose();
  } finally {
    projectFiles = {};
  }
});

test('Add to chat from either of a workspace chat’s folders names the file under its folder’s name, and another folder’s stays out', async () => {
  talked(workspaceChat('ws-add', 'add', '["api","web"]'));
  const { root, dispose } = await mount('workspace:add', [api, web, folder('docs')]);
  addToChat(reference('/w/web/src/cart.ts'));
  addToChat(reference('/w/api/routes.ts'));
  await flush();
  const chips = () =>
    [...root.querySelectorAll<HTMLElement>('.chat-file-reference')].map(
      (chip) => JSON.parse(chip.dataset.file!).relativePath
    );
  assert.deepEqual(chips(), ['web/src/cart.ts', 'api/routes.ts']);

  addToChat(reference('/w/docs/readme.md'));
  await flush();
  assert.deepEqual(chips(), ['web/src/cart.ts', 'api/routes.ts'], 'docs’ file stays out');
  assert.equal(note(root), 'docs/readme.md is in docs; this chat works in api, web.');
  dispose();
});

test('a workspace chat’s row says the folders checked, or All folders', async () => {
  talked(workspaceChat('ws-some', 'ws-rows', '["web","api"]'), {
    ...workspaceChat('ws-all', 'ws-rows', 'all'),
    updatedAt: 1,
  });
  const { root, dispose } = await mount('workspace:ws-rows', [api, web]);
  (root.querySelector('.chat-history-button') as HTMLButtonElement).click();
  await flush();
  const listed = [...root.querySelectorAll('.chat-history-item')].map((item) => [
    item.querySelector('.chat-history-item-title')?.textContent,
    item.querySelector('.chat-history-item-folders')?.textContent,
  ]);
  assert.deepEqual(listed, [
    ['ws-some', 'api, web'],
    ['ws-all', 'All folders'],
  ]);
  dispose();
});

/** Runs `body` with the virtual-files driver installed here. */
async function withDriver(body: () => Promise<void>) {
  driverInstalled = true;
  try {
    await body();
  } finally {
    driverInstalled = false;
  }
}

test('checking api and web makes a workspace chat across them', () =>
  withDriver(async () => {
    talked(chat('api-1', 'api-project', 2));
    const { root, dispose } = await mount('workspace:across', [api, web, folder('docs')], api);
    await plus(root);
    box(root, 'web').click();
    await flush();
    assert.deepEqual(
      boxes(root).map((b) => b.getAttribute('aria-checked')),
      ['true', 'true', 'false']
    );
    assert.equal(startItem(root)?.textContent, 'Start chat in 2 folders');
    startItem(root)!.click();
    await flush();
    assert.deepEqual(
      sent('chat_create_dialog').map(({ dialog }) => [dialog.projectId, dialog.folders]),
      [['across', '["api","web"]']],
      'the workspace’s, with the names checked'
    );
    assert.equal(label(root)?.textContent, 'in api, web ▾');
    dispose();
  }));

test('checking All folders checks every box, and unchecking one unchecks it; checking every box doesn’t set it', () =>
  withDriver(async () => {
    talked(chat('api-1', 'api-project', 2));
    const { root, dispose } = await mount('workspace:all', [api, web], api);
    await plus(root);
    allBox(root)!.click();
    await flush();
    assert.equal(allBox(root)!.getAttribute('aria-checked'), 'true');
    assert.deepEqual(
      boxes(root).map((b) => b.getAttribute('aria-checked')),
      ['true', 'true']
    );
    assert.equal(startItem(root)?.textContent, 'Start chat in all folders');

    box(root, 'web').click();
    await flush();
    assert.equal(allBox(root)!.getAttribute('aria-checked'), 'false');
    assert.deepEqual(
      boxes(root).map((b) => b.getAttribute('aria-checked')),
      ['true', 'false'],
      'the rest stay checked'
    );
    // Every box checked again, by hand: the chat doesn't follow the workspace.
    box(root, 'web').click();
    await flush();
    assert.equal(allBox(root)!.getAttribute('aria-checked'), 'false');
    startItem(root)!.click();
    await flush();
    assert.deepEqual(
      sent('chat_create_dialog').map(({ dialog }) => dialog.folders),
      ['["api","web"]']
    );
    dispose();

    // All folders: the chat follows the workspace.
    talked(chat('api-1', 'api-project', 2));
    const again = await mount('workspace:all-again', [api, web], api);
    await plus(again.root);
    allBox(again.root)!.click();
    await flush();
    startItem(again.root)!.click();
    await flush();
    assert.deepEqual(
      sent('chat_create_dialog').map(({ dialog }) => [dialog.projectId, dialog.folders]),
      [['all-again', 'all']]
    );
    assert.equal(label(again.root)?.textContent, 'in all folders ▾');
    again.dispose();
  }));

test('without the driver a second box can’t be checked, and All folders says why and opens the page that says how', async () => {
  talked(chat('api-1', 'api-project', 2));
  const { root, dispose } = await mount('workspace:no-driver', [api, web], api);
  await plus(root);
  box(root, 'web').click();
  await flush();
  assert.deepEqual(
    boxes(root).map((b) => b.getAttribute('aria-checked')),
    ['false', 'true'],
    'one at a time: a folder chat works in one'
  );
  const all = allBox(root)!;
  assert.equal(all.getAttribute('aria-disabled'), 'true');
  assert.equal(
    all.querySelector('.folder-menu-why')?.textContent,
    'Needs FUSE: Settings → Virtual files'
  );
  all.click();
  await flush();
  assert.equal(all.getAttribute('aria-checked'), 'false');
  assert.equal(appSettingsPage(), 'vfs', 'Settings → Virtual files');
  assert.equal(boxes(root).length, 0, 'the menu closes');
  closeAppSettings();
  dispose();
});

test('All folders can’t be checked while a folder is missing: its chat couldn’t run', () =>
  withDriver(async () => {
    talked();
    const gone = { ...folder('docs', false), missing: true };
    const { root, dispose } = await mount('workspace:all-missing', [api, web, gone], api);
    const all = allBox(root)!;
    assert.equal(all.getAttribute('aria-disabled'), 'true');
    assert.match(all.querySelector('.folder-menu-why')?.textContent ?? '', /^docs isn’t there/);
    all.click();
    await flush();
    assert.equal(all.getAttribute('aria-checked'), 'false');
    dispose();
  }));

/** The menu's Apply. */
const applyItem = (root: ParentNode) =>
  [...root.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(
    (item) => item.textContent === 'Apply'
  );

/** Opens the workspace chat's menu from its label. */
async function openLabel(root: HTMLElement) {
  (label(root) as HTMLButtonElement).click();
  await flush();
}

test('a workspace chat’s label says its folders and opens its menu, whose Apply changes them', async () => {
  talked(workspaceChat('ws-apply', 'apply', '["api"]'));
  const { root, dispose } = await mount('workspace:apply', [api, web]);
  assert.equal(label(root)?.tagName, 'BUTTON');
  assert.equal(label(root)?.textContent, 'in api ▾');
  await openLabel(root);
  assert.deepEqual(
    boxes(root).map((b) => [b.textContent, b.getAttribute('aria-checked')]),
    [
      ['api', 'true'],
      ['web', 'false'],
    ]
  );
  box(root, 'web').click();
  await flush();
  // Every box checked by hand isn't All folders.
  assert.equal(allBox(root)!.getAttribute('aria-checked'), 'false');
  applyItem(root)!.click();
  await flush();
  assert.deepEqual(sent('chat_set_folders'), [{ dialogId: 'ws-apply', folders: '["api","web"]' }]);
  assert.equal(boxes(root).length, 0, 'the menu closes');
  assert.equal(label(root)?.textContent, 'in api, web ▾');

  // All folders: it follows the workspace again.
  await openLabel(root);
  allBox(root)!.click();
  await flush();
  applyItem(root)!.click();
  await flush();
  assert.deepEqual(sent('chat_set_folders')[1], { dialogId: 'ws-apply', folders: 'all' });
  assert.equal(label(root)?.textContent, 'in all folders ▾');
  dispose();
});

test('Apply stays off with no folder checked, and while the chat has a command running, named, with a way to stop it', async () => {
  talked(workspaceChat('ws-off', 'off', '["api","web"]'));
  const { root, dispose } = await mount('workspace:off', [api, web]);
  await openLabel(root);
  box(root, 'api').click();
  box(root, 'web').click();
  await flush();
  assert.equal(applyItem(root)!.getAttribute('aria-disabled'), 'true');
  applyItem(root)!.click();
  await flush();
  assert.deepEqual(sent('chat_set_folders'), [], 'no folder checked');
  dispose();

  talked(workspaceChat('ws-bg', 'bg', '["api","web"]'));
  const running = await mount('workspace:bg', [api, web]);
  noteBackgroundTasks('ws-bg', [{ id: 'bg1', command: 'npm run dev', startedAt: 1 }]);
  await flush();
  await openLabel(running.root);
  box(running.root, 'web').click();
  await flush();
  assert.equal(applyItem(running.root)!.getAttribute('aria-disabled'), 'true');
  assert.equal(
    running.root.querySelector('.folder-menu-note')?.textContent,
    'npm run dev is running: stop it to change the chat’s folders.'
  );
  const stop = [...running.root.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(
    (item) => item.textContent === 'Stop its commands'
  );
  stop!.click();
  await flush();
  assert.deepEqual(sent('background_stop'), [{ dialogId: 'ws-bg', id: 'bg1' }]);
  // Stopped: nothing runs, and Apply is on again.
  assert.equal(applyItem(running.root)!.getAttribute('aria-disabled'), null);
  applyItem(running.root)!.click();
  await flush();
  assert.deepEqual(sent('chat_set_folders'), [{ dialogId: 'ws-bg', folders: '["api"]' }]);
  running.dispose();
});

test('Apply stays off while the chat is at work', async () => {
  talked(workspaceChat('ws-busy', 'busy', '["api","web"]'));
  const { root, dispose } = await mount('workspace:busy', [api, web]);
  await sendText(root, 'Add the cart');
  emit({ type: 'runStarted', dialogId: 'ws-busy', runId: 'run-1', steerable: true });
  await flush();
  await openLabel(root);
  assert.equal(applyItem(root)!.getAttribute('aria-disabled'), 'true');
  assert.equal(
    root.querySelector('.folder-menu-note')?.textContent,
    'This chat is at work: its folders can change once it stops.'
  );
  dispose();
});

test('unchecking a folder with changes that haven’t landed is refused, saying which', async () => {
  talked(workspaceChat('ws-changed', 'changed', '["api","web"]'));
  const { root, dispose } = await mount('workspace:changed', [api, web]);
  foldersRefused =
    '“ws-changed” has changes in web that aren’t landed: land or discard them first.';
  try {
    await openLabel(root);
    box(root, 'web').click();
    await flush();
    const before = notifications().length;
    applyItem(root)!.click();
    await flush();
    assert.deepEqual(
      notifications()
        .slice(before)
        .map((n) => n.message),
      [foldersRefused]
    );
    assert.equal(label(root)?.textContent, 'in api, web ▾', 'its folders as they were');
  } finally {
    foldersRefused = null;
  }
  dispose();
});

test('a missing folder the chat has can be unchecked, and then not checked again', async () => {
  const gone = { ...folder('docs', false), missing: true };
  talked(workspaceChat('ws-gone', 'gone', '["api","docs"]'));
  const { root, dispose } = await mount('workspace:gone', [api, web, gone]);
  await openLabel(root);
  const docs = box(root, 'docs');
  assert.equal(docs.getAttribute('aria-checked'), 'true');
  assert.equal(docs.getAttribute('aria-disabled'), null);
  docs.click();
  await flush();
  assert.equal(box(root, 'docs').getAttribute('aria-checked'), 'false');
  assert.equal(box(root, 'docs').getAttribute('aria-disabled'), 'true');
  box(root, 'docs').click();
  await flush();
  assert.equal(box(root, 'docs').getAttribute('aria-checked'), 'false', 'not again');
  applyItem(root)!.click();
  await flush();
  assert.deepEqual(sent('chat_set_folders'), [{ dialogId: 'ws-gone', folders: '["api"]' }]);
  dispose();
});

/** The branch dialog's text and its name field, while it asks. */
const branchDialog = () => {
  const asked = document.querySelector<HTMLElement>('.confirm-dialog');
  return (
    asked && {
      text: asked.textContent?.replace(/\s+/g, ' ') ?? '',
      name: asked.querySelector<HTMLInputElement>('input.new-project-input')!.value,
      yes: () =>
        [...asked.querySelectorAll<HTMLButtonElement>('button')]
          .find((b) => b.textContent === 'Yes')!
          .click(),
    }
  );
};

test('a new workspace chat asks once about its repositories on their main branch', async () => {
  dialogs = [{ ...workspaceChat('ws-new', 'branch-new', '["api","web"]'), messageCount: 0 }];
  rows = {};
  heads = {
    '/w/api': { branch: 'main', main_branch: 'main' },
    '/w/web': { branch: 'master', main_branch: 'master' },
  };
  try {
    const { root, dispose } = await mount('workspace:branch-new', [api, web]);
    await sendText(root, 'Add the cart');
    const asked = branchDialog();
    assert.ok(asked, 'asked');
    assert.match(asked.text, /api \(main\) and web \(master\) are on their main branches/);
    assert.deepEqual(sent('agent_start'), [], 'the run waits');
    asked.yes();
    await flush();
    assert.deepEqual(
      sent('git_create_branch').map((a) => [a.path, a.name]),
      [
        ['/w/api', asked.name],
        ['/w/web', asked.name],
      ],
      'one name in each'
    );
    assert.equal(sent('agent_start').length, 1, 'then the run starts');
    dispose();
  } finally {
    heads = {};
  }
});

test('a folder checked on its repository’s main branch is offered the chat’s branch at the next send', async () => {
  talked(workspaceChat('ws-joined', 'branch-joined', '["api","web"]'));
  // Its last run worked in api alone, on the chat's branch; web joined since.
  rows['ws-joined'][0].meta = { mode: 'agent', folders: ['api'] };
  heads = {
    '/w/api': { branch: 'rovik-cart', main_branch: 'main' },
    '/w/web': { branch: 'main', main_branch: 'main' },
  };
  try {
    const { root, dispose } = await mount('workspace:branch-joined', [api, web]);
    await sendText(root, 'And the checkout');
    const asked = branchDialog();
    assert.ok(asked, 'asked');
    assert.match(asked.text, /web \(main\) is on its main branch/);
    assert.match(asked.text, /api stays on rovik-cart/);
    assert.equal(asked.name, 'rovik-cart', 'the chat’s branch');
    asked.yes();
    await flush();
    assert.deepEqual(sent('git_create_branch'), [
      { path: '/w/web', name: 'rovik-cart', checkout: true, from: null },
    ]);
    dispose();
  } finally {
    heads = {};
  }

  // Its folders as at its last run: no question.
  talked(workspaceChat('ws-same', 'branch-same', '["api","web"]'));
  rows['ws-same'][0].meta = { mode: 'agent', folders: ['api', 'web'] };
  const same = await mount('workspace:branch-same', [api, web]);
  await sendText(same.root, 'And the checkout');
  assert.equal(branchDialog(), null);
  assert.deepEqual(sent('git_head'), []);
  same.dispose();
});
