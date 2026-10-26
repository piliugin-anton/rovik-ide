// fileFormatters.ts: a file is formatted with the config of the folder it's
// in — a workspace's second folder's, not the first's (D14). The formatter
// itself runs in the backend, so what's checked here is which folder's config
// a file is formatted with.
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { formatterRoot } from './fileFormatters';

test("a file of the second folder is formatted with the second folder's config", () => {
  const folders = ['/work/api', '/work/web'];
  assert.equal(formatterRoot(folders, '/work/web/src/a.ts'), '/work/web');
  assert.equal(formatterRoot(folders, '/work/api/src/a.ts'), '/work/api');
  assert.equal(formatterRoot(folders, '/work/web/package.json'), '/work/web');
});

test('a file in no folder of the window is formatted with no config', () => {
  assert.equal(formatterRoot(['/work/api', '/work/web'], '/elsewhere/a.ts'), null);
  assert.equal(formatterRoot([], '/work/api/a.ts'), null);
});
