// openInEditor.ts tests (node:test): the reveal that waits for a tab, the
// place a request names, and the opener App registers.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { diffLines } from './lineDiff';
import {
  dropReveal,
  firstChangedLine,
  openInEditor,
  pendingReveal,
  placeOf,
  requestReveal,
  setEditorOpener,
  takeReveal,
} from './openInEditor';

test('a reveal waits for its tab; the latest for a path wins; a closing tab drops it', () => {
  requestReveal('/p/a.ts', { line: 3 });
  const first = pendingReveal('/p/a.ts');
  assert.ok(first);
  assert.deepEqual(first, {
    line: 3,
    column: 1,
    length: 0,
    highlight: 'flash',
    focus: false,
    nonce: first.nonce,
  });
  requestReveal(
    '/p/a.ts',
    { line: 8, column: 4, length: 2 },
    { highlight: 'selection', focus: true }
  );
  const second = pendingReveal('/p/a.ts');
  assert.ok(second);
  assert.deepEqual(
    { line: second.line, column: second.column, length: second.length },
    { line: 8, column: 4, length: 2 }
  );
  assert.equal(second.highlight, 'selection');
  assert.equal(second.focus, true);
  assert.ok(second.nonce > first.nonce);
  // The editor showing the older one leaves the newer waiting.
  takeReveal('/p/a.ts', first);
  assert.equal(pendingReveal('/p/a.ts'), second);
  takeReveal('/p/a.ts', second);
  assert.equal(pendingReveal('/p/a.ts'), undefined);

  requestReveal('/p/b.ts', { line: 0 });
  assert.equal(pendingReveal('/p/b.ts')?.line, 1, 'a line before the first is the first');
  dropReveal('/p/b.ts');
  assert.equal(pendingReveal('/p/b.ts'), undefined);
  dropReveal('/p/never.ts');
});

test('the place a request names: given, where the snippet is, or the first change', () => {
  const text = 'a\nb\nc\nd\n';
  assert.deepEqual(placeOf({ line: 2, column: 3 }, text, undefined), { line: 2, column: 3 });
  assert.deepEqual(placeOf({ snippet: 'c\nd' }, text, undefined), { line: 3 });
  assert.equal(placeOf({ snippet: 'nowhere' }, text, undefined), undefined);
  const hunks = diffLines(['a', 'b', 'c'], ['a', 'B', 'c', 'd']);
  assert.equal(firstChangedLine(hunks), 2);
  assert.deepEqual(placeOf('first-change', text, hunks), { line: 2 });
  assert.equal(placeOf('first-change', text, []), undefined);
  assert.equal(placeOf('first-change', text, undefined), undefined);
  assert.equal(placeOf(undefined, text, hunks), undefined);
  assert.equal(firstChangedLine(diffLines(['a'], ['x', 'a'])), 1, 'an insertion at the top');
  assert.equal(firstChangedLine(diffLines(['a', 'b'], ['a', 'b', 'c'])), 3, 'one at the end');
  assert.equal(firstChangedLine(diffLines(['a', 'b'], ['a'])), 2, 'a deletion at the end');
});

test('a request goes to the opener App registered; without one, nothing happens', async () => {
  await openInEditor({ path: '/p/a.ts' });
  const seen: string[] = [];
  setEditorOpener(async (request) => {
    seen.push(request.path);
  });
  await openInEditor({ path: '/p/b.ts', focus: true, at: 'first-change' });
  setEditorOpener(null);
  await openInEditor({ path: '/p/c.ts' });
  assert.deepEqual(seen, ['/p/b.ts']);
});
