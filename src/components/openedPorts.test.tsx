// Component test: what a chat has running shows under its title row: the
// ports a remote chat's servers opened (D20), the server's own port each,
// on one button that opens its page in a browser tab of the editor, or, as
// its ▾ chooses for every port, copies the address on this computer that
// leads to it (the port forwarded here), a port whose pages no tab can show
// copying; and the commands its agent left running, counted, which open
// into a list with how long each has run and a Stop for each. Hidden while
// the chat has neither. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import OpenedPorts from './OpenedPorts';
import { initCloud } from '../lib/cloud';
import { setPageOpener } from '../lib/browserTabs';
import type { BackgroundTask } from '../lib/backgroundTasks';

/** What each chat's agent left running, where it runs, and the Stops it got. */
const running: Record<string, BackgroundTask[]> = {};
const stops: [string, string][] = [];
/** The chats whose host is away: asking what they run fails. */
const away = new Set<string>();
/** A task started `ago` ms before now. */
const task = (id: string, command: string, ago: number): BackgroundTask => ({
  id,
  command,
  startedAt: Date.now() - ago,
});

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
    if (cmd === 'hosts_summaries') return [];
    if (cmd === 'background_tasks') {
      if (away.has(args.dialogId)) throw "The chat's host isn't connected.";
      return running[args.dialogId] ?? [];
    }
    if (cmd === 'background_stop') {
      if (args.id === 'stuck') throw "The host isn't reachable.";
      stops.push([args.dialogId, args.id]);
      const before = running[args.dialogId] ?? [];
      running[args.dialogId] = before.filter((t) => t.id !== args.id);
      return before.length !== running[args.dialogId].length;
    }
    // Forwarded before this window listened.
    if (cmd === 'cloud_forwarded_ports') return { earlier: [{ port: 8000, localPort: 8000 }] };
    return null;
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

function emit(payload: unknown) {
  for (const l of listeners.filter((l) => l.event === 'cloud-event')) {
    callbacks.get(l.handler)!({ event: 'cloud-event', id: 1, payload });
  }
}

const opened: string[] = [];
setPageOpener((address) => opened.push(address));

let copied = '';
Object.defineProperty(navigator, 'clipboard', {
  configurable: true,
  value: {
    writeText: async (text: string) => {
      copied = text;
    },
  },
});

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const numbers = (root: HTMLElement) =>
  [...root.querySelectorAll('.opened-port-number')].map((n) => n.textContent);
const mains = (root: HTMLElement) => [
  ...root.querySelectorAll<HTMLButtonElement>('.opened-port-main'),
];
const picks = (root: HTMLElement) => [
  ...root.querySelectorAll<HTMLButtonElement>('.opened-port-pick'),
];
const labels = (root: HTMLElement) => mains(root).map((b) => b.getAttribute('aria-label'));
const items = () => [
  ...document.querySelectorAll<HTMLElement>('.opened-port-menu [role="menuitemradio"]'),
];
const press = (key: string) =>
  document.activeElement!.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));

/** The ports of chat c1, rendered: its servers' on 3000 (free here) and on 5173 (5174 here). */
async function shown() {
  await initCloud();
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="c1" />, root);
  emit({
    type: 'ports',
    dialogId: 'c1',
    ports: [
      { port: 3000, localPort: 3000, tabPort: 40001 },
      { port: 5173, localPort: 5174, tabPort: 40002 },
    ],
  });
  emit({ type: 'ports', dialogId: 'other', ports: [{ port: 8080, localPort: 8080 }] });
  return { root, dispose };
}

test('a chat shows the ports its servers opened, each on a button that opens its page in a tab', async () => {
  await initCloud();
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="c1" />, root);
  emit({ type: 'ports', dialogId: 'c1', ports: [] });
  assert.equal(root.querySelector('.opened-ports'), null, 'hidden without ports');
  dispose();

  const { root: row, dispose: done } = await shown();
  assert.equal(row.querySelector('.opened-ports-label')?.textContent, 'Opened ports:');
  assert.deepEqual(numbers(row), ['3000', '5173'], 'the server’s own ports, this chat’s only');
  assert.deepEqual(labels(row), ['Open localhost:3000 in a tab', 'Open localhost:5174 in a tab']);
  assert.equal(picks(row).length, 2, 'one ▾ each');

  opened.length = 0;
  mains(row)[1].click();
  assert.deepEqual(opened, ['http://localhost:5174/'], 'at the port forwarded here');
  done();
});

test('the ▾ chooses Copy for every port, which copies the address forwarded here, remembered', async () => {
  const { root, dispose } = await shown();
  const pick = picks(root)[1];
  assert.equal(pick.getAttribute('aria-haspopup'), 'menu');
  pick.click();
  assert.equal(pick.getAttribute('aria-expanded'), 'true');
  assert.deepEqual(
    items().map((i) => [
      i.querySelector('.context-menu-label')!.textContent,
      i.getAttribute('aria-checked'),
    ]),
    [
      ['Open in a tab', 'true'],
      ['Copy localhost:5174', 'false'],
    ]
  );
  assert.equal(document.activeElement, items()[0], 'what it does now has the focus');

  items()[1].click();
  await tick();
  assert.equal(copied, 'localhost:5174', 'the port forwarded here, its own being taken');
  assert.equal(document.querySelector('.opened-port-menu'), null, 'closed once picked');
  assert.equal(localStorage.getItem('rovik.openedPorts.action'), 'copy');
  assert.deepEqual(labels(root), ['Copy localhost:3000', 'Copied']);

  opened.length = 0;
  mains(root)[0].click();
  await tick();
  assert.equal(copied, 'localhost:3000', 'every port copies now');
  assert.deepEqual(opened, []);

  // Back to Open, from the keyboard.
  picks(root)[0].click();
  assert.equal(document.activeElement, items()[1]);
  press('ArrowDown');
  assert.equal(document.activeElement, items()[0], 'around from the end');
  press('Enter');
  assert.deepEqual(opened, ['http://localhost:3000/']);
  assert.equal(localStorage.getItem('rovik.openedPorts.action'), 'open');
  assert.equal(labels(root)[1], 'Open localhost:5174 in a tab');
  dispose();
});

test('a port whose pages no tab can show copies, its Open not to be picked', async () => {
  await initCloud();
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="https" />, root);
  emit({ type: 'ports', dialogId: 'https', ports: [{ port: 8443, localPort: 8443 }] });
  assert.deepEqual(labels(root), ['Copy localhost:8443']);

  picks(root)[0].click();
  const [open, copy] = items();
  assert.equal(open.getAttribute('aria-disabled'), 'true');
  assert.equal(copy.getAttribute('aria-checked'), 'true');
  opened.length = 0;
  open.click();
  assert.deepEqual(opened, [], 'not opened');
  assert.ok(document.querySelector('.opened-port-menu'), 'still open');
  assert.equal(localStorage.getItem('rovik.openedPorts.action'), 'open', 'the choice unchanged');

  press('Escape');
  assert.equal(document.querySelector('.opened-port-menu'), null);
  assert.equal(document.activeElement, picks(root)[0], 'back on its ▾');
  dispose();
});

test('a press elsewhere closes the menu, and a closed port goes', async () => {
  const { root, dispose } = await shown();
  picks(root)[0].click();
  assert.ok(document.querySelector('.opened-port-menu'));
  document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
  assert.equal(document.querySelector('.opened-port-menu'), null);

  emit({ type: 'ports', dialogId: 'c1', ports: [{ port: 5173, localPort: 5174, tabPort: 40002 }] });
  assert.deepEqual(numbers(root), ['5173'], 'a closed port goes');
  emit({ type: 'ports', dialogId: 'c1', ports: [] });
  assert.equal(root.querySelector('.opened-ports'), null, 'hidden once none is left');
  dispose();
});

test('ports forwarded before the window listened show too', async () => {
  await initCloud();
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="earlier" />, root);
  assert.deepEqual(numbers(root), ['8000']);
  dispose();
});

const commands = (root: HTMLElement) =>
  [...root.querySelectorAll('.background-task-command')].map((c) => c.textContent);

test('a chat with background tasks shows how many, with its ports when it has some', async () => {
  await initCloud();
  running.t1 = [task('bg1', 'npm run dev', 90_000), task('bg2', 'npm run watch:css', 5_500)];
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="t1" />, root);
  await tick();
  assert.equal(root.querySelector('.opened-ports-label')?.textContent, 'Background tasks: 2');
  assert.equal(root.querySelector('.background-tasks'), null, 'closed at first');

  emit({ type: 'ports', dialogId: 't1', ports: [{ port: 5173, localPort: 5173, tabPort: 40003 }] });
  assert.equal(
    root.querySelector('.opened-ports-label')?.textContent,
    'Background tasks: 2, open ports:'
  );
  assert.deepEqual(numbers(root), ['5173'], 'its ports on their buttons, as without tasks');
  assert.deepEqual(labels(root), ['Open localhost:5173 in a tab']);
  dispose();
});

test('the row opens into the tasks, how long each has run, and a Stop for each', async () => {
  await initCloud();
  running.t2 = [task('bg1', 'npm run dev', 90_000), task('bg2', 'npm run watch:css', 5_500)];
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="t2" />, root);
  await tick();
  const toggle = root.querySelector<HTMLButtonElement>('.background-tasks-toggle')!;
  assert.equal(toggle.getAttribute('aria-expanded'), 'false');
  toggle.click();
  assert.equal(toggle.getAttribute('aria-expanded'), 'true');
  assert.deepEqual(commands(root), ['npm run dev', 'npm run watch:css'], 'oldest first');
  assert.deepEqual(
    [...root.querySelectorAll('.background-task-ran')].map((r) => r.textContent),
    ['2 min', '6 s']
  );
  const stopsOf = () => [...root.querySelectorAll<HTMLButtonElement>('.background-task-stop')];
  assert.deepEqual(
    stopsOf().map((b) => [b.textContent, b.getAttribute('aria-label')]),
    [
      ['Stop', 'Stop npm run dev'],
      ['Stop', 'Stop npm run watch:css'],
    ]
  );

  stopsOf()[0].click();
  await tick();
  await tick();
  assert.deepEqual(stops, [['t2', 'bg1']], 'where the chat runs');
  assert.deepEqual(commands(root), ['npm run watch:css'], 'gone once stopped');
  assert.equal(root.querySelector('.opened-ports-label')?.textContent, 'Background tasks: 1');

  stopsOf()[0].click();
  await tick();
  await tick();
  assert.equal(root.querySelector('.opened-ports'), null, 'hidden once nothing runs');
  dispose();
});

test('a Stop that fails says why, and may be tried again', async () => {
  await initCloud();
  running.t3 = [task('stuck', 'npm run dev', 1_000)];
  const root = container();
  const dispose = render(() => <OpenedPorts dialogId="t3" />, root);
  await tick();
  root.querySelector<HTMLButtonElement>('.background-tasks-toggle')!.click();
  const stop = root.querySelector<HTMLButtonElement>('.background-task-stop')!;
  stop.click();
  assert.equal(stop.textContent, 'Stopping…');
  assert.ok(stop.disabled);
  await tick();
  await tick();
  assert.equal(
    root.querySelector('.background-task-failed')?.textContent,
    "It didn't stop: The host isn't reachable."
  );
  assert.equal(stop.textContent, 'Stop');
  assert.ok(!stop.disabled);
  assert.deepEqual(commands(root), ['npm run dev'], 'still there');
  dispose();
});

test('a chat whose host was away as it opened asks again once a host catches up', async () => {
  await initCloud();
  running.t4 = [task('bg1', 'npm run dev', 1_000)];
  away.add('t4');
  const root = container();
  const original = console.error;
  console.error = () => {};
  try {
    const dispose = render(() => <OpenedPorts dialogId="t4" />, root);
    await tick();
    assert.equal(root.querySelector('.opened-ports'), null, 'its host couldn’t say');
    away.delete('t4');
    emit({ type: 'caughtUp', host: 'h-t4' });
    await tick();
    assert.equal(root.querySelector('.opened-ports-label')?.textContent, 'Background tasks: 1');
    dispose();
  } finally {
    console.error = original;
  }
});
