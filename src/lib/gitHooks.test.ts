// Skipping a git hook that stopped the Git panel (lib/gitHooks.ts). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { skippableHook } from './gitHooks';

test('git hooks: a check that stopped a commit or a push can be skipped', () => {
  assert.equal(
    skippableHook('Not committed: the pre-commit hook failed (exit code 1):\nlint: 2 errors'),
    'pre-commit'
  );
  assert.equal(
    skippableHook('Not committed: the commit-msg hook failed (exit code 1).'),
    'commit-msg'
  );
  assert.equal(
    skippableHook(
      'Not committed: the pre-merge-commit hook failed (exit code 1).\nThe merge is still in progress: once that’s fixed, continue it (or abort it).'
    ),
    'pre-merge-commit'
  );
  assert.equal(
    skippableHook('Not pushed: the pre-push hook failed (exit code 1):\ntests failed'),
    'pre-push'
  );
  // Windows without Git for Windows: the hook can't run at all.
  assert.equal(
    skippableHook(
      "Not committed: the pre-commit hook couldn't start: it's a script, and running one takes the sh of Git for Windows"
    ),
    'pre-commit'
  );
});

test('git hooks: what --no-verify wouldn’t skip, or another error, has no skip', () => {
  // git runs prepare-commit-msg even with --no-verify.
  assert.equal(
    skippableHook('Not committed: the prepare-commit-msg hook failed (exit code 1).'),
    null
  );
  assert.equal(skippableHook('Not rebased: the pre-rebase hook failed (exit code 1).'), null);
  assert.equal(skippableHook('Not committed: the commit message the hooks left is empty.'), null);
  assert.equal(skippableHook('Push failed: authentication required'), null);
  assert.equal(skippableHook('Resolve the conflicts first.'), null);
});
