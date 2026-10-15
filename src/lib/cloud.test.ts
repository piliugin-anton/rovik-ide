import { test } from 'node:test';
import assert from 'node:assert/strict';
import { barChanged, type CloudEvent } from './cloud';

const changed: CloudEvent = { type: 'changed', dialogId: 'chat-1' };

test("a remote chat's changes looking different refresh its own bar", () => {
  assert.equal(barChanged(changed, 'chat-1'), true);
  assert.equal(barChanged(changed, 'chat-2'), false);
  assert.equal(barChanged(changed, null), false);
});

test('transfers leave the bar alone', () => {
  const pushing: CloudEvent = { type: 'pushing', dialogId: 'chat-1', project: 'p', sent: 10 };
  const uploading: CloudEvent = {
    type: 'uploading',
    dialogId: 'chat-1',
    messageId: 'm',
    sent: 1,
    total: 2,
  };
  assert.equal(barChanged(pushing, 'chat-1'), false);
  assert.equal(barChanged(uploading, 'chat-1'), false);
});
