// Component tests: the dialog that offers a new chat a branch of its own
// (lib/chatBranch.ts): the name made up for it, Yes and No, a name git
// can't take, a branch that can't be created, and "Don't ask again"; for a
// chat across folders, the repositories it's made in and those that stay.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { askForChatBranch, NewBranchDialogHost } from './NewBranchDialog';
import type { BranchOffer } from '../lib/chatBranch';

const OFFER: BranchOffer = {
  repos: [{ name: 'p', branch: 'main' }],
  staying: [],
  suggested: 'rovik-kind-torvalds-x7k2m9qa',
};

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

const dialog = () => document.querySelector('.confirm-dialog') as HTMLElement | null;
const nameInput = () => dialog()!.querySelector('input.new-project-input') as HTMLInputElement;
const dontAsk = () => dialog()!.querySelector('input[type="checkbox"]') as HTMLInputElement;
const note = () => dialog()!.querySelector('.new-project-note.error')?.textContent ?? null;

function button(label: string): HTMLButtonElement {
  const found = [...dialog()!.querySelectorAll<HTMLButtonElement>('button')].find(
    (b) => b.textContent === label
  );
  assert.ok(found, `no button ${label}`);
  return found;
}

function type(text: string) {
  nameInput().value = text;
  nameInput().dispatchEvent(new Event('input', { bubbles: true }));
}

/** The host, and the branches `create` was asked for; `refuse` makes it fail. */
function mount(refuse?: string) {
  const dispose = render(() => <NewBranchDialogHost />, container());
  const created: string[] = [];
  const create = async (name: string) => {
    created.push(name);
    if (refuse) throw refuse;
  };
  return { dispose, created, create };
}

test('the dialog names the main branch and offers the name made up for it', async () => {
  const { dispose, created, create } = mount();
  const answer = askForChatBranch(OFFER, create);
  assert.equal(
    dialog()!.querySelector('.confirm-dialog-title')!.textContent,
    'Create a branch for this chat?'
  );
  assert.match(dialog()!.querySelector('.confirm-dialog-message')!.textContent!, /on main\./);
  assert.equal(nameInput().value, OFFER.suggested);
  assert.equal(dontAsk().checked, false);
  assert.equal(document.activeElement, button('Yes'), 'Enter takes the name as it is');

  button('Yes').click();
  assert.equal(await answer, true, 'asked again next time');
  assert.deepEqual(created, [OFFER.suggested]);
  assert.equal(dialog(), null, 'closed');
  dispose();
});

test('across folders the dialog names each repository the branch is made in, and those that stay', async () => {
  const { dispose, created, create } = mount();
  const answer = askForChatBranch(
    {
      repos: [
        { name: 'api', branch: 'main' },
        { name: 'web', branch: 'master' },
      ],
      staying: [{ name: 'docs', branch: 'feature' }],
      suggested: OFFER.suggested,
    },
    create
  );
  assert.equal(
    dialog()!.querySelector('.confirm-dialog-message')!.textContent!.replace(/\s+/g, ' '),
    'api (main) and web (master) are on their main branches: the new branch is made in each, and the chat’s changes stay off theirs until you merge them. docs stays on feature.'
  );
  button('No').click();
  assert.equal(await answer, true);
  assert.deepEqual(created, [], 'No makes none');
  dispose();
});

test('the name can be changed, and one git can’t take is said and not created', async () => {
  const { dispose, created, create } = mount();
  const answer = askForChatBranch(OFFER, create);
  type('my feature');
  assert.equal(note(), "'my feature' isn't a valid branch name.");
  assert.equal(button('Yes').disabled, true);
  type('  ');
  assert.equal(note(), null, 'no name yet: nothing to say');
  assert.equal(button('Yes').disabled, true);

  type(' my-feature ');
  assert.equal(note(), null);
  assert.equal(button('Yes').disabled, false);
  nameInput().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  assert.equal(await answer, true);
  assert.deepEqual(created, ['my-feature'], 'Enter is Yes, without the spaces around');
  dispose();
});

test('No creates nothing, and “Don’t ask again” goes with either answer', async () => {
  const { dispose, created, create } = mount();
  let answer = askForChatBranch(OFFER, create);
  button('No').click();
  assert.equal(await answer, true);

  answer = askForChatBranch(OFFER, create);
  dontAsk().click();
  button('No').click();
  assert.equal(await answer, false, 'not asked again');
  assert.deepEqual(created, []);

  answer = askForChatBranch(OFFER, create);
  dontAsk().click();
  button('Yes').click();
  assert.equal(await answer, false);
  assert.deepEqual(created, [OFFER.suggested]);
  dispose();
});

test('Escape is No', async () => {
  const { dispose, created, create } = mount();
  const answer = askForChatBranch(OFFER, create);
  nameInput().dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  assert.equal(await answer, true);
  assert.deepEqual(created, []);
  assert.equal(dialog(), null);
  dispose();
});

test('a branch that can’t be created keeps the dialog open, saying why', async () => {
  const refused = `Couldn't create branch '${OFFER.suggested}': a reference with that name already exists`;
  const { dispose, create } = mount(refused);
  let answered = false;
  const answer = askForChatBranch(OFFER, create).then((askAgain) => {
    answered = true;
    return askAgain;
  });
  button('Yes').click();
  await flush();
  assert.equal(note(), refused);
  assert.ok(dialog(), 'still open');
  assert.equal(answered, false, 'the chat still waits');
  assert.equal(button('Yes').disabled, false, 'to try again');

  type('rovik-kind-torvalds-m3p8z1kd');
  assert.equal(note(), null, 'a new name clears it');
  button('No').click();
  assert.equal(await answer, true);
  dispose();
});

test('one question at a time: a chat that would be asked meanwhile isn’t', async () => {
  const { dispose, create } = mount();
  const first = askForChatBranch(OFFER, create);
  assert.equal(
    await askForChatBranch({ ...OFFER, suggested: 'rovik-brave-hopper-q4w8e2r6' }, create),
    true
  );
  assert.equal(nameInput().value, OFFER.suggested, 'the first one stays');
  button('No').click();
  assert.equal(await first, true);
  dispose();
});

test('a dialog still asking when the app goes lets the chat go on', async () => {
  const { dispose, created, create } = mount();
  const answer = askForChatBranch(OFFER, create);
  dispose();
  assert.equal(await answer, true);
  assert.deepEqual(created, []);
});
