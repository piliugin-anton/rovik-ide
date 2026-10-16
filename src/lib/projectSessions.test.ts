// Each project's tabs, kept for coming back to it (projectSessions.ts).
// Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { projectSession, rememberProjectSession } from './projectSessions';

const file = (path: string) => ({
  id: path,
  path,
  name: path.split('/').pop()!,
  language: 'typescript',
});

test('a project left keeps its tabs, the active one and its open folders', () => {
  localStorage.clear();
  assert.equal(projectSession('/p/a'), null);
  rememberProjectSession('/p/a', {
    files: [file('/p/a/x.ts'), file('/p/a/y.ts')],
    active: '/p/a/x.ts',
    expanded: ['/p/a/src'],
  });
  assert.deepEqual(projectSession('/p/a'), {
    files: [file('/p/a/x.ts'), file('/p/a/y.ts')],
    active: '/p/a/x.ts',
    expanded: ['/p/a/src'],
  });
  // Left again later: the newer tabs.
  rememberProjectSession('/p/a', { files: [file('/p/a/z.ts')], active: null, expanded: [] });
  assert.deepEqual(projectSession('/p/a')?.files, [file('/p/a/z.ts')]);
});

test('only the most recently left projects are kept, and bad data is no data', () => {
  localStorage.clear();
  for (let i = 0; i < 35; i++)
    rememberProjectSession(`/p/${i}`, { files: [], active: null, expanded: [] }, 1000 + i);
  assert.equal(projectSession('/p/0'), null);
  assert.ok(projectSession('/p/34'));
  localStorage.setItem('rovik.projectSessions', '{not json');
  assert.equal(projectSession('/p/34'), null);
});
