// Component test: the Files tab's search over a workspace's folders (C02) —
// every folder searched at once, its results under its own header, and a
// result opening its own folder's file. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import SearchPanel, { mergeResults } from './SearchPanel';
import { setEditorOpener } from '../lib/openInEditor';
import type { OpenFolder } from '../lib/openFolders';
import type { ToolSearchLine, ToolSearchResult } from '../types';

const api: OpenFolder = { path: '/w/api', name: 'api', missing: false };
const web: OpenFolder = { path: '/w/web', name: 'web', missing: false };

/** What the backend answers, per folder searched. */
let answers: Record<string, ToolSearchResult> = {};
/** The folders each search asked for. */
let asked: string[] = [];

(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: { root: string }) => {
    if (cmd !== 'search_in_files') throw new Error(`unexpected ${cmd}`);
    asked.push(args.root);
    const answer = answers[args.root];
    if (!answer) throw new Error(`no answer for ${args.root}`);
    return answer;
  },
};

const match = (path: string): ToolSearchLine => ({
  path,
  line: 1,
  text: 'foo',
  isMatch: true,
  ranges: [[0, 3]],
});

/** A folder's answer for these files, one match each. */
function answer(paths: string[], over: Partial<ToolSearchResult> = {}): ToolSearchResult {
  const lines = paths.map(match);
  return {
    lines,
    matchCount: lines.length,
    fileCount: paths.length,
    truncated: false,
    totalMatches: lines.length,
    totalFiles: paths.length,
    totalsComplete: true,
    ...over,
  };
}

const flush = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

function panel(folders: OpenFolder[]) {
  const el = container();
  const dispose = render(() => <SearchPanel folders={() => folders} />, el);
  /** Waits for the panel's debounce to run the search and draw it. */
  async function search(text: string) {
    const input = el.querySelector<HTMLInputElement>('.search-input');
    assert.ok(input, 'the search input');
    input.value = text;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    for (let waited = 0; waited < 3000; waited += 5) {
      if (el.querySelector('.search-file')) return;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    assert.fail('the search never drew');
  }
  const texts = (selector: string) =>
    [...el.querySelectorAll(selector)].map((node) => node.textContent);
  return { el, texts, search, dispose };
}

test("two folders' results come under their headers", async () => {
  answers = { '/w/api': answer(['src/main.ts']), '/w/web': answer(['src/app.ts']) };
  asked = [];
  const { texts, search, dispose } = panel([api, web]);
  await search('foo');
  assert.deepEqual(asked, ['/w/api', '/w/web'], 'both folders are searched at once');
  assert.deepEqual(texts('.search-folder-header'), ['api', 'web']);
  assert.deepEqual(texts('.search-file-name'), ['main.ts', 'app.ts']);
  dispose();
});

test("the same relative path opens each folder's own file", async () => {
  answers = { '/w/api': answer(['package.json']), '/w/web': answer(['package.json']) };
  const opened: string[] = [];
  setEditorOpener(async (request) => void opened.push(request.path));
  const { el, search, dispose } = panel([api, web]);
  await search('foo');
  const groups = [...el.querySelectorAll<HTMLElement>('.search-file')];
  assert.equal(groups.length, 2, 'the same path in two folders is two groups');

  for (const group of groups) {
    group.querySelector<HTMLElement>('.search-line')!.click();
    await flush();
  }
  assert.deepEqual(opened, ['/w/api/package.json', '/w/web/package.json']);

  // Collapsing one folder's group leaves the other's open: the key has the
  // folder in it.
  groups[0].querySelector<HTMLElement>('.search-file-header')!.click();
  await flush();
  assert.equal(groups[0].querySelectorAll('.search-line').length, 0);
  assert.equal(groups[1].querySelectorAll('.search-line').length, 1);
  setEditorOpener(null);
  dispose();
});

test('one folder looks as it does today', async () => {
  answers = { '/w/api': answer(['src/main.ts']) };
  const { el, texts, search, dispose } = panel([api]);
  await search('foo');
  assert.deepEqual(texts('.search-folder-header'), [], 'no header for one folder');
  assert.deepEqual(texts('.search-file-name'), ['main.ts']);
  assert.match(el.querySelector('.search-status')?.textContent ?? '', /1 result in 1 file/);
  dispose();
});

test("one folder's failure leaves the others' results", async () => {
  // The second folder's drive went between the window's check and the search.
  answers = { '/w/api': answer(['src/main.ts']) };
  const { el, texts, search, dispose } = panel([api, web]);
  await search('foo');
  assert.deepEqual(texts('.search-file-name'), ['main.ts'], 'the folder that answered');
  assert.deepEqual(texts('.search-folder-header'), ['api', 'web'], 'both are listed');
  const note = el.querySelector('.search-folder-error');
  assert.ok(note, "the failed folder's note");
  assert.match(note.textContent ?? '', /^web: /);
  assert.match(note.textContent ?? '', /no answer for \/w\/web/);
  dispose();
});

test('no folder answering at all: the panel says why', async () => {
  answers = {};
  const { el, dispose } = panel([api]);
  const input = el.querySelector<HTMLInputElement>('.search-input')!;
  input.value = 'foo';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  // Nothing draws but the panel's own error, which `search` waits past.
  for (let waited = 0; waited < 3000 && !el.querySelector('.search-error'); waited += 5) {
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  const message = el.querySelector('.search-error');
  assert.ok(message, 'the search failed');
  assert.match(message.textContent ?? '', /no answer for \/w\/api/);
  assert.equal(el.querySelector('.search-file'), null, 'nothing to show');
  dispose();
});

test('the result limit is for the whole search', () => {
  const many = (count: number, prefix: string) =>
    answer(Array.from({ length: count }, (_, i) => `${prefix}${i}.ts`));
  const whole = mergeResults(
    [
      { folder: api, result: many(3, 'a') },
      { folder: web, result: many(3, 'b') },
    ],
    4
  );
  assert.deepEqual(
    whole.perFolder.map((part) => part.lines.length),
    [3, 1],
    'the second folder keeps what the limit left'
  );
  assert.equal(whole.matchCount, 4);
  assert.equal(whole.truncated, true, 'the merge cut lines');
  assert.deepEqual([whole.totalMatches, whole.totalFiles], [6, 6], 'the totals are of both');
  assert.equal(whole.totalsComplete, true);

  // One folder stopped counting: the totals are at least that many.
  const partial = mergeResults(
    [
      { folder: api, result: many(1, 'a') },
      { folder: web, result: answer(['b0.ts'], { totalsComplete: false, truncated: true }) },
    ],
    2000
  );
  assert.equal(partial.totalsComplete, false);
  assert.equal(partial.truncated, true);
});
