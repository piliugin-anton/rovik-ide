// Component tests: Settings → Restricted files: the files found whose names
// say they hold credentials, both boxes ticked, the user's own entries,
// and every change saved for the project at once. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import RestrictedFilesSettings from './RestrictedFilesSettings';
import type { RestrictedFile, RestrictedFileRow } from '../lib/restrictedFiles';

/** The project's saved settings, and the files the backend finds by name. */
let stored: any = null;
let found: string[] = [];
let failing = false;
let savesFail = false;
/** The projects tables were asked for. */
let looked: string[] = [];
/**
 * Set: each table, as the backend has it when asked, waits here until the
 * test lets it go, so a load can come back after a later change.
 */
let held: (() => void)[] | null = null;
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'load_project_settings') return stored;
    if (cmd === 'save_project_settings') {
      if (savesFail) throw new Error('the disk is full');
      stored = args.settings;
      return;
    }
    if (cmd === 'restricted_files') {
      looked.push(args.root);
      if (failing) throw new Error('the project is gone');
      const rows = table();
      if (held) return new Promise((resolve) => held!.push(() => resolve(rows)));
      return rows;
    }
    throw new Error(`no app: ${cmd}`);
  },
};

/** The table as the backend makes it: what's found, then the user's entries. */
function table(): RestrictedFileRow[] {
  const listed: RestrictedFile[] = stored?.restrictedFiles ?? [];
  const entry = (path: string) => listed.find((e) => e.path === path);
  const rows: RestrictedFileRow[] = found.map((path) => ({
    path,
    blockRead: entry(path)?.blockRead ?? true,
    blockWrite: entry(path)?.blockWrite ?? true,
    detected: true,
    listed: !!entry(path),
  }));
  for (const e of listed) {
    if (!found.includes(e.path)) rows.push({ ...e, detected: false, listed: true });
  }
  return rows.sort((a, b) => a.path.localeCompare(b.path));
}

const flush = async () => {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

async function page(options: { found?: string[]; stored?: unknown } = {}) {
  found = options.found ?? ['.env', 'config/master.key'];
  stored = options.stored ?? null;
  failing = false;
  savesFail = false;
  held = null;
  const root = container();
  render(() => <RestrictedFilesSettings root="/home/me/demo" />, root);
  await flush();
  return root;
}

/** Each row: its file, its two boxes, and whether it can be removed. */
const rows = (root: HTMLElement) =>
  [...root.querySelectorAll('tbody tr')].map((tr) => {
    const boxes = [...tr.querySelectorAll('input[type="checkbox"]')] as HTMLInputElement[];
    return {
      file: tr.querySelector('.restricted-file-name')?.textContent ?? tr.textContent,
      read: boxes[0]?.checked,
      write: boxes[1]?.checked,
      removable: !!tr.querySelector('button[aria-label^="Remove"]'),
    };
  });

const box = (root: HTMLElement, label: string) =>
  root.querySelector(`input[aria-label="${label}"]`) as HTMLInputElement;

test('restricted files: what’s found is listed with both boxes ticked, under Read and Write', async () => {
  const root = await page();
  assert.deepEqual(
    [...root.querySelectorAll('thead th')].map((th) => th.textContent),
    ['File', 'Read', 'Write', '']
  );
  assert.deepEqual(rows(root), [
    { file: '.env', read: true, write: true, removable: false },
    { file: 'config/master.key', read: true, write: true, removable: false },
  ]);
  assert.match(root.textContent!, /Restricted files in demo/);
  assert.match(root.textContent!, /A ticked box keeps the agent from reading or changing the file/);
  assert.equal(stored, null, 'opening the page saves nothing');
});

test('restricted files: unticking a box saves the file’s entry, keeping the project’s checks', async () => {
  const checks = [{ kind: 'test', command: 'npm test', enabled: true }];
  const root = await page({ stored: { checks } });
  box(root, 'Keep the agent from reading .env').click();
  await flush();
  assert.deepEqual(stored, {
    checks,
    restrictedFiles: [{ path: '.env', blockRead: false, blockWrite: true }],
  });
  assert.deepEqual(rows(root)[0], { file: '.env', read: false, write: true, removable: false });

  box(root, 'Keep the agent from changing .env').click();
  await flush();
  assert.deepEqual(stored.restrictedFiles, [{ path: '.env', blockRead: false, blockWrite: false }]);
  assert.deepEqual(stored.checks, checks);
});

test('restricted files: a box stays where it is, focus and all, once its change is saved', async () => {
  const root = await page();
  const read = box(root, 'Keep the agent from reading .env');
  read.focus();
  read.click();
  await flush();
  assert.equal(box(root, 'Keep the agent from reading .env'), read, 'the same box');
  assert.equal(document.activeElement, read);
  assert.equal(read.checked, false);
});

test('restricted files: a file the user adds is kept from the agent both ways, and can be removed', async () => {
  const root = await page();
  const input = root.querySelector('.restricted-file-add input') as HTMLInputElement;
  input.value = './docs/private notes.md';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  (root.querySelector('.restricted-file-add') as HTMLFormElement).dispatchEvent(
    new Event('submit', { bubbles: true, cancelable: true })
  );
  await flush();
  assert.deepEqual(stored, {
    restrictedFiles: [{ path: 'docs/private notes.md', blockRead: true, blockWrite: true }],
  });
  const added = rows(root).find((r) => r.file === 'docs/private notes.md')!;
  assert.deepEqual(added, {
    file: 'docs/private notes.md',
    read: true,
    write: true,
    removable: true,
  });
  assert.match(root.textContent!, /added by you/);

  (root.querySelector('button[aria-label="Remove docs/private notes.md"]') as HTMLElement).click();
  await flush();
  assert.equal(stored, null, 'the list is empty again: the settings are removed');
  assert.equal(
    rows(root).some((r) => r.file === 'docs/private notes.md'),
    false
  );
});

test('restricted files: a path outside the project, or one listed already, isn’t added', async () => {
  const root = await page();
  const input = root.querySelector('.restricted-file-add input') as HTMLInputElement;
  const submit = (value: string) => {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    (root.querySelector('.restricted-file-add') as HTMLFormElement).dispatchEvent(
      new Event('submit', { bubbles: true, cancelable: true })
    );
  };
  submit('../other/.env');
  await flush();
  assert.match(root.textContent!, /Give the path of a file or folder in the project/);
  submit('.env');
  await flush();
  assert.match(root.textContent!, /\.env is in the list already\./);
  assert.equal(stored, null, 'nothing saved');
});

test('restricted files: none found says so; a project that can’t be read says why', async () => {
  const empty = await page({ found: [] });
  assert.match(empty.querySelector('tbody')!.textContent!, /No files that look like credentials/);

  found = [];
  failing = true;
  const root = container();
  render(() => <RestrictedFilesSettings root="/home/me/gone" />, root);
  await flush();
  assert.match(root.textContent!, /Couldn't look at the project: Error: the project is gone/);
});

test('restricted files: a change shown isn’t undone by a table that comes back late, nor lost by the next', async () => {
  const root = await page();
  held = [];
  box(root, 'Keep the agent from reading .env').click();
  await flush();
  box(root, 'Keep the agent from reading config/master.key').click();
  await flush();
  // The table asked for after the first change comes back now, older than
  // the second: it isn't shown.
  held.shift()!();
  await flush();
  assert.equal(box(root, 'Keep the agent from reading config/master.key').checked, false);
  // A third change keeps the second, whatever the table showed.
  box(root, 'Keep the agent from changing .env').click();
  await flush();
  const entry = (path: string) => stored.restrictedFiles.find((e: any) => e.path === path);
  assert.deepEqual(entry('config/master.key'), {
    path: 'config/master.key',
    blockRead: false,
    blockWrite: true,
  });
  assert.deepEqual(entry('.env'), { path: '.env', blockRead: false, blockWrite: false });
  while (held.length > 0) held.shift()!();
  await flush();
  assert.deepEqual(rows(root), [
    { file: '.env', read: false, write: false, removable: false },
    { file: 'config/master.key', read: false, write: true, removable: false },
  ]);
});

test('restricted files: a change that can’t be saved says so, and the table shows what’s saved', async () => {
  const root = await page();
  savesFail = true;
  box(root, 'Keep the agent from reading .env').click();
  await flush();
  assert.match(root.textContent!, /Couldn't save: Error: the disk is full/);
  assert.equal(box(root, 'Keep the agent from reading .env').checked, true);
  assert.equal(stored, null);
});

test('restricted files: another project’s table is shown when it opens', async () => {
  found = ['.env'];
  stored = null;
  failing = false;
  savesFail = false;
  held = null;
  looked = [];
  const [project, setProject] = createSignal('/home/me/one');
  const root = container();
  render(() => <RestrictedFilesSettings root={project()} />, root);
  await flush();
  assert.match(root.textContent!, /Restricted files in one/);
  found = ['keys/deploy.pem'];
  setProject('/home/me/two');
  await flush();
  assert.deepEqual(looked, ['/home/me/one', '/home/me/two']);
  assert.match(root.textContent!, /Restricted files in two/);
  assert.deepEqual(
    rows(root).map((r) => r.file),
    ['keys/deploy.pem']
  );
});

test('restricted files: a path with brackets in its names is added', async () => {
  const root = await page();
  const input = root.querySelector('.restricted-file-add input') as HTMLInputElement;
  input.value = 'app/[id]/secret.json';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  (root.querySelector('.restricted-file-add') as HTMLFormElement).dispatchEvent(
    new Event('submit', { bubbles: true, cancelable: true })
  );
  await flush();
  assert.ok(
    rows(root).some((r) => r.file === 'app/[id]/secret.json'),
    root.textContent!
  );
});

test('restricted files: a pattern isn’t added, as the list matches paths', async () => {
  const root = await page();
  const input = root.querySelector('.restricted-file-add input') as HTMLInputElement;
  input.value = '*.pem';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  (root.querySelector('.restricted-file-add') as HTMLFormElement).dispatchEvent(
    new Event('submit', { bubbles: true, cancelable: true })
  );
  await flush();
  assert.match(root.textContent!, /a pattern such as \*\.pem isn’t matched/);
  assert.equal(stored, null, 'nothing saved');
});
