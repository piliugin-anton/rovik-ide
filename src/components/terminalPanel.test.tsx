// Component tests: the terminal panel's "+" and its tabs, with cloud hosts.
// With none connected, "+" opens a terminal here; with one, it asks where
// (TerminalNewMenu): "Local", or each connected host, by mouse or by
// keyboard. A remote terminal opens through `spawn_remote_terminal`, and
// the tabs say where each runs while a host is connected or one runs on
// one. Ctrl+Shift+V pastes the clipboard once. The app is answered here,
// command by command. Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import TerminalPanel from './TerminalPanel';
import { initCloud, refreshHosts, type HostSummary } from '../lib/cloud';
import type { TerminalEvent } from '../lib/terminal';

/** How the app answers each command in the test running now. */
let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
const callbacks = new Map<number, (event: unknown) => void>();
let nextCallback = 1;
const listeners: { event: string; handler: number }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = nextCallback++;
    callbacks.set(id, callback);
    return id;
  },
  unregisterCallback: (id: number) => callbacks.delete(id),
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

/** Each terminal's channel, as it was spawned, and how many messages went on it. */
let channels: { id: number; sent: number }[] = [];

/** The backend sends `event` on the `index`th terminal's channel. */
function send(index: number, event: TerminalEvent) {
  const channel = channels[index];
  callbacks.get(channel.id)!({ index: channel.sent++, message: event });
}

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

/** What the `index`th terminal shows, once xterm has drawn it (on an animation frame). */
async function screen(root: HTMLElement, index: number): Promise<string> {
  await new Promise((resolve) => setTimeout(resolve, 50));
  const terminal = root.querySelectorAll('.terminal-instance')[index];
  return terminal?.querySelector('.xterm-rows')?.textContent ?? '';
}
const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

const BUILD: HostSummary = {
  id: 'h1',
  name: 'Build',
  deployed: true,
  state: 'ready',
  terminal: true,
};
const OFFLINE: HostSummary = {
  id: 'h2',
  name: 'Spare',
  deployed: true,
  state: 'offline',
  terminal: true,
};
const UNDEPLOYED: HostSummary = { id: 'h3', name: 'New', deployed: false, terminal: true };
/** A daemon the developer setting names by its socket: no SSH there. */
const SOCKET: HostSummary = {
  id: '/run/agentd.sock',
  name: 'agentd.sock',
  deployed: true,
  state: 'ready',
};

/** What rendered last: gone when the next one renders, its terminals with it. */
let disposeLast = () => {};
after(() => disposeLast());

/** The app's hosts are `hosts`; its terminals start bash here, zsh on a host. */
async function app(hosts: HostSummary[]) {
  disposeLast();
  disposeLast = () => {};
  calls = [];
  channels = [];
  const spawned = (shell: string) => (args: any) => {
    channels.push({ id: args.onEvent.id, sent: 0 });
    return { id: `t${channels.length}`, shell };
  };
  backend = {
    hosts_summaries: () => hosts,
    spawn_terminal: spawned('bash'),
    spawn_remote_terminal: spawned('zsh'),
    resize_terminal: () => null,
    write_to_terminal: () => null,
    close_terminal: () => null,
  };
  await initCloud();
  await refreshHosts();
}

/** The panel, open: it starts a terminal here. */
async function panel() {
  const root = container();
  const [collapsed, setCollapsed] = createSignal(false);
  disposeLast = render(
    () => (
      <TerminalPanel
        collapsed={collapsed}
        onCollapsedChange={setCollapsed}
        rootPath={() => '/work/project'}
      />
    ),
    root
  );
  await flush();
  return root;
}

const labels = (root: HTMLElement) =>
  [...root.querySelectorAll('.terminal-tab-name')].map((e) => e.textContent);
const tooltips = (root: HTMLElement) =>
  [...root.querySelectorAll('.terminal-tab')].map((e) => e.getAttribute('title'));
const plus = (root: HTMLElement) => root.querySelector<HTMLButtonElement>('.terminal-new-button')!;
const menuItems = (root: HTMLElement) => [
  ...root.querySelectorAll<HTMLElement>('.terminal-new-menu [role="menuitem"]'),
];
const itemLabel = (item: Element) => item.querySelector('.context-menu-label')!.textContent;

/** What has the focus: a menu item's label, or "+" (an element itself would be inspected whole). */
function focused(root: HTMLElement): string {
  const active = document.activeElement;
  if (active === plus(root)) return '+';
  return active?.querySelector('.context-menu-label')?.textContent ?? String(active?.tagName);
}

/** `name` pressed where the focus is. */
function press(name: string) {
  const target = document.activeElement ?? document.body;
  target.dispatchEvent(new KeyboardEvent('keydown', { key: name, bubbles: true }));
}

test('no host to open one on: "+" opens a terminal here, and the tabs say nothing of where', async () => {
  await app([OFFLINE, UNDEPLOYED, SOCKET]);
  const root = await panel();
  assert.deepEqual(labels(root), ['bash']);
  assert.equal(plus(root).getAttribute('aria-haspopup'), null);
  plus(root).click();
  await flush();
  assert.equal(root.querySelector('.terminal-new-menu'), null, 'no menu');
  assert.deepEqual(labels(root), ['bash 1', 'bash 2']);
  assert.deepEqual(
    sent('spawn_terminal').map((a) => a.cwd),
    ['/work/project', '/work/project']
  );
  assert.deepEqual(sent('spawn_remote_terminal'), []);
});

test('a connected host: "+" asks where, and each tab says where it runs', async () => {
  await app([BUILD, OFFLINE, UNDEPLOYED, SOCKET]);
  const root = await panel();
  assert.deepEqual(labels(root), ['bash (local)']);
  assert.equal(plus(root).getAttribute('aria-haspopup'), 'menu');
  assert.equal(plus(root).getAttribute('aria-expanded'), 'false');

  plus(root).click();
  await flush();
  assert.deepEqual(menuItems(root).map(itemLabel), ['Local', 'Build (remote)']);
  assert.equal(plus(root).getAttribute('aria-expanded'), 'true');
  menuItems(root)[1].click();
  await flush();
  assert.equal(root.querySelector('.terminal-new-menu'), null, 'closed once picked');
  const opened = sent('spawn_remote_terminal').map((a) => [a.host, a.cols, a.rows]);
  assert.deepEqual(opened, [['h1', 80, 24]]);
  assert.deepEqual(labels(root), ['bash (local)', 'zsh (Build (remote))']);
  assert.deepEqual(tooltips(root), ['/work/project', 'Build (remote)']);

  plus(root).click();
  await flush();
  menuItems(root)[0].click();
  await flush();
  assert.deepEqual(labels(root), ['bash 1 (local)', 'zsh (Build (remote))', 'bash 2 (local)']);
  assert.equal(sent('spawn_terminal').length, 2);
});

test('the menu takes the keyboard, and a press elsewhere closes it', async () => {
  await app([BUILD]);
  const root = await panel();
  plus(root).click();
  await flush();
  assert.equal(focused(root), 'Local', 'the first item has the focus');
  press('ArrowDown');
  assert.equal(focused(root), 'Build (remote)');
  press('ArrowDown');
  assert.equal(focused(root), 'Local', 'around from the end');
  press('ArrowUp');
  assert.equal(focused(root), 'Build (remote)');
  press('Escape');
  await flush();
  assert.equal(root.querySelector('.terminal-new-menu'), null);
  assert.equal(focused(root), '+', 'back on "+"');

  plus(root).click();
  await flush();
  press('End');
  press('Enter');
  await flush();
  assert.equal(sent('spawn_remote_terminal').length, 1);
  assert.deepEqual(labels(root), ['bash (local)', 'zsh (Build (remote))']);

  plus(root).click();
  await flush();
  assert.ok(root.querySelector('.terminal-new-menu'));
  document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
  await flush();
  assert.equal(root.querySelector('.terminal-new-menu'), null);
  plus(root).click();
  await flush();
  plus(root).click();
  await flush();
  assert.equal(root.querySelector('.terminal-new-menu'), null, '"+" again closes it');
});

test('a remote terminal says it connects until its shell says something, and when its link went', async () => {
  await app([BUILD]);
  const root = await panel();
  plus(root).click();
  await flush();
  menuItems(root)[1].click();
  await flush();
  assert.match(await screen(root, 1), /Connecting to Build…/);
  send(1, { type: 'output', data: 'me@build:~$ ' });
  const shown = await screen(root, 1);
  assert.match(shown, /me@build:~\$/);
  assert.doesNotMatch(shown, /Connecting/, 'the line goes once the shell speaks');
  send(1, { type: 'exit', code: null });
  assert.match(await screen(root, 1), /\[connection closed\]/);
  assert.deepEqual(labels(root), ['bash (local)', 'zsh (Build (remote))'], 'kept to be read');
});

test("a host that can't be reached: its terminal says so", async () => {
  await app([BUILD]);
  backend.spawn_remote_terminal = () => {
    throw "The host couldn't be reached: Build didn't answer in time.";
  };
  const root = await panel();
  plus(root).click();
  await flush();
  menuItems(root)[1].click();
  await flush();
  const shown = await screen(root, 1);
  assert.match(shown, /Couldn't open a shell on Build: The host couldn't be reached/);
  assert.doesNotMatch(shown, /Connecting/);
});

test('a host going offline: its terminal still says where it runs until it ends', async () => {
  await app([BUILD]);
  const root = await panel();
  plus(root).click();
  await flush();
  menuItems(root)[1].click();
  await flush();
  send(1, { type: 'output', data: 'build$ ' });
  emit({ type: 'host', host: 'h1', state: 'offline' });
  await flush();
  assert.deepEqual(labels(root), ['bash (local)', 'zsh (Build (remote))']);

  // Nowhere else to open one: "+" opens it here.
  plus(root).click();
  await flush();
  assert.equal(root.querySelector('.terminal-new-menu'), null);
  assert.deepEqual(labels(root), ['bash 1 (local)', 'zsh (Build (remote))', 'bash 2 (local)']);

  // Its shell exits cleanly: the tab closes, and nothing says where any more.
  send(1, { type: 'exit', code: 0 });
  await flush();
  assert.deepEqual(labels(root), ['bash 1', 'bash 2']);
});

test("Ctrl+Shift+V pastes the clipboard once: the webview's own paste doesn't follow", async () => {
  await app([]);
  const root = await panel();
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { readText: async () => 'git status', writeText: async () => {} },
  });
  try {
    const before = calls.length;
    const key = new KeyboardEvent('keydown', {
      key: 'V',
      code: 'KeyV',
      ctrlKey: true,
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    });
    root.querySelector('.terminal-instance .xterm-helper-textarea')!.dispatchEvent(key);
    await flush();
    // Linux's and Windows' webviews paste on this key themselves unless
    // its default is prevented, and xterm would send that paste too.
    assert.ok(key.defaultPrevented, "the webview's paste is kept from following");
    const written = calls.slice(before).filter((c) => c.cmd === 'write_to_terminal');
    assert.deepEqual(
      written.map((c) => c.args.data),
      ['git status']
    );
  } finally {
    delete (navigator as any).clipboard;
  }
});
