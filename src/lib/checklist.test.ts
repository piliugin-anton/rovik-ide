// checklist.ts: the live checklist really goes when cleared.
// Run with: npm test
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { clearLiveChecklist, liveChecklist, noteChecklist } from './checklist';

describe('live checklist', () => {
  it('is gone once cleared (a store merges objects, so the key must be unset)', () => {
    noteChecklist('chat-a', [{ title: 'Write it', status: 'done' }]);
    noteChecklist('chat-b', [{ title: 'Other', status: 'pending' }]);
    assert.deepEqual(liveChecklist('chat-a'), [{ title: 'Write it', status: 'done' }]);
    clearLiveChecklist('chat-a');
    // A finished run's list must not reappear when the next run starts.
    assert.equal(liveChecklist('chat-a'), null);
    // Only that chat's list goes.
    assert.deepEqual(liveChecklist('chat-b'), [{ title: 'Other', status: 'pending' }]);
  });

  it('clearing a chat with no list is harmless', () => {
    clearLiveChecklist('never-seen');
    assert.equal(liveChecklist('never-seen'), null);
  });

  it('a new list replaces the old one', () => {
    noteChecklist('chat-c', [{ title: 'Old', status: 'done' }]);
    noteChecklist('chat-c', [{ title: 'New', status: 'in_progress' }]);
    assert.deepEqual(liveChecklist('chat-c'), [{ title: 'New', status: 'in_progress' }]);
  });
});
