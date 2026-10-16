import { test } from 'node:test';
import assert from 'node:assert/strict';
import { movedPath } from './fileTools';

test('a renamed file and the files in a renamed folder follow it', () => {
  const moves = [{ from: '/p/docs', to: '/p/documentation' }];
  assert.equal(movedPath('/p/docs', moves), '/p/documentation');
  assert.equal(movedPath('/p/docs/guide.md', moves), '/p/documentation/guide.md');
  assert.equal(movedPath('/p/docs/a/b.md', moves), '/p/documentation/a/b.md');
});

test('only paths at or inside the move follow it', () => {
  const moves = [{ from: '/p/docs', to: '/p/documentation' }];
  // A sibling sharing the prefix isn't inside.
  assert.equal(movedPath('/p/docs2/x.md', moves), null);
  assert.equal(movedPath('/p/src/main.ts', moves), null);
  // Virtual tabs (a commit's or a stash's version) never move.
  assert.equal(movedPath('commit@abc:/p/docs/guide.md', moves), null);
  assert.equal(movedPath('stash@0:/p/docs/guide.md', moves), null);
});

test('a folder given with a trailing separator still matches', () => {
  assert.equal(
    movedPath('/p/docs/x.md', [{ from: '/p/docs/', to: '/p/src/docs' }]),
    '/p/src/docs/x.md'
  );
});

test('the new path keeps its own separator (Windows)', () => {
  const moves = [{ from: 'C:\\p\\docs', to: 'C:\\p\\src\\docs' }];
  assert.equal(movedPath('C:\\p\\docs\\a\\b.md', moves), 'C:\\p\\src\\docs\\a\\b.md');
  // A tab opened with forward slashes lands on the tree's own form.
  assert.equal(movedPath('C:\\p\\docs/a/b.md', moves), 'C:\\p\\src\\docs\\a\\b.md');
});

test('the first move that covers a path decides', () => {
  const moves = [
    { from: '/p/a', to: '/p/x' },
    { from: '/p/b', to: '/p/y' },
  ];
  assert.equal(movedPath('/p/b/c.ts', moves), '/p/y/c.ts');
  assert.equal(movedPath('/p/a', moves), '/p/x');
});
