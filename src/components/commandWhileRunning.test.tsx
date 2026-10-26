// Component test: a connector's command picked from the composer's `/` menu
// (`/compact`) while the chat's run works. Claude Code runs a command only
// as a message of its own: sent to the run, it reached Claude Code inside
// the run's next turn as words, which it answered ("I can't run /compact
// from here") instead of compacting. So it waits, queued, and goes as a new
// message, a command, once the run has ended. The chat panel as the app
// mounts it, the app answered here, command by command. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ChatPanel from './ChatPanel';
import { LATE_LABEL } from '../lib/steering';

const SETTINGS = {
  version: 2,
  providers: [
    {
      id: 'cc',
      name: 'Claude Code',
      baseUrl: 'connector://claude-code',
      apiKey: '',
      stream: true,
      connector: 'claude-code',
      settings: {},
      models: [{ id: 'haiku', enabled: true }],
    },
  ],
  active: { providerId: 'cc', modelId: 'haiku' },
};

/** The commands the panel sent, in order. */
const calls: { cmd: string; args: any }[] = [];
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
      case 'chat_list_dialogs':
        return { items: [], nextCursor: null };
      case 'chat_get_messages':
        return { messages: [], hasOlder: false, hasNewer: false };
      case 'agent_status':
        return { runs: [], approvals: [], questions: [] };
      case 'background_tasks':
      case 'chat_unsettled_sends':
        return [];
      // Not on main: no branch is offered for the new chat.
      case 'git_head':
        return { branch: 'work', main_branch: 'main' };
      case 'connector_commands':
        return [{ name: 'compact', description: 'Free up context', builtin: true }];
      case 'agent_start':
        return 'run-1';
      // As a run that takes messages answers one.
      case 'agent_steer':
        return { kind: 'queued' };
      case 'send_chat_message':
        // The chat's title: no model here.
        throw new Error('no model here');
      default:
        return null;
    }
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
// happy-dom has no `execCommand`: the composer inserts a picked command its
// own way, as where the browser's command does nothing.
(document as any).execCommand = () => false;

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

/** Types `text` into the composer, the caret after it. */
async function type(root: HTMLElement, text: string) {
  const input = root.querySelector('.chat-input-contenteditable') as HTMLElement;
  input.textContent = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

async function enter(root: HTMLElement) {
  const input = root.querySelector('.chat-input-contenteditable') as HTMLElement;
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  await flush();
}

test('a command picked while the run works waits for it to end, then goes as a command', async () => {
  localStorage.clear();
  const root = container();
  const dispose = render(
    () => (
      <ChatPanel
        rootPath={() => '/p'}
        activeFileContext={() => null}
        files={() => []}
        projectId={() => 'project-1'}
      />
    ),
    root
  );
  try {
    await flush();

    // A request: its run starts, and takes messages while it works.
    await type(root, 'Write a long story');
    await enter(root);
    const [first] = sent('agent_start');
    assert.ok(first, 'the run started');
    const dialogId = first.request.dialogId;
    emit({ type: 'runStarted', dialogId, runId: 'run-1', steerable: true });
    await flush();

    // `/compact`, picked from the `/` menu, sent while the run works.
    await type(root, '/comp');
    const offered = root.querySelectorAll('.command-suggestions .file-suggestion');
    assert.equal(offered.length, 1, 'the menu offers /compact');
    await enter(root);
    const composer = root.querySelector('.chat-input-contenteditable') as HTMLElement;
    assert.equal(composer.textContent?.trim(), '/compact');
    await enter(root);

    // Not the run's to read: it would reach Claude Code as words.
    assert.deepEqual(sent('agent_steer'), []);
    assert.ok(root.textContent?.includes(LATE_LABEL), 'shown queued for after the run');
    assert.equal(sent('agent_start').length, 1, 'no run of its own yet');

    // The run ends: the command goes as a new message, a command.
    emit({ type: 'runEnded', dialogId, runId: 'run-1', outcome: 'done' });
    await flush();
    const starts = sent('agent_start');
    assert.equal(starts.length, 2, 'a run of its own');
    const { message } = starts[1].request.start;
    assert.equal(message.content, '/compact');
    assert.equal(message.meta.command, true);
    assert.ok(!root.textContent?.includes(LATE_LABEL), 'no longer queued');
  } finally {
    dispose();
  }
});
