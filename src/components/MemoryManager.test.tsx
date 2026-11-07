// Component tests: Settings → Long-term memory → "Manage memories…"
// (MemoryManager.tsx): the project's memories listed with their state, the
// search whose pause the test holds in its own hands, editing one memory,
// deleting one, clearing them all, and the keys the dialog takes for itself.
// The backend is answered here, command by command; confirmations are drawn by
// the real ConfirmDialogHost and answered by clicking its buttons. Run with:
// npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import MemoryManager from './MemoryManager';
import { ConfirmDialogHost } from './ConfirmDialog';
import type { MemoryEntry } from '../lib/memory';

const PROJECT = '/home/me/demo';

let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};

const flush = async () => {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

// The search waits out a pause through the DOM window (src/test/dom.ts makes
// that happy-dom's window, and leaves setTimeout out of the globals it copies,
// so this pair is only the component's: the test's own flush keeps Node's).
// The test holds the component's callbacks and fires them when it says.
let held = new Map<number, () => void>();
let nextTimer = 0;
const windowTimers = {
  set: ((window as any).setTimeout as (...args: unknown[]) => number).bind(window),
  clear: ((window as any).clearTimeout as (id: number) => void).bind(window),
};

function holdWindowTimers() {
  held = new Map();
  nextTimer = 0;
  Object.defineProperty(window, 'setTimeout', {
    configurable: true,
    writable: true,
    value: (callback: () => void) => {
      nextTimer += 1;
      held.set(nextTimer, callback);
      return nextTimer;
    },
  });
  Object.defineProperty(window, 'clearTimeout', {
    configurable: true,
    writable: true,
    value: (id: number) => {
      held.delete(id);
    },
  });
}

function restoreWindowTimers() {
  Object.defineProperty(window, 'setTimeout', {
    configurable: true,
    writable: true,
    value: windowTimers.set,
  });
  Object.defineProperty(window, 'clearTimeout', {
    configurable: true,
    writable: true,
    value: windowTimers.clear,
  });
}

/** The pause the search waits out ends now. */
function fireTimers() {
  const due = [...held.values()];
  held.clear();
  for (const callback of due) callback();
}

/** A memory as the store hands one over, with the fields this test cares about. */
function memory(over: Partial<MemoryEntry>): MemoryEntry {
  return {
    id: 'm-1',
    kind: 'project fact',
    title: 'A memory',
    content: 'It says so',
    files: [],
    tags: [],
    verification: 'verified',
    applicability: 'current',
    sourceKind: 'user assertion',
    updatedAt: Date.UTC(2026, 10, 6, 9),
    ...over,
  };
}

/** The project's memories, newest first: one of each trust state. */
function project(): MemoryEntry[] {
  return [
    memory({
      id: 'm-1',
      kind: 'project fact',
      title: 'Tests run with npm test',
      content: 'The repo’s tests are node:test files.',
      files: ['src/lib/foo.test.ts', 'package.json'],
      tags: ['checks'],
      verification: 'verified',
      evidence: 'CI run 42',
      updatedAt: Date.UTC(2026, 10, 6, 9),
    }),
    memory({
      id: 'm-2',
      kind: 'procedure',
      title: 'Format with prettier',
      content: 'npx prettier --write <files>.',
      files: [],
      tags: ['style'],
      verification: 'provisional',
      updatedAt: Date.UTC(2026, 10, 5, 9),
    }),
    memory({
      id: 'm-3',
      kind: 'episode',
      title: 'The build broke on Node 18',
      content: 'The lockfile held an old esbuild.',
      verification: 'deprecated',
      updatedAt: Date.UTC(2026, 9, 1, 9),
    }),
  ];
}

/** The manager rendered with a confirm host beside it, as the app has one. */
async function mount(root = PROJECT) {
  holdWindowTimers();
  const closes: number[] = [];
  const el = container();
  const dispose = render(
    () => (
      <>
        <MemoryManager root={root} onClose={() => closes.push(1)} />
        <ConfirmDialogHost />
      </>
    ),
    el
  );
  await flush();
  return {
    el,
    closes,
    dispose: () => {
      dispose();
      el.remove();
      restoreWindowTimers();
    },
  };
}

/** The manager's dialog, wherever it was portalled. */
const dialog = () => document.querySelector('.modal.memory-manager') as HTMLElement | null;
const rows = () => [...(dialog()?.querySelectorAll('li.memory-entry') ?? [])] as HTMLElement[];
const titles = () =>
  rows().map((row) => row.querySelector('.memory-entry-title')?.textContent?.trim() ?? '');

test('the manager follows page cursors and resets them for a new search', async () => {
  calls = [];
  const first = Array.from({ length: 50 }, (_, n) =>
    memory({ id: `page-${n}`, title: `First ${n}` })
  );
  backend = {
    memory_list: ({ before, query }) =>
      query ? [] : before ? [memory({ id: 'last', title: 'Last page' })] : first,
  };
  const view = await mount();
  try {
    assert.equal(rows().length, 50);
    const panel = dialog()!;
    const list = panel.querySelector('.modal-body') as HTMLElement;
    const navigation = panel.querySelector('nav[aria-label="Memory pages"]')!;
    assert.equal(list.contains(navigation), false, 'navigation stays outside the scrolling list');
    assert.equal(list.contains(searchBox()), false, 'search stays outside the scrolling list');
    assert.match(navigation.textContent!, /Page 1/);
    list.scrollTop = 500;
    buttonIn(dialog()!, 'Next memories')!.click();
    await flush();
    assert.deepEqual(titles(), ['Last page']);
    assert.match(navigation.textContent!, /Page 2/);
    assert.equal(list.scrollTop, 0, 'the next page starts at its first memory');
    assert.equal(calls.at(-1)?.args.before, 'page-49');
    buttonIn(dialog()!, 'Previous memories')!.click();
    await flush();
    assert.equal(rows().length, 50);
  } finally {
    view.dispose();
  }
});

function rowFor(title: string): HTMLElement | undefined {
  return rows().find(
    (row) => row.querySelector('.memory-entry-title')?.textContent?.trim() === title
  );
}

/** The button in `scope` whose text or label is `label`. */
function buttonIn(scope: ParentNode, label: string): HTMLElement | undefined {
  return [...scope.querySelectorAll('button')].find(
    (button) =>
      (button.textContent ?? '').trim() === label || button.getAttribute('aria-label') === label
  ) as HTMLElement | undefined;
}

/** The whole-row action "Clear all memories…" (the ellipsis may be either). */
function clearAll(): HTMLElement {
  const button = [...(dialog()?.querySelectorAll('button') ?? [])].find((one) =>
    /^Clear all memories/.test((one.textContent ?? '').trim())
  );
  assert.ok(button, 'the manager offers to clear the project’s memories');
  return button as HTMLElement;
}

const searchBox = () =>
  dialog()!.querySelector(
    'input[aria-label="Search this project\'s memories"]'
  ) as HTMLInputElement;

/** The row being edited, its fields and its buttons. */
const editForm = () => document.querySelector('.memory-entry-edit') as HTMLElement | null;
const draftTitle = () =>
  editForm()!.querySelector('input[aria-label="Memory title"]') as HTMLInputElement;
const draftText = () =>
  editForm()!.querySelector('textarea[aria-label="Memory text"]') as HTMLTextAreaElement;

function typeInto(element: HTMLInputElement | HTMLTextAreaElement, value: string) {
  element.value = value;
  element.dispatchEvent(new Event('input', { bubbles: true }));
}

function press(target: EventTarget, key: string, init: KeyboardEventInit = {}) {
  target.dispatchEvent(
    new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init })
  );
}

/** The queries the manager asked the store for, in order. */
const askedQueries = () =>
  calls.filter((call) => call.cmd === 'memory_list').map((call) => call.args.query);

const confirmation = () => document.querySelector('.confirm-dialog') as HTMLElement | null;

function answerWith(label: string) {
  const button = [...(confirmation()?.querySelectorAll('button') ?? [])].find(
    (one) => (one.textContent ?? '').trim() === label
  );
  assert.ok(button, `the confirmation offers “${label}”`);
  (button as HTMLElement).click();
}

/** The lines announced to the reader (role="status" / role="alert"). */
function announced(role: 'status' | 'alert'): string[] {
  return [...document.querySelectorAll(`[role="${role}"]`)].map((one) =>
    (one.textContent ?? '').trim()
  );
}

test('the manager lists the project’s memories with their state, kind and date', async () => {
  const memories = project();
  calls = [];
  backend = { memory_list: () => memories };
  const view = await mount();

  const panel = dialog();
  assert.ok(panel, 'the manager opens a dialog');
  assert.equal(panel!.getAttribute('role'), 'dialog');
  assert.equal(panel!.getAttribute('aria-modal'), 'true');
  assert.equal(panel!.getAttribute('aria-label'), 'Memories in this project');
  assert.match(panel!.textContent ?? '', /Memories in this project/);
  assert.ok(buttonIn(panel!, 'Close'), 'the header has a close button');
  assert.deepEqual(
    calls.filter((call) => call.cmd === 'memory_list')[0]?.args,
    { root: PROJECT, query: '' },
    'the list is asked for this project as the dialog opens'
  );
  assert.deepEqual(titles(), [
    'Tests run with npm test',
    'Format with prettier',
    'The build broke on Node 18',
  ]);

  const [first, second, third] = rows();
  const chip = (row: HTMLElement) => row.querySelector('.prior-work-state') as HTMLElement;

  assert.ok(chip(first).classList.contains('verified'), 'the chip keeps its state class');
  assert.equal(chip(first).textContent?.trim(), 'verified');
  assert.equal(chip(first).getAttribute('title'), 'CI run 42', 'the chip shows the evidence');
  assert.ok(chip(second).classList.contains('provisional'));
  assert.equal(chip(second).textContent?.trim(), 'provisional');
  assert.ok(chip(third).classList.contains('deprecated'), 'an invalidated memory is listed too');
  assert.equal(chip(third).textContent?.trim(), 'invalidated', 'in the chat panel’s word');

  assert.equal(first.querySelector('.prior-work-kind')?.textContent?.trim(), 'project fact');
  assert.equal(second.querySelector('.prior-work-kind')?.textContent?.trim(), 'procedure');
  assert.equal(third.querySelector('.prior-work-kind')?.textContent?.trim(), 'episode');
  assert.equal(
    first.querySelector('.memory-entry-date')?.textContent?.trim(),
    new Date(Date.UTC(2026, 10, 6, 9)).toLocaleDateString()
  );
  assert.match(first.querySelector('.memory-entry-content')?.textContent ?? '', /node:test files/);
  assert.equal(
    first.querySelector('.memory-entry-files')?.textContent?.trim(),
    'src/lib/foo.test.ts, package.json'
  );
  assert.equal(third.querySelector('.memory-entry-files'), null, 'a memory with no files has none');
  assert.ok(buttonIn(first, 'Edit'), 'each row can be edited');
  assert.ok(buttonIn(first, 'Delete'), 'each row can be deleted');
  clearAll();
  view.dispose();
});

test('the manager: a memory to check again says when it was last confirmed', async () => {
  const confirmedAt = Date.UTC(2026, 10, 15, 12);
  calls = [];
  backend = {
    memory_list: () => [
      memory({
        id: 'm-1',
        title: 'Tests run with npm test',
        verification: 'verified',
        evidence: 'CI run 42',
        applicability: 'needs_revalidation',
        lastValidatedAt: confirmedAt,
      }),
      memory({
        id: 'm-2',
        title: 'Format with prettier',
        verification: 'provisional',
        applicability: 'unknown',
      }),
      memory({
        id: 'm-3',
        title: 'The API lives in src/api',
        verification: 'provisional',
        applicability: 'current',
        lastValidatedAt: confirmedAt,
      }),
    ],
  };
  const view = await mount();

  /** The applicability chip of the row named `title`, if the row carries one. */
  const caveat = (title: string) =>
    rowFor(title)?.querySelector('.prior-work-applicability') as HTMLElement | null;

  // The ordinary case: still current, so the row carries no caveat.
  assert.equal(
    caveat('The API lives in src/api')?.textContent?.trim() ?? '',
    '',
    'a current memory says nothing'
  );

  // Marked to be checked again: the chip’s word, and when the memory last held.
  const again = caveat('Tests run with npm test');
  assert.ok(again, 'a memory to be checked again carries an applicability chip');
  assert.equal(again!.textContent?.trim(), 'check again', 'in the manager’s word for it');
  assert.ok(
    again!.classList.contains('needs_revalidation'),
    'the chip keeps its applicability class'
  );
  const againTitle = again!.getAttribute('title') ?? '';
  assert.match(againTitle, /^Last confirmed /, `the tooltip says when: ${againTitle}`);
  assert.match(againTitle, /2026/, 'the date it was last confirmed');
  // The state chip stays the memory’s trust: applicability is a second question.
  assert.equal(
    rowFor('Tests run with npm test')!.querySelector('.prior-work-state')?.textContent?.trim(),
    'verified'
  );

  // Never checked by evidence: said so, and no confirmation claimed.
  const never = caveat('Format with prettier');
  assert.ok(never, 'an unchecked memory carries an applicability chip');
  assert.equal(never!.textContent?.trim(), 'unchecked');
  assert.equal(never!.getAttribute('title'), 'Never confirmed by evidence');
  view.dispose();
});

test('the manager: the search asks nothing until its pause ends, then once with what was typed', async () => {
  calls = [];
  backend = { memory_list: () => project() };
  const view = await mount();
  const fromOpening = askedQueries().length;
  assert.deepEqual(
    askedQueries().filter((query) => query !== ''),
    [],
    'only the opening asked for anything'
  );

  typeInto(searchBox(), 'ret');
  typeInto(searchBox(), 'retry');
  await flush();
  assert.deepEqual(
    askedQueries().filter((query) => query !== ''),
    [],
    'nothing is asked while the pause has not ended'
  );
  assert.ok(held.size > 0, 'the pause is waiting in a timer');

  fireTimers();
  await flush();
  assert.deepEqual(
    askedQueries().filter((query) => query !== ''),
    ['retry'],
    'one ask, with what was typed, once the pause ends'
  );
  assert.ok(askedQueries().length > fromOpening);
  view.dispose();
});

test('the manager: editing a memory saves its trimmed title and text and shows it again', async () => {
  let memories = project();
  calls = [];
  backend = {
    memory_list: () => memories,
    memory_edit: (args: any) => {
      memories = memories.map((one) =>
        one.id === args.id ? { ...one, title: args.title, content: args.content } : one
      );
      return null;
    },
  };
  const view = await mount();
  const asked = askedQueries().length;

  buttonIn(rowFor('Format with prettier')!, 'Edit')!.click();
  await flush();
  assert.ok(editForm(), 'the row turns into a form');
  assert.equal(draftTitle().value, 'Format with prettier', 'the form starts with the title');
  assert.equal(draftText().value, 'npx prettier --write <files>.', 'and with the text');

  typeInto(draftTitle(), '  Format with prettier, twice  ');
  typeInto(draftText(), '  npx prettier --write <files> && npm run format:check  ');
  buttonIn(editForm()!, 'Save')!.click();
  await flush();

  const edits = calls.filter((call) => call.cmd === 'memory_edit');
  assert.deepEqual(
    edits.map((call) => call.args),
    [
      {
        root: PROJECT,
        id: 'm-2',
        title: 'Format with prettier, twice',
        content: 'npx prettier --write <files> && npm run format:check',
      },
    ],
    'the trimmed title and text are saved'
  );
  assert.equal(editForm(), null, 'the form closes');
  assert.ok(askedQueries().length > asked, 'the list is asked for again');
  assert.ok(titles().includes('Format with prettier, twice'), 'the row shows the new title');
  assert.ok(!titles().includes('Format with prettier'));
  view.dispose();
});

test('the manager: an edit that fails says so and keeps what was typed', async () => {
  calls = [];
  backend = {
    memory_list: () => project(),
    memory_edit: () => {
      throw new Error('the memory file is locked');
    },
  };
  const view = await mount();

  buttonIn(rowFor('Format with prettier')!, 'Edit')!.click();
  await flush();
  typeInto(draftTitle(), '  Format every morning  ');
  typeInto(draftText(), '  npx prettier --write .  ');
  buttonIn(editForm()!, 'Save')!.click();
  await flush();

  const alerts = announced('alert');
  assert.ok(
    alerts.some((line) => line.length > 0),
    `a failure is announced: ${alerts.join(' | ')}`
  );
  assert.ok(editForm(), 'the form stays open');
  assert.equal(draftTitle().value.trim(), 'Format every morning', 'the title typed is kept');
  assert.equal(draftText().value.trim(), 'npx prettier --write .', 'the text typed is kept');
  view.dispose();
});

test('the manager: a blank title or text is not saved', async () => {
  calls = [];
  backend = { memory_list: () => project(), memory_edit: () => null };
  const view = await mount();

  buttonIn(rowFor('Format with prettier')!, 'Edit')!.click();
  await flush();
  typeInto(draftTitle(), '   ');
  buttonIn(editForm()!, 'Save')!.click();
  await flush();
  assert.equal(calls.filter((call) => call.cmd === 'memory_edit').length, 0, 'no blank title goes');
  assert.ok(editForm(), 'the form stays open on a blank title');
  assert.ok(
    announced('alert').some((line) => line.length > 0),
    'and says why'
  );

  typeInto(draftTitle(), 'Format with prettier');
  typeInto(draftText(), '\n  \n');
  buttonIn(editForm()!, 'Save')!.click();
  await flush();
  assert.equal(calls.filter((call) => call.cmd === 'memory_edit').length, 0, 'no blank text goes');
  assert.ok(editForm(), 'the form stays open on a blank text');
  view.dispose();
});

test('the manager: opening a memory’s form puts the focus in its title', async () => {
  calls = [];
  backend = { memory_list: () => project() };
  const view = await mount();

  // The second row, so the focus cannot be merely where it already was: the
  // button pressed is unmounted when the form opens.
  buttonIn(rowFor('Format with prettier')!, 'Edit')!.click();
  await flush();

  const titleInput = draftTitle();
  assert.ok(editForm(), 'the row turns into a form');
  assert.equal(titleInput.value, 'Format with prettier', 'it is the second memory’s form');
  assert.equal(
    document.activeElement,
    titleInput,
    'the title of the form just opened takes the focus, not the body'
  );

  // A Tab pressed there starts inside the dialog, so the rule for a press from
  // outside (put the focus on the search box) must not take it.
  press(titleInput, 'Tab');
  await flush();
  assert.notEqual(
    document.activeElement,
    searchBox(),
    'a Tab from the form does not jump to the search box'
  );
  assert.ok(
    dialog()!.contains(document.activeElement),
    'the focus stays among the dialog’s controls'
  );
  view.dispose();
});

test('the manager: deleting asks first, and cancelling keeps the memory', async () => {
  calls = [];
  backend = { memory_list: () => project(), memory_delete: () => null };
  const view = await mount();

  buttonIn(rowFor('Format with prettier')!, 'Delete')!.click();
  await flush();
  const ask = confirmation();
  assert.ok(ask, 'deleting asks first');
  assert.match(ask!.textContent ?? '', /Delete this memory\?/);
  assert.ok(
    [...ask!.querySelectorAll('button')].some((one) => (one.textContent ?? '').trim() === 'Delete'),
    'the confirmation offers Delete'
  );

  answerWith('Cancel');
  await flush();
  assert.equal(
    calls.filter((call) => call.cmd === 'memory_delete').length,
    0,
    'nothing is deleted'
  );
  assert.equal(rows().length, 3, 'the memory stays');
  assert.equal(titles().includes('Format with prettier'), true);
  view.dispose();
});

test('the manager: deleting confirmed removes the memory and lists the project again', async () => {
  let memories = project();
  calls = [];
  backend = {
    memory_list: () => memories,
    memory_delete: (args: any) => {
      memories = memories.filter((one) => one.id !== args.id);
      return null;
    },
  };
  const view = await mount();
  const asked = askedQueries().length;

  buttonIn(rowFor('Format with prettier')!, 'Delete')!.click();
  await flush();
  answerWith('Delete');
  await flush();

  assert.deepEqual(
    calls.filter((call) => call.cmd === 'memory_delete').map((call) => call.args),
    [{ root: PROJECT, id: 'm-2' }],
    'the memory asked about is the one deleted'
  );
  assert.ok(askedQueries().length > asked, 'the list is asked for again');
  assert.deepEqual(titles(), ['Tests run with npm test', 'The build broke on Node 18']);
  view.dispose();
});

test('the manager: clearing asks first and says how many went', async () => {
  let went = 3;
  calls = [];
  backend = { memory_list: () => [], memory_clear: () => went };
  const view = await mount();

  clearAll().click();
  await flush();
  const ask = confirmation();
  assert.ok(ask, 'clearing asks first');
  assert.match(ask!.textContent ?? '', /Clear this project.{0,2}s memory\?/);
  assert.ok(
    [...ask!.querySelectorAll('button')].some(
      (one) => (one.textContent ?? '').trim() === 'Clear memory'
    ),
    'the confirmation offers Clear memory'
  );

  answerWith('Cancel');
  await flush();
  assert.equal(calls.filter((call) => call.cmd === 'memory_clear').length, 0, 'nothing is cleared');
  const asked = askedQueries().length;

  clearAll().click();
  await flush();
  answerWith('Clear memory');
  await flush();
  assert.deepEqual(
    calls.filter((call) => call.cmd === 'memory_clear').map((call) => call.args),
    [{ root: PROJECT }]
  );
  assert.ok(askedQueries().length > asked, 'the list is asked for again');
  assert.ok(
    announced('status').some((line) => line.includes('Cleared 3 memories.')),
    `says how many went: ${announced('status').join(' | ')}`
  );

  went = 1;
  clearAll().click();
  await flush();
  answerWith('Clear memory');
  await flush();
  assert.ok(
    announced('status').some((line) => line.includes('Cleared 1 memory.')),
    'one memory reads as one'
  );
  view.dispose();
});

test('the manager: a search with no match says so, and an empty project says it has none', async () => {
  const memories = project();
  calls = [];
  backend = {
    memory_list: ({ query }: any) => {
      if (!query) return memories;
      const found = memories.some((one) =>
        [...[one.title, one.content], ...one.files, ...one.tags].some((text) =>
          text.includes(query)
        )
      );
      return found ? memories : [];
    },
  };
  const view = await mount();
  assert.equal(rows().length, 3);

  typeInto(searchBox(), 'zzz');
  await flush();
  fireTimers();
  await flush();
  assert.equal(rows().length, 0, 'nothing matches');
  assert.match(dialog()!.textContent ?? '', /No memories match/);
  assert.match(dialog()!.textContent ?? '', /zzz/, 'the search that matched nothing is named');

  buttonIn(dialog()!, 'Clear search')!.click();
  await flush();
  assert.equal(rows().length, 3, 'clearing the search brings them all back, without the pause');
  view.dispose();

  calls = [];
  backend = { memory_list: () => [] };
  const empty = await mount();
  assert.equal(rows().length, 0);
  assert.match(
    dialog()!.textContent ?? '',
    /No memories yet: the agent adds them as it works\./,
    'a project with no memories says so'
  );
  assert.doesNotMatch(dialog()!.textContent ?? '', /No memories match/);
  empty.dispose();
});

test('the manager: a list that can’t be read says so', async () => {
  calls = [];
  backend = {
    memory_list: () => {
      throw new Error('the memory file is gone');
    },
  };
  const view = await mount();

  const alerts = announced('alert');
  assert.ok(
    alerts.some((line) => /could.{1,2}t be read/i.test(line)),
    `a failed read is announced: ${alerts.join(' | ')}`
  );
  assert.equal(rows().length, 0);
  view.dispose();
});

test('the manager: Escape closes it after a click on a memory’s text, and a window listener never sees the key', async () => {
  calls = [];
  backend = { memory_list: () => project() };
  const view = await mount();
  let seen = 0;
  const spy = () => {
    seen += 1;
  };
  // What SettingsDialog keeps on the window behind the overlay.
  window.addEventListener('keydown', spy);

  press(searchBox(), 'Escape');
  await flush();
  assert.deepEqual(view.closes, [1], 'Escape closes the manager');
  assert.equal(seen, 0, 'the page behind never sees the key');

  // A click on a memory's text: the next key starts outside the dialog.
  (rowFor('Format with prettier')!.querySelector('.memory-entry-content') as HTMLElement).click();
  await flush();
  press(document.body, 'Escape');
  await flush();
  assert.deepEqual(view.closes, [1, 1], 'it closes wherever the press starts');
  assert.equal(seen, 0, 'the window listener the page behind would use never sees it');

  window.removeEventListener('keydown', spy);
  view.dispose();
});

test('the manager: Tab wraps at its ends, and a Tab from outside lands on the search box', async () => {
  calls = [];
  backend = { memory_list: () => project() };
  const view = await mount();
  const panel = dialog()!;
  const focusable = [
    ...panel.querySelectorAll('button, input, textarea, select, a[href], [tabindex]'),
  ].filter(
    (one) => !(one as HTMLButtonElement).disabled && one.getAttribute('tabindex') !== '-1'
  ) as HTMLElement[];
  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  assert.ok(first && last && first !== last, 'the dialog has controls to wrap between');

  last.focus();
  assert.equal(document.activeElement, last, 'the last control takes the focus');
  press(last, 'Tab');
  await flush();
  assert.equal(document.activeElement, first, 'Tab past the last control goes to the first');

  first.focus();
  press(first, 'Tab', { shiftKey: true });
  await flush();
  assert.equal(document.activeElement, last, 'Shift+Tab from the first goes to the last');

  // A Tab whose press starts outside the dialog: after a click on a memory's text.
  (rowFor('Format with prettier')!.querySelector('.memory-entry-content') as HTMLElement).click();
  await flush();
  (document.activeElement as HTMLElement | null)?.blur?.();
  press(document.body, 'Tab');
  await flush();
  assert.equal(document.activeElement, searchBox(), 'the search box takes the focus');
  view.dispose();
});

test('the manager: Escape drops the correction being typed before it closes the manager', async () => {
  calls = [];
  backend = { memory_list: () => project(), memory_edit: () => null };
  const view = await mount();

  buttonIn(rowFor('Format with prettier')!, 'Edit')!.click();
  await flush();
  assert.ok(editForm(), 'the row turns into a form');
  typeInto(draftTitle(), 'Format with prettier, always');
  await flush();

  // The press starts in the title input the correction is being typed in.
  press(draftTitle(), 'Escape');
  await flush();

  assert.equal(
    calls.filter((call) => call.cmd === 'memory_edit').length,
    0,
    'the correction is dropped, not saved'
  );
  assert.equal(editForm(), null, 'the form is gone');
  assert.ok(rowFor('Format with prettier'), 'the row shows the stored title again');
  assert.ok(!titles().includes('Format with prettier, always'), 'the typed title is not shown');
  assert.deepEqual(view.closes, [], 'closing the form does not close the manager');
  assert.ok(dialog(), 'the dialog is still in the document');

  press(searchBox(), 'Escape');
  await flush();
  assert.deepEqual(view.closes, [1], 'a second Escape closes the manager');
  view.dispose();
});

test('the manager hides pagination when all memories fit on one page', async () => {
  backend = { memory_list: () => project() };
  const view = await mount();
  try {
    assert.equal(dialog()!.querySelector('nav[aria-label="Memory pages"]'), null);
  } finally {
    view.dispose();
  }
});

test('the manager retries a failed read without closing the dialog', async () => {
  backend = {
    memory_list: () => {
      throw new Error('temporarily locked');
    },
  };
  const view = await mount();
  try {
    assert.equal(rows().length, 0);
    backend = { memory_list: () => project() };
    buttonIn(dialog()!, 'Try again')!.click();
    await flush();
    assert.equal(rows().length, 3);
    assert.equal(buttonIn(dialog()!, 'Try again'), undefined);
    assert.deepEqual(view.closes, []);
  } finally {
    view.dispose();
  }
});

test('the manager returns focus to the search field after clearing a search', async () => {
  backend = { memory_list: () => [] };
  const view = await mount();
  try {
    typeInto(searchBox(), 'nothing');
    fireTimers();
    await flush();
    const clear = buttonIn(dialog()!, 'Clear search')!;
    clear.focus();
    clear.click();
    await flush();
    assert.equal(document.activeElement, searchBox());
    assert.equal(searchBox().value, '');
  } finally {
    view.dispose();
  }
});
