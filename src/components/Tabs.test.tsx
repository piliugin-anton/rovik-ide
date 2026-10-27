// Component test: the tab strip (Tabs.tsx) shows files and browser tabs in
// their order: a file by its name, marked when it's unsaved or deleted; a
// browser tab by its page's host, with a globe, titled with its address.
// Each is selected and closed by its key. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import Tabs from './Tabs';
import type { EditorTab } from '../lib/editorTabs';
import type { OpenFile } from '../types';

const file = (path: string, more: Partial<OpenFile> = {}): OpenFile => ({
  id: path,
  path,
  name: path.split('/').pop()!,
  content: '',
  originalContent: '',
  dirty: false,
  language: 'typescript',
  ...more,
});

const TABS: EditorTab[] = [
  file('/p/a.ts', { dirty: true }),
  { kind: 'browser', path: 'browser:1', address: 'http://localhost:5173/admin', loads: 0 },
  file('/p/b.ts', { deleted: true }),
];

function rendered() {
  const root = container();
  const selected: string[] = [];
  const closed: string[] = [];
  const dispose = render(
    () => (
      <Tabs
        tabs={() => TABS}
        activePath={() => 'browser:1'}
        folders={() => [{ path: '/p', name: 'p' }]}
        onSelect={(path) => selected.push(path)}
        onClose={(path) => closed.push(path)}
      />
    ),
    root
  );
  const tabs = [...root.querySelectorAll<HTMLElement>('.tab')];
  return { tabs, selected, closed, dispose };
}

test('files and browser tabs show in their order, each by its own name', () => {
  const { tabs, dispose } = rendered();
  const names = tabs.map((t) => t.querySelector('.tab-name')!.textContent);
  assert.deepEqual(names, ['a.ts •', 'localhost:5173', 'b.ts [DELETED]']);
  const [a, page, b] = tabs;
  assert.ok(page.classList.contains('browser') && page.classList.contains('active'));
  assert.equal(page.getAttribute('title'), 'http://localhost:5173/admin');
  assert.ok(page.querySelector('svg.tab-icon'), 'a globe');
  assert.equal(a.querySelector('svg.tab-icon'), null);
  assert.ok(b.classList.contains('deleted'));
  assert.equal(a.getAttribute('title'), '/p/a.ts');
  dispose();
});

test('a browser tab is selected and closed by its key', () => {
  const { tabs, selected, closed, dispose } = rendered();
  const page = tabs[1];
  page.click();
  assert.deepEqual(selected, ['browser:1']);
  page.querySelector<HTMLElement>('.tab-close')!.click();
  assert.deepEqual(closed, ['browser:1']);
  assert.deepEqual(selected, ['browser:1'], 'closing it doesn’t select it');
  page.dispatchEvent(new MouseEvent('auxclick', { button: 1, bubbles: true }));
  assert.deepEqual(closed, ['browser:1', 'browser:1'], 'a middle click closes it too');
  dispose();
});

test('two tabs of one name show where each one is', () => {
  const root = container();
  const tabs: EditorTab[] = [file('/p/api/package.json'), file('/p/web/package.json')];
  const dispose = render(
    () => (
      <Tabs
        tabs={() => tabs}
        activePath={() => null}
        folders={() => [{ path: '/p', name: 'p' }]}
        onSelect={() => {}}
        onClose={() => {}}
      />
    ),
    root
  );
  const hints = [...root.querySelectorAll('.tab-hint')].map((el) => el.textContent);
  assert.deepEqual(hints, ['· api', '· web']);
  // The file's own name is unchanged; the hint is beside it.
  const names = [...root.querySelectorAll('.tab-name')].map((el) => el.firstChild?.textContent);
  assert.deepEqual(names, ['package.json', 'package.json']);
  dispose();
});
