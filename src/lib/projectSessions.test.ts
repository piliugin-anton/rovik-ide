// Each project's tabs, kept for coming back to it (projectSessions.ts).
// Outside the app the windows' shared file isn't there: these test what a
// window keeps (lib/sharedState.test.ts tests the sharing). Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  projectSession,
  rememberProjectSession,
  savedProjectSession,
  sessionInFolders,
  type ProjectSession,
} from './projectSessions';

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
    terminalOpen: true,
  });
  assert.deepEqual(await savedProjectSession('/p/a'), {
    files: [file('/p/a/x.ts'), file('/p/a/y.ts')],
    active: '/p/a/x.ts',
    expanded: ['/p/a/src'],
    terminalOpen: true,
  });
  // Left again later: the newer tabs.
  await rememberProjectSession('/p/a', {
    files: [file('/p/a/z.ts')],
    active: null,
    expanded: [],
    terminalOpen: false,
  });
  assert.deepEqual(projectSession('/p/a')?.files, [file('/p/a/z.ts')]);
  // Kept in this window's storage too, to start from.
  const stored = JSON.parse(localStorage.getItem('rovik.projectSessions') ?? '{}');
  assert.deepEqual(stored['/p/a'].files, [file('/p/a/z.ts')]);
});

test('only the most recently left projects are kept', async () => {
  for (let i = 0; i < 35; i++)
    await rememberProjectSession(
      `/p/${i}`,
      { files: [], active: null, expanded: [], terminalOpen: false },
      1000 + i
    );
  assert.equal(projectSession('/p/0'), null);
  assert.ok(projectSession('/p/34'));
  // The newest of all (the first test's) stays with them.
  assert.ok(projectSession('/p/a'));
  assert.equal(projectSession('/p/5'), null, 'the 30 kept: /p/a and /p/6 … /p/34');
  assert.ok(projectSession('/p/6'));
});

test("a session kept before its terminal panel was: the panel's state unknown", async () => {
  const older: ProjectSession = { files: [], active: null, expanded: [] };
  await rememberProjectSession('/p/older', older);
  assert.equal(projectSession('/p/older')?.terminalOpen, undefined);
});

test('a workspace keeps its entry under its own key', async () => {
  await rememberProjectSession('workspace:w-1', {
    files: [file('/p/api/x.ts'), file('/p/web/y.ts')],
    active: '/p/api/x.ts',
    expanded: ['/p/api/src'],
    terminalOpen: false,
  });
  assert.deepEqual(await savedProjectSession('workspace:w-1'), {
    files: [file('/p/api/x.ts'), file('/p/web/y.ts')],
    active: '/p/api/x.ts',
    expanded: ['/p/api/src'],
    terminalOpen: false,
  });
  // Its folders' own entries are theirs, under their own paths.
  assert.equal(projectSession('/p/api'), null);
});

test('a tab of a folder that left the workspace is not reopened', async () => {
  const session: ProjectSession = {
    files: [file('/p/api/x.ts'), file('/p/web/y.ts')],
    active: '/p/web/y.ts',
    expanded: ['/p/api/src', '/p/web/src'],
    terminalOpen: true,
  };
  assert.deepEqual(sessionInFolders(session, ['/p/api']), {
    files: [file('/p/api/x.ts')],
    active: '/p/api/x.ts',
    expanded: ['/p/api/src'],
    terminalOpen: true,
  });
  // Both folders: nothing leaves, the active tab stays.
  assert.deepEqual(sessionInFolders(session, ['/p/api', '/p/web']), session);
  // A project is a list of one, and its own tabs are all under it.
  assert.deepEqual(
    sessionInFolders({ ...session, files: [file('/p/api/x.ts')], active: null }, ['/p/api']),
    {
      files: [file('/p/api/x.ts')],
      active: '/p/api/x.ts',
      expanded: ['/p/api/src'],
      terminalOpen: true,
    }
  );
});
