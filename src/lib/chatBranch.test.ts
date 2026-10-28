// chatBranch.ts tests (node:test): when a new chat is offered a branch of
// its own and what it's offered, what Yes and "Don't ask again" do, and
// that git it can't read never holds a chat up. A chat across folders is
// asked once about each of its repositories on its main branch (D29), and
// a folder that joins it later about its own. The app is answered here,
// command by command. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { ChatMessage, GitHead, GitRepo } from '../types';
import {
  foldersAtLastRun,
  offerChatBranch,
  offersChatBranch,
  setOffersChatBranch,
} from './chatBranch';
import type { BranchOffer } from './chatBranch';

/** Where HEAD is, or why it can't be told. */
let head: () => GitHead = () => ({ branch: 'main', main_branch: 'main' });
/** Where HEAD is in each repository, by the folder git is asked from: else `head`. */
let heads: Record<string, GitHead> = {};
/** The repositories `git_repos` finds. */
let repos: GitRepo[] = [];
/** Why `git_create_branch` fails there, by folder, once. */
let failing: Record<string, string> = {};
/** The commands sent, in order. */
let calls: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'git_head') return heads[args.path] ?? head();
      if (cmd === 'git_repos') return { repos, without: [] };
      if (cmd === 'git_create_branch') {
        const why = failing[args.path];
        delete failing[args.path];
        if (why) throw why;
        return null;
      }
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

/** A name the dialog could have been offered. */
const NAME = 'rovik-kind-torvalds-x7k2m9qa';

/** The project at /p: a folder chat's one folder. */
const P = [{ path: '/p', name: 'p' }];

test('a new chat on the main branch is offered a name made up for it', async () => {
  calls = [];
  const { offers, ask } = dialog(async () => true);
  await offerChatBranch(P, ask);
  assert.equal(offers.length, 1);
  assert.deepEqual(offers[0].repos, [{ name: 'p', branch: 'main' }]);
  assert.deepEqual(offers[0].staying, []);
  assert.match(offers[0].suggested, /^rovik-[a-z]+-[a-z]+-[a-z0-9]{8}$/);
  assert.deepEqual(
    calls,
    [{ cmd: 'git_head', args: { path: '/p' } }],
    'only where HEAD is read, and No creates nothing'
  );
});

test('Yes creates the branch where the project is and switches to it', async () => {
  calls = [];
  const { ask } = dialog(async (create) => {
    await create(NAME);
    return true;
  });
  await offerChatBranch(P, ask);
  assert.deepEqual(sent('git_create_branch'), [
    { path: '/p', name: NAME, checkout: true, from: null },
  ]);
  assert.equal(offersChatBranch(), true, 'asked again for the next new chat');
});

test('a branch that can’t be created is the dialog’s to show', async () => {
  const refused = `Couldn't create branch '${NAME}': a reference with that name already exists`;
  const window = (globalThis as any).window;
  const invoke = window.__TAURI_INTERNALS__.invoke;
  window.__TAURI_INTERNALS__.invoke = async (cmd: string, args: any) =>
    cmd === 'git_create_branch' ? Promise.reject(refused) : invoke(cmd, args);
  let shown: unknown;
  const { ask } = dialog(async (create) => {
    await create(NAME).catch((err) => (shown = err));
    return true;
  });
  try {
    await offerChatBranch(P, ask);
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
    await offerChatBranch(P, ask);
    assert.deepEqual(offers, [], JSON.stringify(at));
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
    await offerChatBranch(P, ask);
    assert.deepEqual(warned, [], 'a project outside git is no problem');

    head = () => {
      throw 'failed to read HEAD: permission denied';
    };
    await offerChatBranch(P, ask);
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
  await offerChatBranch(P, ask);
  assert.equal(offers.length, 1);
  assert.equal(offersChatBranch(), false);

  calls = [];
  await offerChatBranch(P, ask);
  assert.equal(offers.length, 1, 'not asked');
  assert.deepEqual(calls, [], 'git isn’t even read');

  setOffersChatBranch(true);
  await offerChatBranch(P, ask);
  assert.equal(offers.length, 2, 'asked again');
});

/** A repository of the workspace's folders, as `git_repos` finds it. */
const repo = (name: string, folders: string[]): GitRepo => ({
  key: `/w/${name}/.git`,
  workdir: `/w/${name}`,
  name,
  scopes: folders.map(() => ''),
  folders,
});
const API = { path: '/w/api', name: 'api' };
const WEB = { path: '/w/web', name: 'web' };
const DOCS = { path: '/w/docs', name: 'docs' };

test('repositories on their main branch are offered one branch, named once; one on another branch keeps it', async () => {
  setOffersChatBranch(true);
  calls = [];
  repos = [repo('api', ['/w/api']), repo('web', ['/w/web']), repo('docs', ['/w/docs'])];
  heads = {
    '/w/api': { branch: 'main', main_branch: 'main' },
    '/w/web': { branch: 'master', main_branch: 'master' },
    '/w/docs': { branch: 'feature', main_branch: 'main' },
  };
  const { offers, ask } = dialog(async (create) => {
    await create(NAME);
    return true;
  });
  try {
    await offerChatBranch([API, WEB, DOCS], ask);
  } finally {
    heads = {};
  }
  assert.equal(offers.length, 1, 'one question');
  assert.deepEqual(offers[0].repos, [
    { name: 'api', branch: 'main' },
    { name: 'web', branch: 'master' },
  ]);
  assert.deepEqual(offers[0].staying, [{ name: 'docs', branch: 'feature' }]);
  assert.deepEqual(sent('git_create_branch'), [
    { path: '/w/api', name: NAME, checkout: true, from: null },
    { path: '/w/web', name: NAME, checkout: true, from: null },
  ]);
});

test('two folders of one repository are asked about once', async () => {
  setOffersChatBranch(true);
  calls = [];
  const front = { path: '/w/mono/web', name: 'web' };
  const back = { path: '/w/mono/api', name: 'api' };
  repos = [repo('mono', ['/w/mono/api', '/w/mono/web'])];
  const { offers, ask } = dialog(async (create) => {
    await create(NAME);
    return true;
  });
  await offerChatBranch([back, front], ask);
  assert.deepEqual(offers[0].repos, [{ name: 'mono', branch: 'main' }]);
  assert.equal(sent('git_head').length, 1);
  assert.deepEqual(sent('git_create_branch'), [
    { path: '/w/mono/api', name: NAME, checkout: true, from: null },
  ]);
});

test('a branch that fails in one repository says which have it, and Yes again makes it in the rest', async () => {
  setOffersChatBranch(true);
  calls = [];
  repos = [repo('api', ['/w/api']), repo('web', ['/w/web'])];
  failing = { '/w/web': 'the index is locked' };
  const errors: unknown[] = [];
  const { ask } = dialog(async (create) => {
    await create(NAME).catch((err) => errors.push(err));
    await create(NAME);
    return true;
  });
  await offerChatBranch([API, WEB], ask);
  assert.deepEqual(errors, ['web: the index is locked api has it: Yes makes it in the rest.']);
  assert.deepEqual(
    sent('git_create_branch').map((args) => args.path),
    ['/w/api', '/w/web', '/w/web'],
    'api once'
  );
});

test('a folder that joined the chat is asked about, offered the branch the chat’s other repositories are on', async () => {
  setOffersChatBranch(true);
  calls = [];
  repos = [repo('api', ['/w/api']), repo('web', ['/w/web'])];
  heads = { '/w/api': { branch: 'rovik-cart', main_branch: 'main' } };
  const { offers, ask } = dialog(async (create) => {
    await create(offers[0].suggested);
    return true;
  });
  try {
    await offerChatBranch([API, WEB], ask, ['api']);
  } finally {
    heads = {};
  }
  assert.equal(offers[0].suggested, 'rovik-cart', 'the chat’s branch');
  assert.deepEqual(offers[0].repos, [{ name: 'web', branch: 'main' }]);
  assert.deepEqual(offers[0].staying, [{ name: 'api', branch: 'rovik-cart' }]);
  assert.deepEqual(sent('git_create_branch'), [
    { path: '/w/web', name: 'rovik-cart', checkout: true, from: null },
  ]);

  // The chat's repositories on their main branch too: a new name.
  const fresh = dialog(async () => true);
  await offerChatBranch([API, WEB], fresh.ask, ['api']);
  assert.match(fresh.offers[0].suggested, /^rovik-[a-z]+-[a-z]+-[a-z0-9]{8}$/);
  // None joined: nothing to ask.
  const none = dialog(async () => true);
  await offerChatBranch([API, WEB], none.ask, ['api', 'web']);
  assert.deepEqual(none.offers, []);
});

test('a workspace chat’s folders at its last run are the latest its run recorded', () => {
  const user = (meta?: ChatMessage['meta']): ChatMessage => ({
    id: crypto.randomUUID(),
    role: 'user',
    content: 'go',
    meta,
  });
  assert.equal(foldersAtLastRun([]), null, 'no run yet');
  assert.deepEqual(
    foldersAtLastRun([user({ folders: ['api'] }), user({ folders: ['api', 'web'] }), user()]),
    ['api', 'web']
  );
});
