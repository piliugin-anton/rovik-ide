// Component tests: a new chat's first message, sent while the project is on
// its main branch, waits while the chat is offered a branch of its own
// (lib/chatBranch.ts, NewBranchDialog.tsx); its run starts once that's
// answered. Any other message starts its run at once. The chat panel as
// the app mounts it, the app answered here, command by command. Run with:
// npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ChatPanel from './ChatPanel';
import { NewBranchDialogHost } from './NewBranchDialog';
import type { MessageRow } from '../lib/chatApi';
import type { DialogSummary, GitHead } from '../types';

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

/** Where HEAD is in the test running now. */
let head: GitHead = { branch: 'main', main_branch: 'main' };
/** The project's chats, and their messages. */
let dialogs: DialogSummary[] = [];
let rows: MessageRow[] = [];
/** The commands the panel sent, in order. */
let calls: { cmd: string; args: any }[] = [];

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case 'load_ai_settings':
        return JSON.stringify(SETTINGS);
      // Copies: the store changes what it's given.
      case 'chat_list_dialogs':
        return { items: [...dialogs], nextCursor: null };
      case 'chat_get_messages':
        return { messages: [...rows], hasOlder: false, hasNewer: false };
      case 'agent_status':
        return { runs: [], approvals: [], questions: [] };
      case 'background_tasks':
      case 'chat_unsettled_sends':
        return [];
      case 'git_head':
        return head;
      case 'agent_start':
        return 'run-1';
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
const dialog = () => document.querySelector('.confirm-dialog') as HTMLElement | null;

function button(label: string): HTMLButtonElement {
  const found = [...dialog()!.querySelectorAll<HTMLButtonElement>('button')].find(
    (b) => b.textContent === label
  );
  assert.ok(found, `no button ${label}`);
  return found;
}

/**
 * The panel on a project of its own, with `text` sent: the test before's
 * send, never answered, isn't recovered into its input.
 */
async function mountAndSend(projectId: string, text: string) {
  calls = [];
  localStorage.clear();
  const root = container();
  const dispose = render(
    () => (
      <>
        <ChatPanel
          rootPath={() => '/p'}
          activeFileContext={() => null}
          files={() => []}
          projectId={() => projectId}
        />
        <NewBranchDialogHost />
      </>
    ),
    root
  );
  await flush();
  const input = root.querySelector('.chat-input-contenteditable') as HTMLElement;
  input.textContent = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
  (root.querySelector('.chat-send-button') as HTMLButtonElement).click();
  await flush();
  return dispose;
}

test('a new chat’s first message waits for the answer, and Yes starts its run on the new branch', async () => {
  head = { branch: 'main', main_branch: 'main' };
  const dispose = await mountAndSend('project-yes', 'Fix the build');
  assert.ok(dialog(), 'asked');
  assert.deepEqual(sent('agent_start'), [], 'the run waits');
  const name = (dialog()!.querySelector('input.new-project-input') as HTMLInputElement).value;
  button('Yes').click();
  await flush();
  assert.deepEqual(sent('git_create_branch'), [{ path: '/p', name, checkout: true, from: null }]);
  assert.equal(sent('agent_start').length, 1, 'then it starts');
  const at = (cmd: string) => calls.findIndex((c) => c.cmd === cmd);
  assert.ok(at('git_create_branch') < at('agent_start'), 'on the new branch');
  dispose();
});

test('No starts the run where the project is', async () => {
  head = { branch: 'main', main_branch: 'main' };
  const dispose = await mountAndSend('project-no', 'Fix the build');
  button('No').click();
  await flush();
  assert.deepEqual(sent('git_create_branch'), []);
  assert.equal(sent('agent_start').length, 1);
  dispose();
});

test('a project on another branch than its main one starts the run at once', async () => {
  head = { branch: 'feature', main_branch: 'main' };
  const dispose = await mountAndSend('project-feature', 'Fix the build');
  assert.equal(dialog(), null, 'not asked');
  assert.deepEqual(sent('git_head'), [{ path: '/p' }]);
  assert.equal(sent('agent_start').length, 1);
  dispose();
});

test('a message to a chat that has messages already starts its run at once', async () => {
  head = { branch: 'main', main_branch: 'main' };
  const now = Date.now();
  dialogs = [
    {
      id: 'chat-1',
      title: 'Earlier work',
      createdAt: now,
      updatedAt: now,
      messageCount: 2,
      preview: 'Done.',
    },
  ];
  const row = (id: string, seq: number, role: 'user' | 'assistant', content: string) => ({
    id,
    dialogId: 'chat-1',
    seq,
    role,
    content,
    bodyExternal: false,
    bodyBytes: content.length,
    toolCalls: null,
    toolCallId: null,
    meta: null,
    error: null,
    failed: false,
    finalized: true,
    attachments: [],
    createdAt: now,
  });
  rows = [row('m1', 1, 'user', 'Add a test'), row('m2', 2, 'assistant', 'Done.')];
  try {
    const dispose = await mountAndSend('project-earlier', 'And another');
    assert.equal(dialog(), null, 'not asked');
    assert.deepEqual(sent('git_head'), [], 'git isn’t even read');
    assert.equal(sent('agent_start').length, 1);
    dispose();
  } finally {
    dialogs = [];
    rows = [];
  }
});
