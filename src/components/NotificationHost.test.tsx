// Component test: a notification keeps its elements while it's updated (a
// download's progress), so its buttons stay clickable. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import NotificationHost from './NotificationHost';
import { notify } from '../lib/notifications';

test('an updated notification keeps its close and action buttons', () => {
  const root = container();
  const dispose = render(() => <NotificationHost />, root);
  let ran = 0;
  const handle = notify({
    title: 'Downloading',
    progress: 0,
    actions: [{ label: 'Cancel', run: () => ran++, keepOpen: true }],
  });
  const close = root.querySelector('.notification-close');
  const action = root.querySelector('.notification-action');
  assert.ok(close && action);
  for (const progress of [0.25, 0.5, 0.75]) {
    handle.update({ progress, message: `${progress * 100}%` });
    assert.equal(root.querySelector('.notification-close'), close);
    assert.equal(root.querySelector('.notification-action'), action);
  }
  assert.equal(root.querySelector('.notification-message')?.textContent, '75%');
  assert.equal(
    (root.querySelector('.notification-progress-fill') as HTMLElement).style.width,
    '75%'
  );
  (action as HTMLButtonElement).click();
  assert.equal(ran, 1);
  (close as HTMLButtonElement).click();
  assert.equal(root.querySelectorAll('.notification').length, 0);
  dispose();
});
