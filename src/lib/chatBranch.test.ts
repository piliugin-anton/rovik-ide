// chatBranch.ts tests (node:test): when a new chat is offered a branch of
// its own and what it's offered, what Yes and "Don't ask again" do, and
// that git it can't read never holds a chat up. The app is answered here,
// command by command. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { GitBranch, GitHead } from '../types';
import { ADJECTIVES, PROGRAMMERS } from './branchNames';
import { offerChatBranch, offersChatBranch, setOffersChatBranch } from './chatBranch';
import type { BranchOffer } from './chatBranch';

/** Where HEAD is, or why it can't be told. */
let head: () => GitHead = () => ({ branch: 'main', main_branch: 'main' });
let branches: GitBranch[] = [{ name: 'main', is_remote: false }];
/** The commands sent, in order. */
let calls: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'git_head') return head();
      if (cmd === 'git_branches') return branches;
      if (cmd === 'git_create_branch') return null;
      throw new Error(`no app: ${cmd}`);
    },
  },
};

/** The dialog, answering as told; what it was offered. */
function dialog(answer: (create: (name: string) => Promise<void>) => Promise<boolean>) {
  const offers: BranchOffer[] = [];
  const ask = async (offer: BranchOffer, create: (name: string) => Promise<void>) => {
    offers.push(offer);
    return answer(create);
  };
  return { offers, ask };
}

const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

/** The name suggested first: what Math.random's lowest draws make. */
const FIRST = `${ADJECTIVES[0]}-${PROGRAMMERS[0]}`;

test('a new chat on the main branch is offered a name no branch has, here or on a remote', async () => {
  calls = [];
  branches = [
    { name: 'main', is_remote: false },
    { name: FIRST, is_remote: false },
    { name: `origin/${FIRST}-2`, is_remote: true },
  ];
  const random = Math.random;
  Math.random = () => 0;
  const { offers, ask } = dialog(async () => true);
  try {
    await offerChatBranch('/p', ask);
  } finally {
    Math.random = random;
  }
  assert.deepEqual(offers, [{ current: 'main', suggested: `${FIRST}-3` }]);
  assert.deepEqual(sent('git_head'), [{ path: '/p' }]);
  assert.deepEqual(sent('git_create_branch'), [], 'No: nothing created');
});

test('Yes creates the branch where the project is and switches to it', async () => {
  calls = [];
  const { ask } = dialog(async (create) => {
    await create('kind-torvalds');
    return true;
  });
  await offerChatBranch('/p', ask);
  assert.deepEqual(sent('git_create_branch'), [
    { path: '/p', name: 'kind-torvalds', checkout: true, from: null },
  ]);
  assert.equal(offersChatBranch(), true, 'asked again for the next new chat');
});

test('a branch that can’t be created is the dialog’s to show', async () => {
  const refused =
    "Couldn't create branch 'kind-torvalds': a reference with that name already exists";
  const window = (globalThis as any).window;
  const invoke = window.__TAURI_INTERNALS__.invoke;
  window.__TAURI_INTERNALS__.invoke = async (cmd: string, args: any) =>
    cmd === 'git_create_branch' ? Promise.reject(refused) : invoke(cmd, args);
  let shown: unknown;
  const { ask } = dialog(async (create) => {
    await create('kind-torvalds').catch((err) => (shown = err));
    return true;
  });
  try {
    await offerChatBranch('/p', ask);
  } finally {
    window.__TAURI_INTERNALS__.invoke = invoke;
  }
  assert.equal(shown, refused);
});

test('another branch, a detached HEAD or no main branch: no question', async () => {
  const heads: GitHead[] = [
    { branch: 'feature', main_branch: 'main' },
    { branch: null, main_branch: 'main' },
    { branch: 'work', main_branch: null },
  ];
  for (const at of heads) {
    calls = [];
    head = () => at;
    const { offers, ask } = dialog(async () => true);
    await offerChatBranch('/p', ask);
    assert.deepEqual(offers, [], JSON.stringify(at));
    assert.deepEqual(sent('git_branches'), [], 'the branches aren’t read');
  }
  head = () => ({ branch: 'main', main_branch: 'main' });
});

test('git that can’t be read asks nothing, and only an unexpected failure is logged', async () => {
  const warn = console.warn;
  const warned: unknown[][] = [];
  console.warn = (...args: unknown[]) => void warned.push(args);
  const { offers, ask } = dialog(async () => true);
  try {
    head = () => {
      throw 'Not a git repository: could not find repository at /p';
    };
    await offerChatBranch('/p', ask);
    assert.deepEqual(warned, [], 'a project outside git is no problem');

    head = () => {
      throw 'failed to read HEAD: permission denied';
    };
    await offerChatBranch('/p', ask);
    assert.equal(warned.length, 1);
    assert.match(String(warned[0][1]), /permission denied/);
  } finally {
    console.warn = warn;
    head = () => ({ branch: 'main', main_branch: 'main' });
  }
  assert.deepEqual(offers, []);
});

test('“Don’t ask again” turns the offer off until it’s turned on again', async () => {
  const { offers, ask } = dialog(async () => false);
  await offerChatBranch('/p', ask);
  assert.equal(offers.length, 1);
  assert.equal(offersChatBranch(), false);

  calls = [];
  await offerChatBranch('/p', ask);
  assert.equal(offers.length, 1, 'not asked');
  assert.deepEqual(calls, [], 'git isn’t even read');

  setOffersChatBranch(true);
  await offerChatBranch('/p', ask);
  assert.equal(offers.length, 2, 'asked again');
});
