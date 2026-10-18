// remoteGuards.ts tests (node:test): a remote chat's run isn't resumed
// here (K07), its draft from before a restart waits for its host to catch
// up (K23), and a send that fails after its upload gives the input back,
// however big (K49).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  draftWaits,
  recoverDrafts,
  resumesHere,
  sendKeepingInput,
  type DraftRecovery,
  type KeepInput,
} from './remoteGuards';
import { clearPendingDraft, listPendingDrafts, savePendingDraft } from './pendingDraft';
import { stageRows } from './autopilot';
import type { DraftSegment } from './composerSegments';
import type { PendingDraft } from './pendingDraft';
import type { ChatMessage, ChatPlace } from '../types';

// localStorage as the webview has it: a few megabytes at most.
const QUOTA = 5 * 1024 * 1024;
const stored = new Map<string, string>();
(globalThis as any).localStorage = {
  getItem: (key: string) => stored.get(key) ?? null,
  setItem: (key: string, value: string) => {
    if (value.length > QUOTA) throw new Error('QuotaExceededError');
    stored.set(key, value);
  },
  removeItem: (key: string) => void stored.delete(key),
};

const ON_BUILD: ChatPlace = { kind: 'host', host: 'h1' };

const call = (id: string) => ({ id, name: 'bash', arguments: '{"command":"npm test"}' });
const unanswered = [
  { id: 'u1', role: 'user', content: 'Fix it' },
  {
    id: 'a1',
    role: 'assistant',
    content: '',
    finalized: true,
    toolCalls: [call('c1'), call('c2')],
  },
  { id: 't1', role: 'tool', content: 'ok', tool_call_id: 'c1' },
] as ChatMessage[];

test('a remote chat with unanswered calls starts no resume: its daemon picks it up', () => {
  assert.equal(resumesHere(unanswered, undefined), true, 'a local chat does');
  assert.equal(resumesHere(unanswered, ON_BUILD), false);
  assert.equal(resumesHere(unanswered, { kind: 'moving', host: 'h1' }), false);
  const answered = [...unanswered, { id: 't2', role: 'tool', content: 'ok', tool_call_id: 'c2' }];
  assert.equal(resumesHere(answered as ChatMessage[], undefined), false, 'nothing to pick up');
});

test('a local chat whose Autopilot task is unfinished resumes; a remote one’s daemon does', () => {
  const record = (name: string, status: string) => ({
    id: `s-${name}-${status}`,
    role: 'assistant',
    content: '',
    meta: { stage: { task: 'u1', name, status, round: 1, effort: 'balanced' } },
  });
  const task = [
    { id: 'u1', role: 'user', content: 'Do it', meta: { mode: 'autopilot' } },
    record('plan', 'done'),
  ] as ChatMessage[];
  assert.equal(resumesHere(task, undefined), true, 'cut between two stages');
  assert.equal(resumesHere(task, ON_BUILD), false);
  const delivered = [...task, record('deliver', 'done')] as ChatMessage[];
  assert.equal(resumesHere(delivered, undefined), false, 'over');
  // R03: the page held shows none of its records (a long run since), the
  // task as stored does.
  const page = [{ id: 'a1', role: 'assistant', content: 'Working.' }] as ChatMessage[];
  const stored = stageRows([
    record('plan', 'done'),
    record('implement', 'running'),
  ] as ChatMessage[]);
  assert.equal(resumesHere(page, undefined), false, 'nothing held says so');
  assert.equal(resumesHere(page, undefined, stored), true, 'cut in a long run');
  assert.equal(resumesHere(page, ON_BUILD, stored), false);
  const over = stageRows([record('plan', 'done'), record('deliver', 'done')] as ChatMessage[]);
  assert.equal(resumesHere(page, undefined, over), false, 'over');
});

test('a remote chat’s draft waits until its host has caught up; a moving chat’s always', () => {
  assert.equal(draftWaits(undefined, {}), false);
  assert.equal(draftWaits(ON_BUILD, {}), true);
  assert.equal(draftWaits(ON_BUILD, { h1: true }), false);
  assert.equal(draftWaits({ kind: 'moving', host: 'h1' }, { h1: true }), true);
  assert.equal(draftWaits({ kind: 'movingBack', host: 'h1' }, { h1: true }), true);
});

const draft = (dialogId: string, messageId: string): PendingDraft => ({
  dialogId,
  messageId,
  segments: [{ type: 'text', text: `Message ${messageId}` }],
  createdAt: '2026-10-15T10:00:00.000Z',
});

test('draft recovery leaves a remote chat’s message alone until its host has caught up', async () => {
  const places: Record<string, ChatPlace | undefined> = { remote: ON_BUILD };
  const cleared: string[] = [];
  const failed: string[] = [];
  const asked: string[][] = [];
  const deps = (caughtUp: Record<string, true>): DraftRecovery => ({
    isRunning: (id) => id === 'running',
    placeOf: async (id) => places[id],
    caughtUp,
    answered: async (ids) => {
      asked.push(ids);
      return ids.filter((id) => id === 'm-answered');
    },
    clear: (_dialogId, messageId) => cleared.push(messageId),
    markFailed: (_dialogId, messageId) => failed.push(messageId),
  });
  const drafts = [
    draft('local', 'm-answered'),
    draft('local-2', 'm-unanswered'),
    draft('remote', 'm-remote'),
    draft('running', 'm-running'),
  ];
  const later = await recoverDrafts(drafts, deps({}));
  assert.deepEqual(cleared, ['m-answered']);
  assert.deepEqual(failed, ['m-unanswered']);
  assert.deepEqual(asked, [['m-answered', 'm-unanswered']], 'the mirror isn’t asked about it');
  assert.deepEqual(
    later.map((d) => d.messageId),
    ['m-remote', 'm-running']
  );

  // Its host caught up: judged now, by the mirror that has what it stored.
  const again = await recoverDrafts(later, deps({ h1: true }));
  assert.deepEqual(failed, ['m-unanswered', 'm-remote']);
  assert.deepEqual(
    again.map((d) => d.messageId),
    ['m-running']
  );
});

/** Ten text files of 1 MB each, attached in the input. */
const TEN_FILES: DraftSegment[] = Array.from({ length: 10 }, (_, i) => ({
  type: 'pasted',
  name: `log-${i}.txt`,
  text: `${i}`.repeat(1024 * 1024),
}));

/** `composer_drafts`, and what the send did with it. */
function keeping() {
  const saved = new Map<string, DraftSegment[]>();
  const started: string[] = [];
  const keep: KeepInput = {
    savePending: savePendingDraft,
    clearPending: clearPendingDraft,
    saveDraft: async (dialogId, segments) => void saved.set(dialogId, segments),
    clearDraft: (dialogId) => void saved.delete(dialogId),
    started: (dialogId) => {
      started.push(dialogId);
      saved.delete(dialogId);
    },
  };
  return { saved, started, keep };
}

test('a remote send that fails after the upload gives the input back its ten 1 MB text files', async () => {
  const quiet = console.error;
  console.error = () => {};
  try {
    const { saved, started, keep } = keeping();
    let uploaded = false;
    const input = { dialogId: 'remote', messageId: 'm1', segments: TEN_FILES, remote: true };
    const sending = sendKeepingInput(
      input,
      async () => {
        // The app could close during the upload: the input is saved by then.
        assert.equal(saved.get('remote'), TEN_FILES);
        uploaded = true;
        return false;
      },
      keep
    );
    const back = await sending;
    assert.equal(uploaded, true);
    assert.deepEqual(listPendingDrafts(), [], 'localStorage couldn’t hold them');
    assert.equal(back, TEN_FILES, 'all ten come back');
    assert.equal(saved.get('remote'), TEN_FILES, 'and stay saved until a send starts');
    assert.deepEqual(started, []);
  } finally {
    console.error = quiet;
  }
});

test('a remote send that starts drops the saved input; a local one never saved it', async () => {
  const { saved, started, keep } = keeping();
  const remote = { dialogId: 'remote', messageId: 'm2', segments: TEN_FILES, remote: true };
  const quiet = console.error;
  console.error = () => {};
  try {
    assert.equal(await sendKeepingInput(remote, async () => true, keep), null);
  } finally {
    console.error = quiet;
  }
  assert.deepEqual(started, ['remote']);
  assert.equal(saved.has('remote'), false);

  const small: DraftSegment[] = [{ type: 'text', text: 'Hello' }];
  saved.set('local', small);
  const local = { dialogId: 'local', messageId: 'm3', segments: small, remote: false };
  let savedDuringSend = true;
  const back = await sendKeepingInput(
    local,
    async () => {
      savedDuringSend = saved.has('local');
      return false;
    },
    keep
  );
  assert.equal(savedDuringSend, false, 'a local input is cleared at once, as before');
  assert.equal(back, small, 'and comes back from memory when the send fails');
  assert.deepEqual(listPendingDrafts(), [], 'its pending copy goes with it');
});
