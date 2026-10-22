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
