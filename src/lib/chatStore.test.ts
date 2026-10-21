// The chat store (chatStore.ts): a patch streamed into a message merges its
// tool calls and meta into what's there. What didn't change keeps its
// objects, so the rows drawn from them aren't rebuilt (a tool call's block
// opened while the reply streams, a failed check's output opened in the
// verification record). Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createChatStore } from './chatStore';
import type { MessageRow } from './chatApi';
import type { ChatToolCall } from '../types';
import type { CheckResult, VerificationMeta } from './verification';

const call = (id: string, path: string): ChatToolCall => ({
  id,
  name: 'read',
  arguments: JSON.stringify({ path }),
});
const check = (kind: CheckResult['kind'], status: CheckResult['status']): CheckResult => ({
  kind,
  command: `npm run ${kind}`,
  status,
});
const verification = (checks: CheckResult[]): VerificationMeta => ({
  status: 'running',
  round: 1,
  maxRounds: 3,
  checks,
  changedFiles: ['src/a.ts'],
});

const row: MessageRow = {
  id: 'm1',
  dialogId: 'chat',
  seq: 1,
  role: 'assistant',
  content: 'Reading',
  bodyExternal: false,
  bodyBytes: 7,
  toolCalls: [call('c1', 'src/a.ts')],
  toolCallId: null,
  meta: null,
  error: null,
  failed: false,
  finalized: false,
  attachments: [],
  createdAt: 1,
};

(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string) => {
    if (cmd === 'chat_get_messages') return { messages: [row], hasOlder: false, hasNewer: false };
    return null;
  },
};

test('a patch keeps the objects of the tool calls and checks it doesn’t change', async () => {
  const store = createChatStore();
  await store.openChat({ id: 'chat', title: 'Chat', createdAt: 1, updatedAt: 1, messageCount: 1 });
  const message = () => store.messagesOf('chat')[0];
  const first = message().toolCalls![0];

  // The reply streams on: a second call appears, the first is the same.
  store.applyPatch('chat', 'm1', {
    content: 'Reading two',
    toolCalls: [call('c1', 'src/a.ts'), call('c2', 'src/b.ts')],
  });
  assert.equal(message().content, 'Reading two');
  assert.equal(message().toolCalls![0], first);
  assert.equal(message().toolCalls![1].id, 'c2');

  // Its arguments stream in: the same object, updated.
  store.applyPatch('chat', 'm1', {
    toolCalls: [call('c1', 'src/a.ts'), call('c2', 'src/b.ts.bak')],
  });
  assert.equal(message().toolCalls![0], first);
  assert.equal(JSON.parse(message().toolCalls![1].arguments).path, 'src/b.ts.bak');

  // The verification record: each check's result as it finishes.
  store.applyPatch('chat', 'm1', {
    meta: { verification: verification([check('typecheck', 'failed'), check('test', 'running')]) },
  });
  const failed = message().meta!.verification!.checks[0];
  store.applyPatch('chat', 'm1', {
    meta: { verification: verification([check('typecheck', 'failed'), check('test', 'passed')]) },
  });
  assert.equal(message().meta!.verification!.checks[0], failed);
  assert.equal(message().meta!.verification!.checks[1].status, 'passed');

  // Cleared when the patch says so.
  store.applyPatch('chat', 'm1', { toolCalls: undefined });
  assert.equal(message().toolCalls, undefined);
});

test('a stage record the chat doesn’t hold is kept from its patch, and one that ended stays so', async () => {
  const store = createChatStore();
  await store.openChat({ id: 'chat', title: 'Chat', createdAt: 1, updatedAt: 1, messageCount: 1 });
  const stage = (status: 'running' | 'done') => ({
    task: 't',
    name: 'implement' as const,
    status,
    round: 1,
    effort: 'balanced' as const,
  });
  const kept = () => store.state.open.chat.stagePatches ?? {};
  // A remote chat's record past the page held (m9) ends on the host: kept
  // for the strip, which would read the mirror's older copy.
  store.applyPatch('chat', 'm9', { meta: { stage: stage('done') } });
  assert.equal(kept().m9?.status, 'done');
  // A pull that read the host before it ended doesn't take it back.
  store.applyPatch('chat', 'm9', { meta: { stage: stage('running') } });
  assert.equal(kept().m9?.status, 'done');
  // Nor for a record the chat holds.
  store.applyPatch('chat', 'm1', { meta: { stage: stage('running') } });
  store.applyPatch('chat', 'm1', { meta: { stage: stage('done') } });
  store.applyPatch('chat', 'm1', { meta: { stage: stage('running') } });
  assert.equal(store.messagesOf('chat')[0].meta?.stage?.status, 'done');
  // Anything else past the page isn't kept.
  store.applyPatch('chat', 'm8', { content: 'More' });
  assert.equal(kept().m8, undefined);
});

/** A message sent to the run while it works, as the panel queues it. */
const queued = (id: string) => ({
  message: { id, role: 'user' as const, content: `Use ${id}`, finalized: true },
  queuedAt: 1,
});

test('a message sent to a run waits in its chat until the run reads it (steering)', async () => {
  const store = createChatStore();
  await store.openChat({ id: 'chat', title: 'Chat', createdAt: 1, updatedAt: 1, messageCount: 1 });
  store.setSteerable('chat', true);
  assert.ok(store.isSteerable('chat'));
  store.queueSteer('chat', queued('s1'));
  store.queueSteer('chat', queued('s1'));
  store.queueSteer('chat', queued('s2'));
  assert.deepEqual(
    store.queuedOf('chat').map((q) => q.message.id),
    ['s1', 's2'],
    'once each, in order'
  );

  // Read: its row comes, with the same id, and shows where it was stored.
  store.receiveMessages('chat', [
    {
      ...row,
      id: 's1',
      seq: 2,
      role: 'user',
      content: 'Use s1',
      toolCalls: null,
      meta: { steer: { queuedAt: 1 } },
      finalized: true,
    },
  ]);
  assert.deepEqual(
    store.queuedOf('chat').map((q) => q.message.id),
    ['s2']
  );
  const shown = store.messagesOf('chat');
  assert.equal(shown[shown.length - 1]?.id, 's1');

  // Too late for the run: it goes as a new message after it.
  store.markSteerLate('chat', 's2');
  assert.equal(store.queuedOf('chat')[0].late, true);
  // Handed back (or taken back): it leaves the queue.
  assert.equal(store.unqueueSteer('chat', 's2')?.message.id, 's2');
  assert.equal(store.unqueueSteer('chat', 's2'), undefined);
  assert.deepEqual(store.queuedOf('chat'), []);
  store.setSteerable('chat', false);
  assert.ok(!store.isSteerable('chat'));
});
