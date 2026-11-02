// Component test: the composer's `@` file list follows the picked row. The
// list is a scroll box a few rows tall (styles.css), and the arrow keys only
// moved the pick: past the rows on screen it walked out of sight and the keys
// looked dead, so the composer now asks for the picked row to be scrolled
// into view. happy-dom has no layout, so the test records that request
// (test/scroll.ts) instead of measuring the box. Run with: npm test
import { container } from '../test/dom';
import { recordScrollIntoView } from '../test/scroll';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ChatPanel from './ChatPanel';

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

// More files than the box shows, so the pick has to walk past its bottom.
const FILES = Array.from({ length: 40 }, (_, i) => `src/file-${String(i + 1).padStart(2, '0')}.ts`);

const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = callbacks.size + 1;
    callbacks.set(id, callback);
    return id;
  },
  invoke: async (cmd: string, args: any) => {
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
      case 'git_head':
        return { branch: 'work', main_branch: 'main' };
      case 'list_project_files':
        return { files: FILES, truncated: false };
      default:
        return null;
    }
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

const flush = async () => {
  for (let i = 0; i < 10; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

/** Types `text` into the composer, the caret after it. */
async function type(root: HTMLElement, text: string) {
  const input = root.querySelector('.chat-input-contenteditable') as HTMLElement;
  input.textContent = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

/** Presses a key in the composer. */
async function press(root: HTMLElement, key: string) {
  const input = root.querySelector('.chat-input-contenteditable') as HTMLElement;
  input.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
  await flush();
}

const rows = (root: HTMLElement) => [
  ...root.querySelectorAll<HTMLElement>('.file-suggestions .file-suggestion'),
];

test('the @ list scrolls each picked row into view, and back to the first for a new query', async () => {
  localStorage.clear();
  const root = container();
  const scrolls = recordScrollIntoView();
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

    // "@" offers the project's files, the first row picked.
    await type(root, '@');
    assert.equal(rows(root).length, FILES.length, 'every file is offered');
    assert.ok(scrolls.revealed.at(-1) === rows(root)[0], 'the first row is in view');

    // Each arrow press brings its row into view, past the box's bottom.
    for (let i = 1; i <= 12; i++) {
      await press(root, 'ArrowDown');
      const picked = rows(root)[i];
      assert.ok(picked.classList.contains('selected'), `row ${i} is picked`);
      assert.ok(scrolls.revealed.at(-1) === picked, `row ${i} is scrolled into view`);
    }

    // A query narrows the list, which starts again at its first row.
    await type(root, '@file-1');
    assert.ok(rows(root).length > 0 && rows(root).length < FILES.length, 'fewer rows match');
    assert.ok(rows(root)[0].classList.contains('selected'), 'the first row is picked again');
    assert.ok(scrolls.revealed.at(-1) === rows(root)[0], 'and back in view');
  } finally {
    dispose();
    scrolls.restore();
  }
});
