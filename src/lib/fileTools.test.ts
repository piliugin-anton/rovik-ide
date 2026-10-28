import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  isAtOrUnder,
  movedPath,
  normalizePath,
  parentOf,
  pathsIgnoreCase,
  samePath,
} from './fileTools';

/** Runs `body` as if the webview ran on `platform`, which the case rule reads. */
function onPlatform<T>(platform: string, body: () => T): T {
  const saved = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  Object.defineProperty(globalThis, 'navigator', { value: { platform }, configurable: true });
  try {
    return body();
  } finally {
    if (saved) Object.defineProperty(globalThis, 'navigator', saved);
    else delete (globalThis as { navigator?: unknown }).navigator;
  }
}

test('a path is at or under a folder, but a sibling sharing its first letters is not', () => {
  assert.equal(isAtOrUnder('/p', '/p'), true);
  assert.equal(isAtOrUnder('/p/src/a.ts', '/p/src'), true);
  assert.equal(isAtOrUnder('/p/src/a.ts', '/p/src/'), true);
  assert.equal(isAtOrUnder('/p/src2/a.ts', '/p/src'), false);
  assert.equal(isAtOrUnder('C:\\p\\src\\a.ts', 'C:\\p'), true);
  assert.equal(isAtOrUnder('C:\\p2\\a.ts', 'C:\\p'), false);
  assert.equal(isAtOrUnder('D:\\x', 'D:\\'), true);
});

test('everything is under a file system root, and a share is one', () => {
  assert.equal(isAtOrUnder('/p/a.ts', '/'), true);
  assert.equal(isAtOrUnder('D:\\a.ts', 'D:\\'), true);
  assert.equal(isAtOrUnder('\\\\server\\share\\a.ts', '\\\\server\\share'), true);
  assert.equal(isAtOrUnder('\\\\server\\share', '\\\\server\\share'), true);
  assert.equal(isAtOrUnder('\\\\server\\other\\a.ts', '\\\\server\\share'), false);
});

test("a folder is what's before the last separator, and a root has none", () => {
  assert.equal(parentOf('/p/src/a.ts'), '/p/src');
  assert.equal(parentOf('/p/src/'), '/p');
  assert.equal(parentOf('/p'), '/');
  assert.equal(parentOf('/'), null);
  assert.equal(parentOf('D:\\x'), 'D:\\');
  assert.equal(parentOf('D:\\x\\y'), 'D:\\x');
  assert.equal(parentOf('D:\\'), null);
  assert.equal(parentOf('\\\\server\\share\\x'), '\\\\server\\share');
  assert.equal(parentOf('\\\\server\\share'), null);
  assert.equal(parentOf('a.ts'), null);
});

test('paths name the same file as this file system compares them', () => {
  onPlatform('Linux x86_64', () => {
    assert.equal(pathsIgnoreCase(), false);
    assert.equal(samePath('/p/A.ts', '/p/a.ts'), false);
    assert.equal(isAtOrUnder('/p/A.ts', '/p/a'), false);
    assert.equal(isAtOrUnder('/P/a.ts', '/p'), false);
  });
  for (const platform of ['Win32', 'MacIntel']) {
    onPlatform(platform, () => {
      assert.equal(pathsIgnoreCase(), true, platform);
      assert.equal(samePath('/p/A.ts', '/p/a.ts'), true, platform);
      assert.equal(isAtOrUnder('/P/a.ts', '/p'), true, platform);
      assert.equal(samePath('C:\\P\\A.ts', 'c:/p/a.ts'), true, platform);
    });
  }
});

test('one spelling of a path, for keys', () => {
  assert.equal(normalizePath('C:\\Users\\me\\x.ts'), 'c:/Users/me/x.ts');
  assert.equal(normalizePath('/p//src/a.ts'), '/p/src/a.ts');
  assert.equal(normalizePath('/p/src/a.ts'), '/p/src/a.ts');
});

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
