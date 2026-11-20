import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import RepositoryAccounts from './RepositoryAccounts';

test('both accounts are available without opening a project and signing out affects one provider', async () => {
  const actions: { provider: string; action: { kind: string } }[] = [];
  const signed = new Set(['github', 'gitlab']);
  (window as any).__TAURI_INTERNALS__ = {
    transformCallback: () => 1,
    invoke: async (command: string, args: any) => {
      assert.equal(command, 'repository_account');
      assert.equal(args.root, undefined);
      actions.push(args);
      if (args.action.kind === 'signOut') {
        signed.delete(args.provider);
        return null;
      }
      return signed.has(args.provider)
        ? { login: args.provider + '-user', issuer: 'https://team.example/' }
        : null;
    },
  };
  const root = container();
  const dispose = render(() => <RepositoryAccounts />, root);
  try {
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.match(root.textContent!, /github-user/);
    assert.match(root.textContent!, /gitlab-user/);
    Array.from(root.querySelectorAll('button'))
      .find((b) => b.textContent === 'Sign out of GitHub')!
      .click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.match(root.textContent!, /Sign in to GitHub/);
    assert.match(root.textContent!, /gitlab-user/);
    assert.deepEqual(
      actions.filter((a) => a.action.kind === 'signOut').map((a) => a.provider),
      ['github']
    );
  } finally {
    dispose();
    root.remove();
  }
});
