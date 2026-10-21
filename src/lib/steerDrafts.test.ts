// steerDrafts.ts tests (node:test): a message sent to a run is kept until
// the run has read it and gone on, and one handed back is put back into its
// chat's input once, oldest first. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  clearSteerDraft,
  clearSteerDraftsOf,
  clearStoredSteerDrafts,
  listSteerDrafts,
  markSteerDraftsStored,
  returnSteerDrafts,
  saveSteerDraft,
  steerDraft,
  steerDraftsOf,
  takeReturnedSteerDrafts,
} from './steerDrafts';

const stored = new Map<string, string>();
(globalThis as any).localStorage = {
  getItem: (key: string) => stored.get(key) ?? null,
  setItem: (key: string, value: string) => void stored.set(key, value),
  removeItem: (key: string) => void stored.delete(key),
};

const draft = (dialogId: string, messageId: string, at: number) => ({
  dialogId,
  messageId,
  segments: [{ type: 'text' as const, text: messageId }],
  createdAt: new Date(at).toISOString(),
});

test('several drafts in a chat, kept by message, oldest first', () => {
  saveSteerDraft(draft('c', 'b', 2));
  saveSteerDraft(draft('c', 'a', 1));
  saveSteerDraft(draft('other', 'x', 3));
  assert.deepEqual(
    steerDraftsOf('c').map((d) => d.messageId),
    ['a', 'b']
  );
  assert.equal(steerDraft('x')?.dialogId, 'other');
  clearSteerDraft('x');
  assert.equal(steerDraft('x'), undefined);
  clearSteerDraftsOf('c');
  assert.deepEqual(listSteerDrafts(), []);
  assert.equal(stored.size, 0, 'nothing left in storage');
});

test('a read message’s draft goes at the run’s end; a returned one waits for the input', () => {
  saveSteerDraft(draft('c', 'read', 1));
  saveSteerDraft(draft('c', 'back2', 3));
  saveSteerDraft(draft('c', 'back1', 2));
  saveSteerDraft(draft('c', 'waiting', 4));
  markSteerDraftsStored(['read', 'unknown']);
  returnSteerDrafts(['back2', 'back1']);
  clearStoredSteerDrafts('c');
  assert.equal(steerDraft('read'), undefined, 'read and gone on');
  assert.deepEqual(
    takeReturnedSteerDrafts('c').map((d) => d.messageId),
    ['back1', 'back2'],
    'oldest first'
  );
  assert.deepEqual(takeReturnedSteerDrafts('c'), [], 'once');
  assert.deepEqual(
    listSteerDrafts().map((d) => d.messageId),
    ['waiting']
  );
  clearSteerDraftsOf('c');
});
