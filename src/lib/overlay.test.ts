// overlay.ts tests (node:test). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { redoGroup } from './overlay';
import type { NotificationOptions } from './notifications';

function deps(redo: () => Promise<string>, askChat: () => Promise<boolean>) {
  const notes: NotificationOptions[] = [];
  const asked: string[] = [];
  const shown: string[] = [];
  return {
    notes,
    asked,
    shown,
    deps: {
      redo,
      askChat: (_: string, request: string, display: string) => {
        asked.push(request);
        shown.push(display);
        return askChat();
      },
      notify: (n: NotificationOptions) => void notes.push(n),
    },
  };
}

test('redo_group_logic: (a) the redo fails → one error, the chat isn’t asked', async () => {
  const d = deps(
    () => Promise.reject(new Error('busy')),
    async () => true
  );
  assert.equal(await redoGroup('chat', 'api', d.deps), false);
  assert.equal(d.notes.length, 1);
  assert.equal(d.notes[0].kind, 'error');
  assert.equal(d.notes[0].title, 'Couldn’t redo the group');
  assert.match(d.notes[0].message ?? '', /busy/);
  assert.deepEqual(d.asked, []);
});

test('redo_group_logic: (b) undone, but asking the chat throws → says so, with the error', async () => {
  const d = deps(
    async () => 'Carry out steps 2 and 3',
    () => Promise.reject(new Error('no model'))
  );
  assert.equal(await redoGroup('chat', 'api', d.deps), false);
  assert.deepEqual(d.asked, ['Carry out steps 2 and 3']);
  assert.equal(d.notes.length, 1);
  assert.equal(d.notes[0].title, 'The group was undone, but the chat couldn’t be asked to redo it');
  assert.match(d.notes[0].message ?? '', /no model/);
});

test('redo_group_logic: (c) undone, but the chat didn’t start → says so too', async () => {
  const d = deps(
    async () => 'Carry out steps 2 and 3',
    async () => false
  );
  assert.equal(await redoGroup('chat', 'api', d.deps), false);
  assert.equal(d.notes.length, 1);
  assert.equal(d.notes[0].title, 'The group was undone, but the chat couldn’t be asked to redo it');
});

test('redo_group_logic: both steps work → no notice', async () => {
  const d = deps(
    async () => 'Carry out steps 2 and 3',
    async () => true
  );
  assert.equal(await redoGroup('chat', 'api', d.deps), true);
  assert.deepEqual(d.notes, []);
});

test('redo_group_logic: (d) the chat shows a redo message, not the conflicts one (round 3)', async () => {
  const d = deps(
    async () => 'Carry out steps 2 and 3',
    async () => true
  );
  assert.equal(await redoGroup('chat', 'api', d.deps), true);
  assert.deepEqual(d.asked, ['Carry out steps 2 and 3']);
  assert.deepEqual(d.shown, ['Redo the group “api” one step at a time.']);
});
