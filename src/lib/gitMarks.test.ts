// gitMarks.ts: the base a file's change marks compare against, asked of the
// file's own repository (a project at repo/app asks for app/package.json),
// and the tabs a refresh leaves alone (a version of a file, not a file).
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createWorkdirReads,
  gitBaseFor,
  refreshableFiles,
  repoFileOf,
  setGitBase,
} from './gitMarks';

test('a file of a project inside a repository is asked as its path in the repository', () => {
  assert.deepEqual(repoFileOf('/repo/app/package.json', ['/repo']), {
    workdir: '/repo',
    file: 'app/package.json',
  });
  // A folder that is the repository itself: the path from its top.
  assert.deepEqual(repoFileOf('/repo/app/package.json', ['/repo/app']), {
    workdir: '/repo/app',
    file: 'package.json',
  });
  // The deepest working folder decides when several hold the file.
  assert.deepEqual(repoFileOf('/repo/app/package.json', ['/repo', '/repo/app']), {
    workdir: '/repo/app',
    file: 'package.json',
  });
  // Windows, as the app spells it.
  assert.deepEqual(repoFileOf('C:\\repo\\app\\a.ts', ['C:\\repo']), {
    workdir: 'C:\\repo',
    file: 'app/a.ts',
  });
});

test('a file in no repository of the window has no base to ask for', () => {
  assert.equal(repoFileOf('/elsewhere/a.ts', ['/repo']), null);
  assert.equal(repoFileOf('/repo/app/a.ts', []), null);
  // A working folder other than the file's (never: the file is in one of
  // them, the deepest).
  assert.equal(repoFileOf('/repo/app/a.ts', ['/other']), null);
});

test('a tab holding a version of a file is never refreshed', () => {
  setGitBase('/repo/app/a.ts', '/repo', 'app/a.ts', 'on disk\n');
  setGitBase('disk:/repo/app/a.ts', '', 'a.ts', 'on disk, as kept\n');
  setGitBase('chat:d1:src/x.ts', '', 'src/x.ts', 'a chat wrote this\n');
  setGitBase('stash@0:/repo/app/a.ts', '', 'app/a.ts', 'stashed\n');
  assert.deepEqual(
    refreshableFiles().map((file) => file.absPath),
    ['/repo/app/a.ts'],
    'only the file on disk is asked about'
  );
  // The versions keep their bases: they're compared with what they hold.
  assert.equal(gitBaseFor('disk:/repo/app/a.ts'), 'on disk, as kept\n');
  assert.equal(gitBaseFor('chat:d1:src/x.ts'), 'a chat wrote this\n');
  assert.equal(gitBaseFor('stash@0:/repo/app/a.ts'), 'stashed\n');
});

test("a chat's host's edit and plan keep their marks through a refresh", () => {
  // Both open as comparison tabs (approvals.ts showRemoteEdit, plans.ts
  // openPlan) with no repository: a refresh must leave them alone, not ask
  // git with an empty path and drop their marks.
  setGitBase('/repo/app/a.ts', '/repo', 'app/a.ts', 'on disk\n');
  setGitBase('remote-edit:d1:call1', '', 'src/x.ts', 'as the server had it\n');
  setGitBase('plan:d1:/repo/app/plan.md', '', 'plan.md', 'the plan\n');
  assert.deepEqual(
    refreshableFiles().map((file) => file.absPath),
    ['/repo/app/a.ts'],
    'no repository, no refresh'
  );
  assert.equal(gitBaseFor('remote-edit:d1:call1'), 'as the server had it\n');
  assert.equal(gitBaseFor('plan:d1:/repo/app/plan.md'), 'the plan\n');
});

test('a base with no repository is never asked for', () => {
  // No paths differ, but no workdir either: asking would fail and take the
  // marks with it.
  setGitBase('/elsewhere/a.ts', '', 'a.ts', 'as it was\n');
  assert.equal(
    refreshableFiles().some((file) => file.absPath === '/elsewhere/a.ts'),
    false,
    'a base with no repository is left alone'
  );
  assert.equal(gitBaseFor('/elsewhere/a.ts'), 'as it was\n');
});

test('a tab opened before the repositories are read still gets its marks', async () => {
  // The read answers only when the test says so: the window is opening.
  let answer: (workdirs: string[]) => void = () => {};
  const reads = createWorkdirReads(() => new Promise<string[]>((resolve) => (answer = resolve)));
  const asked = reads.of('/repo');
  answer(['/repo']);
  assert.deepEqual(await asked, ['/repo'], "the file's repository, once it's known");
});

test('the repositories are read once per set of folders', async () => {
  let reads = 0;
  const made = createWorkdirReads(async () => {
    reads += 1;
    return ['/api'];
  });
  assert.deepEqual(await made.of('/work/api'), ['/api']);
  await made.of('/work/api');
  assert.equal(reads, 1, 'the same folders: no second read');
  await made.of('/work/api\n/work/web');
  assert.equal(reads, 2, 'other folders: read again');
});

test("git's state changed: the repositories are read again", async () => {
  let reads = 0;
  const made = createWorkdirReads(async () => {
    reads += 1;
    return [`/repo${reads}`];
  });
  assert.deepEqual(await made.of('/repo'), ['/repo1']);
  made.forget();
  assert.deepEqual(await made.of('/repo'), ['/repo2'], 'a `git init` is seen');
});
