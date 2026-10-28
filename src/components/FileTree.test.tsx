// Component test: the tree's name input (a new entry, a rename) ends its
// edit once. Chromium (WebView2) fires blur on the input as ending the edit
// removes it: that blur submitted again (a new file created twice, "already
// exists"), and after Escape it submitted what Escape was cancelling. Run
// with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import FileTree from './FileTree';
import type { TreeRow } from '../types';

const row: TreeRow = { id: 'r1', path: '/p/notes.md', name: 'notes.md', isDir: false, depth: 0 };

function tree() {
  const [editingId, setEditingId] = createSignal<string | null>(row.id);
  const calls: string[] = [];
  const dispose = render(
    () => (
      <FileTree
        // A size for the virtualized list to show its rows in (happy-dom
        // lays nothing out; virtual-core 3.19 reads client*, older offset*).
        forwardRef={(el) => {
          for (const [k, v] of [
            ['offsetHeight', 400],
            ['offsetWidth', 300],
            ['clientHeight', 400],
            ['clientWidth', 300],
          ] as const) {
            Object.defineProperty(el, k, { value: v });
          }
        }}
        rows={() => [row]}
        expanded={() => new Set()}
        activePath={() => null}
        selectedIds={() => new Set()}
        onToggle={() => {}}
        onOpenFile={() => {}}
        onDrop={() => {}}
        rootDrop={() => true}
        onSelect={() => {}}
        onClearSelection={() => {}}
        editingId={editingId}
        onEditSubmit={(_, name) => {
          calls.push(`submit ${name}`);
          setEditingId(null);
        }}
        onEditCancel={() => {
          calls.push('cancel');
          setEditingId(null);
        }}
      />
    ),
    container()
  );
  const input = document.querySelector<HTMLInputElement>('.tree-row-input');
  assert.ok(input, 'the name input');
  return { input, calls, dispose };
}

const key = (input: HTMLInputElement, name: string) =>
  input.dispatchEvent(new KeyboardEvent('keydown', { key: name, bubbles: true, cancelable: true }));
/** What Chromium sends the input as it's removed. */
const blur = (input: HTMLInputElement) => input.dispatchEvent(new Event('blur'));

test('Enter submits once, whatever blur follows', () => {
  const { input, calls, dispose } = tree();
  input.value = 'guide.md';
  key(input, 'Enter');
  blur(input);
  assert.deepEqual(calls, ['submit guide.md']);
  dispose();
});

test('Escape cancels, and the blur after it submits nothing', () => {
  const { input, calls, dispose } = tree();
  input.value = 'renamed.md';
  key(input, 'Escape');
  blur(input);
  assert.deepEqual(calls, ['cancel']);
  dispose();
});

test('leaving the input submits what it holds', () => {
  const { input, calls, dispose } = tree();
  input.value = 'kept.md';
  blur(input);
  key(input, 'Enter');
  assert.deepEqual(calls, ['submit kept.md']);
  dispose();
});

// --- Rows of a workspace's folders: what may be dragged and dropped on ----

const folder: TreeRow = {
  id: 'f1',
  path: '/w/api',
  name: 'api',
  isDir: true,
  depth: 0,
  folderRow: true,
};
const missingFolder: TreeRow = { ...folder, missing: true };
const file: TreeRow = { id: 's1', path: '/w/api/a.ts', name: 'a.ts', isDir: false, depth: 1 };

/** A tree drawn with these rows, its drops and toggles recorded. */
function drawn(props: { rows: TreeRow[]; rootDrop?: boolean }) {
  const calls: string[] = [];
  const dispose = render(
    () => (
      <FileTree
        forwardRef={(el) => {
          for (const [k, v] of [
            ['offsetHeight', 400],
            ['offsetWidth', 300],
            ['clientHeight', 400],
            ['clientWidth', 300],
          ] as const) {
            Object.defineProperty(el, k, { value: v });
          }
        }}
        rows={() => props.rows}
        expanded={() => new Set()}
        activePath={() => null}
        selectedIds={() => new Set()}
        onToggle={(r) => calls.push(`toggle ${r.path}`)}
        onOpenFile={(r) => calls.push(`open ${r.path}`)}
        onDrop={(ids, destDir) =>
          calls.push(`drop ${ids.join(',')} into ${destDir?.path ?? 'root'}`)
        }
        rootDrop={() => props.rootDrop ?? true}
        onSelect={() => {}}
        onClearSelection={() => {}}
        editingId={() => null}
        onEditSubmit={() => {}}
        onEditCancel={() => {}}
      />
    ),
    container()
  );
  return { calls, dispose };
}

function rowEl(id: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-file-id="${id}"]`);
  assert.ok(el, `the row ${id}`);
  return el;
}

/** What the pointer is over: happy-dom lays nothing out, so tests say. */
function pointAt(el: Element | null) {
  (document as unknown as { elementFromPoint: () => Element | null }).elementFromPoint = () => el;
}

/** A press on a row, then a move past the drag threshold. */
function pressAndMove(id: string) {
  rowEl(id).dispatchEvent(
    new MouseEvent('mousedown', {
      bubbles: true,
      cancelable: true,
      button: 0,
      clientX: 10,
      clientY: 10,
    })
  );
  window.dispatchEvent(new MouseEvent('pointermove', { clientX: 40, clientY: 40 }));
}

const release = () =>
  window.dispatchEvent(new MouseEvent('pointerup', { clientX: 40, clientY: 40 }));

test("a folder row can't be dragged", () => {
  const { dispose } = drawn({ rows: [folder, file] });
  pointAt(rowEl('f1'));
  pressAndMove('f1');
  assert.equal(document.querySelector('.tree-drag-ghost'), null, 'nothing is carried');
  release();
  pressAndMove('s1');
  assert.ok(document.querySelector('.tree-drag-ghost'), 'a file row is carried');
  release();
  dispose();
});

test('a drop on a folder row moves into that folder', () => {
  const { calls, dispose } = drawn({ rows: [folder, file] });
  pointAt(rowEl('f1'));
  pressAndMove('s1');
  release();
  assert.deepEqual(calls, ['drop s1 into /w/api']);
  dispose();
});

test('a workspace has no project-root drop target', () => {
  const { calls, dispose } = drawn({ rows: [file], rootDrop: false });
  pointAt(document.querySelector('.file-tree'));
  pressAndMove('s1');
  assert.equal(document.querySelector('.tree-root-drop'), null, 'no bar to drop on');
  release();
  assert.deepEqual(calls, [], 'nothing is moved to a root there is not');
  dispose();
});

test("a missing folder's row says Folder not found", () => {
  const { calls, dispose } = drawn({ rows: [missingFolder] });
  const row = rowEl('f1');
  assert.ok(row.classList.contains('tree-root-missing'));
  assert.equal(row.querySelector('.tree-root-missing-note')?.textContent, 'Folder not found');
  assert.equal(row.querySelector('svg'), null, 'no chevron or folder icon');
  row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
  assert.deepEqual(calls, [], 'it opens nothing');
  dispose();
});

test("a drop on a missing folder's row moves nothing", () => {
  const { calls, dispose } = drawn({ rows: [missingFolder, file] });
  pointAt(rowEl('f1'));
  pressAndMove('s1');
  release();
  assert.deepEqual(calls, [], 'a missing folder has no id to move into');
  dispose();
});
