// Component tests: Settings → Long-term memory: the project's count, the
// checkbox that switches this project's memory off, and the way into
// "Manage memories…". The backend is answered here:
// `load_project_settings`, `save_project_settings`, `memory_stats` and the
// manager's `memory_list`. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import MemorySettings from './MemorySettings';

/** The project's saved settings, and the counts the store has. */
let stored: any = null;
let counts: any = { total: 0, verified: 0, provisional: 0, deprecated: 0 };
let countingFails = false;
let savesFail = false;
/** The projects whose memory was counted, and how many times a change was saved. */
let counted: string[] = [];
let saves = 0;
/** Every search the manager asked the store for. */
let listed: { root: string; query: string }[] = [];
/** Set: the count waits here until the test lets it go. */
let held: (() => void)[] | null = null;

/** What the manager lists as it opens: a small memory or two. */
const memories = () => [
  {
    id: 'm-1',
    kind: 'project fact',
    title: 'Tests run with npm test',
    content: 'The repo’s tests are node:test files.',
    files: ['package.json'],
    tags: ['checks'],
    verification: 'verified',
    evidence: 'CI run 42',
    updatedAt: Date.UTC(2026, 10, 6, 9),
  },
  {
    id: 'm-2',
    kind: 'procedure',
    title: 'Format with prettier',
    content: 'npx prettier --write <files>.',
    files: [],
    tags: ['style'],
    verification: 'provisional',
    updatedAt: Date.UTC(2026, 10, 5, 9),
  },
];

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'load_project_settings') return stored;
    if (cmd === 'save_project_settings') {
      saves++;
      if (savesFail) throw new Error('the disk is full');
      stored = args.settings;
      return;
    }
    if (cmd === 'memory_stats') {
      counted.push(args.root);
      if (countingFails) throw new Error('the memory file is gone');
      if (held) return new Promise((resolve) => held!.push(() => resolve(counts)));
      return counts;
    }
    if (cmd === 'memory_list') {
      listed.push({ root: args.root, query: args.query });
      return memories();
    }
    throw new Error(`no app: ${cmd}`);
  },
};

const flush = async () => {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

/** Back to a project that never chose, with nothing held and no failure. */
function reset() {
  stored = null;
  counts = { total: 0, verified: 0, provisional: 0, deprecated: 0 };
  countingFails = false;
  savesFail = false;
  counted = [];
  saves = 0;
  listed = [];
  held = null;
}

/** Nothing an earlier test rendered is left over, portals included. */
function emptyBody() {
  while (document.body.firstChild) document.body.removeChild(document.body.firstChild);
}

async function page(options: { stored?: unknown; counts?: unknown } = {}): Promise<HTMLElement> {
  reset();
  emptyBody();
  stored = options.stored ?? null;
  counts = options.counts ?? { total: 0, verified: 0, provisional: 0, deprecated: 0 };
  const root = container();
  render(() => <MemorySettings root="/home/me/demo" />, root);
  await flush();
  return root;
}

/** The switch: "Use this project's memory in chats". */
const box = (root: HTMLElement) =>
  root.querySelector('input[aria-label="Use this project\'s memory in chats"]') as HTMLInputElement;

/** The count line. */
const count = (root: HTMLElement) => root.querySelector('.memory-count') as HTMLElement;

test('long-term memory: the project’s count, and looking saves nothing', async () => {
  const root = await page({
    counts: { total: 12, verified: 9, provisional: 2, deprecated: 1 },
  });
  assert.equal(count(root)?.getAttribute('role'), 'status', 'the count is announced');
  assert.equal(count(root).textContent, '12 memories: 9 verified, 2 provisional, 1 invalidated');
  assert.equal(box(root).checked, true, 'memory is on for a project that never chose');
  assert.deepEqual(counted, ['/home/me/demo'], 'counted for this project');
  assert.equal(saves, 0, 'opening the page saves nothing');
  assert.equal(stored, null);
  assert.doesNotMatch(root.textContent!, /Off for this project/);
});

test('long-term memory: none yet says so, and a count that can’t be read says that', async () => {
  const none = await page();
  assert.match(count(none).textContent!, /No memories yet/);

  countingFails = true;
  const root = container();
  render(() => <MemorySettings root="/home/me/gone" />, root);
  await flush();
  assert.match(count(root).textContent!, /(couldn't|could not|can't|cannot)/i);
  assert.doesNotMatch(count(root).textContent!, /No memories yet/);
  // The next render starts from a backend that answers again.
  reset();
});

test('long-term memory: Counting… until the count comes back', async () => {
  reset();
  held = [];
  counts = { total: 2, verified: 1, provisional: 1, deprecated: 0 };
  const root = container();
  render(() => <MemorySettings root="/home/me/demo" />, root);
  await flush();
  assert.ok(held.length > 0, 'the count is asked for, and still on its way');
  assert.match(count(root).textContent!, /counting/i);
  // It comes back now.
  while (held.length > 0) held.shift()!();
  await flush();
  assert.equal(count(root).textContent, '2 memories: 1 verified, 1 provisional');
  reset();
});

test('long-term memory: unticking switches the project off, and ticking again removes the setting', async () => {
  const root = await page({ counts: { total: 3, verified: 3, provisional: 0, deprecated: 0 } });
  box(root).click();
  await flush();
  assert.deepEqual(stored, { memory: false }, 'off is saved at once');
  assert.equal(box(root).checked, false);
  assert.match(root.textContent!, /Off for this project/);
  assert.equal(count(root).textContent, '3 memories: 3 verified', 'the count stays');

  box(root).click();
  await flush();
  assert.equal(box(root).checked, true);
  assert.equal(stored?.memory, undefined, 'the setting is gone: absent means on');
  assert.doesNotMatch(root.textContent!, /Off for this project/);
});

test('long-term memory: a project that switched it off opens unticked, its other settings kept', async () => {
  const checks = [{ kind: 'test', command: 'npm test', enabled: true }];
  const root = await page({
    stored: { checks, memory: false },
    counts: { total: 1, verified: 0, provisional: 1, deprecated: 0 },
  });
  assert.equal(box(root).checked, false, 'off as saved');
  assert.match(root.textContent!, /Off for this project/);
  assert.equal(saves, 0, 'opening the page saves nothing');

  box(root).click();
  await flush();
  assert.equal(box(root).checked, true);
  assert.deepEqual(stored, { checks }, 'the checks are kept, the memory key removed');
});

test('long-term memory: a switch that can’t be saved says so, and stays where it was', async () => {
  const root = await page();
  savesFail = true;
  box(root).click();
  await flush();
  assert.match(root.textContent!, /(couldn't save|failed)/i);
  assert.equal(box(root).checked, true, 'shown as saved: still on');
  assert.equal(stored, null);
});

/** The manager's dialog, wherever it was portalled (the newest one). */
function manager(): HTMLElement | null {
  const dialogs = [...document.querySelectorAll('.modal.memory-manager')] as HTMLElement[];
  return dialogs.length === 0 ? null : dialogs[dialogs.length - 1];
}

/** The page's way in: "Manage memories…" (either ellipsis). */
function manageButton(root: HTMLElement): HTMLElement {
  const button = [...root.querySelectorAll('button')].find((one) =>
    /^Manage memories/.test((one.textContent ?? '').trim())
  ) as HTMLElement | undefined;
  assert.ok(button, 'the page offers to manage this project’s memories');
  return button;
}

/** The manager's search box. */
const searchBox = () =>
  manager()?.querySelector(
    'input[aria-label="Search this project\'s memories"]'
  ) as HTMLInputElement | null;

/** The manager's × close button. */
function closeButton(): HTMLElement {
  const button = manager()?.querySelector('button[aria-label="Close"]') as HTMLElement | null;
  assert.ok(button, 'the manager can be closed');
  return button;
}

test('long-term memory: Manage memories… opens the manager for the open project', async () => {
  const root = await page({ counts: { total: 2, verified: 1, provisional: 1, deprecated: 0 } });
  assert.equal(manager(), null, 'the manager is closed to begin with');

  manageButton(root).click();
  await flush();

  const dialog = manager();
  assert.ok(dialog, 'the manager opens over the page');
  assert.equal(dialog.getAttribute('aria-modal'), 'true', 'it is a modal dialog');
  assert.equal(dialog.getAttribute('aria-label'), 'Memories in this project');
  assert.ok(searchBox(), 'the search box is there');
  assert.ok(listed.length > 0, 'the manager asks for the memories as it opens');
  for (const asked of listed) {
    assert.deepEqual(asked, { root: '/home/me/demo', query: '' }, 'this project, nothing searched');
  }
  assert.equal(
    dialog.querySelectorAll('li.memory-entry').length,
    2,
    'the memories it was given are on screen'
  );

  // Closed again, so nothing is left open for the test after this one.
  closeButton().click();
  await flush();
  assert.equal(manager(), null);
});

test('long-term memory: closing the manager counts again', async () => {
  const root = await page({ counts: { total: 2, verified: 1, provisional: 1, deprecated: 0 } });
  assert.deepEqual(counted, ['/home/me/demo'], 'counted once as the page opens');

  manageButton(root).click();
  await flush();
  const dialog = manager();
  assert.ok(dialog, 'the manager is open');
  assert.equal(counted.length, 1, 'opening the manager costs no count');

  closeButton().click();
  await flush();

  assert.equal(manager(), null, 'the manager is gone');
  assert.equal(document.body.contains(dialog), false, 'and its dialog left the page');
  assert.deepEqual(
    counted,
    ['/home/me/demo', '/home/me/demo'],
    'the count is asked for again, for the same project'
  );
});
