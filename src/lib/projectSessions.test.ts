// Each project's tabs, kept for coming back to it (projectSessions.ts).
// Outside the app the windows' shared file isn't there: these test what a
// window keeps (lib/sharedState.test.ts tests the sharing). Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { projectSession, rememberProjectSession, savedProjectSession } from './projectSessions';

const file = (path: string) => ({
  id: path,
  path,
  name: path.split('/').pop()!,
  language: 'typescript',
});

test('a project left keeps its tabs, the active one and its open folders', async () => {
  assert.equal(projectSession('/p/a'), null);
  await rememberProjectSession('/p/a', {
    files: [file('/p/a/x.ts'), file('/p/a/y.ts')],
    active: '/p/a/x.ts',
    expanded: ['/p/a/src'],
  });
  assert.deepEqual(await savedProjectSession('/p/a'), {
    files: [file('/p/a/x.ts'), file('/p/a/y.ts')],
    active: '/p/a/x.ts',
    expanded: ['/p/a/src'],
  });
  // Left again later: the newer tabs.
  await rememberProjectSession('/p/a', { files: [file('/p/a/z.ts')], active: null, expanded: [] });
  assert.deepEqual(projectSession('/p/a')?.files, [file('/p/a/z.ts')]);
  // Kept in this window's storage too, to start from.
  const stored = JSON.parse(localStorage.getItem('rovik.projectSessions') ?? '{}');
  assert.deepEqual(stored['/p/a'].files, [file('/p/a/z.ts')]);
});

test('only the most recently left projects are kept', async () => {
  for (let i = 0; i < 35; i++)
    await rememberProjectSession(`/p/${i}`, { files: [], active: null, expanded: [] }, 1000 + i);
  assert.equal(projectSession('/p/0'), null);
  assert.ok(projectSession('/p/34'));
  // The newest of all (the first test's) stays with them.
  assert.ok(projectSession('/p/a'));
  assert.equal(projectSession('/p/5'), null, 'the 30 kept: /p/a and /p/6 … /p/34');
  assert.ok(projectSession('/p/6'));
});
