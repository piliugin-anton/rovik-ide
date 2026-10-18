// Publishing a branch from the Git panel (lib/gitPublish.ts). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { GitStatus } from '../types';
import { commitAction, publishQuestion, unpublished } from './gitPublish';

const file: GitStatus['files'][number] = {
  path: 'a.txt',
  status: 'M',
  staged: false,
  unstaged: true,
  additions: 1,
  deletions: 0,
};

/** A new branch: no upstream, origin to push to, nothing changed. */
const newBranch: GitStatus = {
  branch: 'feature/x',
  ahead: 0,
  behind: 0,
  files: [],
  workdir: '/p',
  upstream: null,
  push_remote: 'origin',
};

test('git publish: a branch with no remote branch is published to its push remote', () => {
  assert.equal(unpublished(newBranch), true);
  // Tracking one, no remote to push to, a rebase under way, or no status.
  assert.equal(unpublished({ ...newBranch, upstream: 'origin/feature/x' }), false);
  assert.equal(unpublished({ ...newBranch, push_remote: null }), false);
  assert.equal(unpublished({ ...newBranch, push_remote: undefined }), false);
  assert.equal(
    unpublished({
      ...newBranch,
      operation: {
        kind: 'rebase',
        branch: 'feature/x',
        incoming: 'main',
        step: 1,
        total: 2,
      } as GitStatus['operation'],
    }),
    false
  );
  assert.equal(unpublished(null), false);
});

test('git publish: the commit button commits first, then publishes, and pushes once tracked', () => {
  // Nothing to commit on a new branch: Publish.
  assert.equal(commitAction(newBranch, ''), 'publish');
  // A change without a message, or a message without a change: still Publish.
  assert.equal(commitAction({ ...newBranch, files: [file] }, '  '), 'publish');
  assert.equal(commitAction(newBranch, 'Fix'), 'publish');
  // A change and its message: the commit, which then asks to publish.
  assert.equal(commitAction({ ...newBranch, files: [file] }, 'Fix'), 'commit');
  // Tracked, with commits the remote branch lacks: Push.
  const tracked = { ...newBranch, upstream: 'origin/feature/x' };
  assert.equal(commitAction({ ...tracked, ahead: 2 }, ''), 'push');
  assert.equal(commitAction(tracked, ''), 'commit');
  // No remote at all: the commit stays here.
  assert.equal(commitAction({ ...newBranch, push_remote: null }, ''), 'commit');
});

test('git publish: the question names the branch and the remote', () => {
  const q = publishQuestion(newBranch);
  assert.equal(q.title, 'Publish feature/x?');
  assert.match(q.message, /^origin has no branch feature\/x yet, so the commit wasn’t pushed\./);
  assert.match(q.message, /creates feature\/x on origin and pushes it there/);
  assert.equal(q.confirmLabel, 'Publish branch');
  assert.equal(q.cancelLabel, 'Not now');
});
