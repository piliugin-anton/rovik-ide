// chatPanel.ts
//
// The chat panel as the app mounts it, for component tests: the app's Tauri
// calls answered here (the AI settings, an empty chat, the project's files),
// and the helpers that type into its composer, press its keys and let it
// settle after its timers. Import it after ./dom, which installs happy-dom's
// globals; call installTauri before the panel is rendered.

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

/** The app's callbacks, and the events the panel listens for. */
const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];

/**
 * Answers the Tauri calls the panel makes, before it is rendered. `files` is
 * what the composer's `@` list offers: the panel is rendered with
 * `files={() => []}`, so its rows come from that command alone.
 */
export function installTauri(files: string[]) {
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
          return { files, truncated: false };
        default:
          return null;
      }
    },
  };
  (window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
}

/** Lets the panel's promises and timers run: ten turns of the event loop. */
export async function flush() {
  for (let i = 0; i < 10; i++) await new Promise((resolve) => setTimeout(resolve, 0));
}

/** The composer: the contenteditable box that grows to its maximum, then scrolls. */
export function composer(root: HTMLElement): HTMLElement {
  return root.querySelector('.chat-input-contenteditable') as HTMLElement;
}

/** The `@` file list's rows, as the panel shows them. */
export function rows(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>('.file-suggestions .file-suggestion')];
}

/**
 * A collapsed caret in `el`, `offset` child nodes in: happy-dom's Selection
 * only takes a range while it holds none, so the old ones go first.
 */
export function placeCaret(el: HTMLElement, offset = 0) {
  const at = Math.min(offset, el.childNodes.length);
  const range = document.createRange();
  range.setStart(el, at);
  range.setEnd(el, at);
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
}

/** Types `text` into the composer, the caret after it. */
export async function typeText(root: HTMLElement, text: string) {
  const el = composer(root);
  el.textContent = text;
  placeCaret(el, el.childNodes.length);
  el.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

/** Presses `key` in the composer. */
export async function press(root: HTMLElement, key: string) {
  composer(root).dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
  await flush();
}
