// Publishing a branch from the Git panel (lib/gitPublish.ts). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { GitStatus } from '../types';
import {
  commitAction,
  commitActionAcross,
  hasWork,
  includable,
  includeBlocked,
  partialText,
  publishHintAcross,
  publishQuestion,
  publishQuestionAcross,
  stoppedText,
  unpublished,
} from './gitPublish';

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

const ahead: GitStatus = { ...newBranch, upstream: 'origin/feature/x', ahead: 1 };
const changes: GitStatus = { ...newBranch, upstream: 'origin/feature/x', files: [file] };
const tracked: GitStatus = { ...newBranch, upstream: 'origin/feature/x' };

test('git publish: over several repositories the button commits first, then pushes, then publishes', () => {
  // A change and a message anywhere: the commit, though another is ahead.
  assert.equal(commitActionAcross([ahead, changes], 'Fix'), 'commit');
  // No message, or no change: push what's ahead, else publish.
  assert.equal(commitActionAcross([ahead, changes], ''), 'push');
  assert.equal(commitActionAcross([newBranch, ahead], 'Fix'), 'push');
  assert.equal(commitActionAcross([newBranch, tracked], ''), 'publish');
  // Nothing to do: Commit, which the panel leaves disabled.
  assert.equal(commitActionAcross([tracked], ''), 'commit');
  assert.equal(commitActionAcross([], 'Fix'), 'commit');
});

test('git publish: a merge, a rebase or conflicts keep a repository out of the commit, saying why', () => {
  const merging = {
    ...changes,
    operation: { kind: 'merge', conflicts: [] } as unknown as GitStatus['operation'],
  };
  assert.equal(includeBlocked(merging, 'api'), 'Finish the merge in api first');
  assert.equal(includable(merging), false);
  const conflicted: GitStatus = { ...tracked, files: [{ ...file, status: 'U' }] };
  assert.equal(includeBlocked(conflicted, 'web'), 'Resolve the conflicts in web first');
  assert.equal(includable(conflicted), false);
  // Nothing in the way: in, when there's something to commit or push.
  assert.equal(includeBlocked(changes, 'api'), null);
  assert.equal(includable(changes), true);
  assert.equal(includable(ahead), true);
  assert.equal(includable(newBranch), true, 'a branch to publish');
  assert.equal(hasWork(tracked), false);
  assert.equal(includable(tracked), false, 'nothing to commit or push');
});

test('git publish: one question publishes every branch a commit across repositories left unpublished', () => {
  const api = { name: 'api', status: newBranch };
  const web = { name: 'web', status: { ...newBranch, branch: 'cart', push_remote: 'upstream' } };
  const both = publishQuestionAcross([api, web]);
  assert.equal(both.title, 'Publish api’s feature/x and web’s cart?');
  assert.match(
    both.message,
    /^These branches have no remote branch yet, so their commits weren’t pushed: api’s feature\/x \(origin\) and web’s cart \(upstream\)\./
  );
  assert.equal(both.confirmLabel, 'Publish branches');
  assert.equal(both.cancelLabel, 'Not now');
  const one = publishQuestionAcross([web]);
  assert.equal(one.title, 'Publish web’s cart?');
  assert.match(
    one.message,
    /^This branch has no remote branch yet, so its commit wasn’t pushed: web’s cart \(upstream\)\./
  );
  assert.equal(one.confirmLabel, 'Publish branch');
  assert.equal(
    publishHintAcross([api, web]),
    'Create api’s feature/x on origin and web’s cart on upstream and push them'
  );
});

test('git publish: what a stopped or a partial commit across repositories says names each repository', () => {
  assert.equal(
    stoppedText([
      {
        name: 'web',
        error: 'Not committed: the pre-commit hook failed (exit code 1):\nlint: 2 errors',
      },
    ]),
    'Nothing was committed.\nweb: the pre-commit hook failed (exit code 1):\nlint: 2 errors'
  );
  assert.equal(
    stoppedText([
      { name: 'api', error: 'A merge is in progress: finish or abort it first.' },
      { name: 'docs', error: 'Resolve the conflicts first.' },
    ]),
    'Nothing was committed.\napi: A merge is in progress: finish or abort it first.\ndocs: Resolve the conflicts first.'
  );
  const lock = { name: 'infra', error: 'failed to lock file' };
  assert.equal(
    partialText(['api', 'web'], [lock]),
    'Committed in api and web; nothing is pushed until infra is committed too.\ninfra: failed to lock file'
  );
  assert.equal(
    partialText(['api'], [lock, { name: 'docs', error: 'disk full' }]),
    'Committed in api; nothing is pushed until infra and docs are committed too.\ninfra: failed to lock file\ndocs: disk full'
  );
  assert.equal(partialText([], [lock]), 'Nothing was committed.\ninfra: failed to lock file');
});
