// Component test: the confirmation dialog's three ways out (chooseDialog),
// and confirmDialog's two. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { chooseDialog, confirmDialog, ConfirmDialogHost } from './ConfirmDialog';

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
