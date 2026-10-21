// Which messages start a request (lib/requests.ts): the user's own, not the
// IDE's, and not one sent to the run while it worked. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isRequestMessage, isSteerMessage, steersOfRequest } from './requests';
import type { ChatMessage } from '../types';

const user = (id: string, meta?: ChatMessage['meta'], failed = false): ChatMessage => ({
  id,
  role: 'user',
  content: id,
  meta,
  ...(failed ? { failed } : {}),
});
const reply = (id: string): ChatMessage => ({ id, role: 'assistant', content: id });

test('a request is the user’s message, not the IDE’s nor one sent to a running agent', () => {
  assert.ok(isRequestMessage(user('u')));
  assert.ok(!isRequestMessage(user('h', { harness: 'checklist' })));
  assert.ok(!isRequestMessage(user('s', { steer: { queuedAt: 1 } })));
  assert.ok(!isRequestMessage(reply('a')));
  assert.ok(isSteerMessage(user('s', { steer: { queuedAt: 1 } })));
  assert.ok(!isSteerMessage(user('u')));
});

test('the steering messages of the current request, oldest first, delivered only', () => {
  const steer = (id: string, failed = false) => user(id, { steer: { queuedAt: 1 } }, failed);
  const messages = [
    user('u1'),
    reply('a1'),
    steer('old'),
    user('u2'),
    reply('a2'),
    steer('s1'),
    user('h', { harness: 'verification' }),
    steer('s2'),
    steer('gone', true),
    reply('a3'),
  ];
  assert.deepEqual(
    steersOfRequest(messages).map((m) => m.id),
    ['s1', 's2']
  );
  assert.deepEqual(steersOfRequest([user('u')]), []);
});
