// Component test: the confirmation dialog's three ways out (chooseDialog),
// and confirmDialog's two. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import { chooseDialog, ConfirmDialog, confirmDialog, ConfirmDialogHost } from './ConfirmDialog';

function button(label: string): HTMLButtonElement {
  const found = [...document.querySelectorAll<HTMLButtonElement>('.confirm-dialog button')].find(
    (b) => b.textContent === label
  );
  assert.ok(found, `no button ${label}`);
  return found;
}

test('a third choice is told apart from confirm and cancel', async () => {
  const dispose = render(() => <ConfirmDialogHost />, container());
  const options = { title: 'Save?', confirmLabel: 'Save', alternateLabel: 'Don’t save' };

  let choice = chooseDialog(options);
  button('Don’t save').click();
  assert.equal(await choice, 'alternate');

  choice = chooseDialog(options);
  button('Save').click();
  assert.equal(await choice, 'confirm');

  choice = chooseDialog(options);
  button('Cancel').click();
  assert.equal(await choice, 'cancel');

  // Without a third choice there's no third button, and a plain yes/no.
  const plain = confirmDialog({ title: 'Delete?', confirmLabel: 'Delete' });
  assert.equal(document.querySelectorAll('.confirm-dialog-alternate').length, 0);
  button('Delete').click();
  assert.equal(await plain, true);
  dispose();
});

test('fields under the message are in the Tab loop, and confirm waits for them', () => {
  const [ready, setReady] = createSignal(false);
  const dispose = render(
    () => (
      <ConfirmDialog
        title="Rename?"
        confirmLabel="Rename"
        confirmDisabled={!ready()}
        onConfirm={() => {}}
        onCancel={() => {}}
      >
        <input class="rename-field" />
      </ConfirmDialog>
    ),
    container()
  );
  const field = document.querySelector<HTMLInputElement>('.confirm-dialog input.rename-field');
  assert.ok(field, 'the field is in the dialog');
  assert.equal(button('Rename').disabled, true);
  setReady(true);
  assert.equal(button('Rename').disabled, false);

  const tab = (from: HTMLElement, shiftKey = false) =>
    from.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Tab', shiftKey, bubbles: true, cancelable: true })
    );
  button('Rename').focus();
  tab(button('Rename'));
  assert.equal(document.activeElement, field, 'from the last button round to the field');
  tab(field, true);
  assert.equal(document.activeElement, button('Rename'), 'and back');
  dispose();
});
