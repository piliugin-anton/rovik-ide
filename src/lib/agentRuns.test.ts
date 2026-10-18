// agentRuns.ts tests (node:test): the backend's events and a reloaded page's
// status, as they reach the stores. The backend is answered here: `listen`
// hands over the event handler, and `agent_status` says what's running.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  initAgentRuns,
  isRunning,
  onApiRouteLearned,
  onMessageUnanswered,
  onReasoningLearned,
  toolNotes,
} from './agentRuns';
import { agentChangesFor, recordAgentEdit } from './agentChanges';
import { onFileDeleted, onFileWritten, pendingApproval } from './approvals';
import { chatStore } from './chatStore';
import { liveChecklist } from './checklist';
import { liveClmStatus } from './contextMode';
import { notifications } from './notifications';
import { pendingQuestions } from './questions';
import { childOf, isSubagentChat, liveSubagent, seedRunningSubagent } from './subagents';

const callbacks = new Map<number, (event: unknown) => void>();
let listener: number | null = null;
const status = {
  runs: [{ dialogId: 'lead-3', runId: 'r1' }],
  subagents: [1, 2, 3].map((n) => ({
    dialogId: `kid-3-${n}`,
    parentDialogId: 'lead-3',
    parentCallId: `t${n}`,
    kind: 'scout',
    goal: `Goal ${n}`,
    model: 'Test/m',
    startedAt: 1,
    toolCalls: n,
  })),
  approvals: [],
  questions: [],
  driving: ['task-9'],
};
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    transformCallback: (callback: (event: unknown) => void) => {
      const id = callbacks.size + 1;
      callbacks.set(id, callback);
      return id;
    },
    invoke: async (cmd: string, args: any) => {
      if (cmd === 'plugin:event|listen') {
        if (args.event === 'agent-event') listener = args.handler;
        return 1;
      }
      if (cmd === 'agent_status') return status;
      if (cmd === 'chat_get_messages') return { messages: [], hasOlder: false, hasNewer: false };
      return null;
    },
  },
  // Notifications' timers, which never need to fire here.
  setTimeout: () => 0,
  clearTimeout: () => undefined,
};
// Where the chat store keeps unread chats.
const stored = new Map<string, string>();
(globalThis as any).localStorage = {
  getItem: (key: string) => stored.get(key) ?? null,
  setItem: (key: string, value: string) => void stored.set(key, value),
  removeItem: (key: string) => void stored.delete(key),
};

/** The backend sends an `agent-event`. */
function emit(payload: unknown) {
  assert.ok(listener !== null, 'listening');
  callbacks.get(listener!)!({ event: 'agent-event', id: 1, payload });
}

test('reload_with_0_1_3_running_children', async () => {
  await initAgentRuns();
  // Three children of one lead, from agent_status.
  assert.ok(isRunning('lead-3'));
  // A task's driver at work between two of its runs: the chat is busy.
  assert.ok(isRunning('task-9'));
  assert.ok(!isRunning('task-8'));
  for (const n of [1, 2, 3]) {
    assert.equal(liveSubagent('lead-3', `t${n}`)?.childDialogId, `kid-3-${n}`);
    assert.equal(liveSubagent('lead-3', `t${n}`)?.toolCalls, n);
    assert.equal(childOf('lead-3', `t${n}`), `kid-3-${n}`);
    assert.ok(isSubagentChat(`kid-3-${n}`));
  }
  assert.equal(liveSubagent('lead-3', 't4'), undefined);
  assert.equal(childOf('lead-3', 't4'), null);

  // One.
  seedRunningSubagent({
    dialogId: 'kid-1',
    parentDialogId: 'lead-1',
    parentCallId: 't1',
    kind: 'worker',
    model: 'Test/m',
  });
  assert.equal(liveSubagent('lead-1', 't1')?.childDialogId, 'kid-1');
  assert.equal(childOf('lead-1', 't1'), 'kid-1');
  assert.ok(isSubagentChat('kid-1'));
  assert.equal(liveSubagent('lead-1', 't2'), undefined);

  // None.
  assert.equal(liveSubagent('lead-0', 't1'), undefined);
  assert.equal(childOf('lead-0', 't1'), null);
  assert.ok(!isSubagentChat('lead-0'));

  // Its events go on where the status left off.
  emit({
    type: 'subagentProgress',
    dialogId: 'lead-3',
    toolCallId: 't2',
    toolCalls: 5,
    currentTool: 'read',
    model: 'Test/m',
  });
  assert.equal(liveSubagent('lead-3', 't2')?.toolCalls, 5);
});

test('frontend_closes_a_deleted_tab: a rolled-back new file goes from the editor (P3.4)', async () => {
  await initAgentRuns();
  const deleted: string[] = [];
  const written: string[] = [];
  const stopDeleted = onFileDeleted((path) => deleted.push(path));
  const stopWritten = onFileWritten((path) => written.push(path));
  try {
    recordAgentEdit('/p/src/new.ts', null, 'export {};\n');
    assert.ok(agentChangesFor('/p/src/new.ts'));
    emit({
      type: 'fileWritten',
      absPath: '/p/src/new.ts',
      before: 'export {};\n',
      content: '',
      reviewed: true,
      deleted: true,
    });
    assert.equal(agentChangesFor('/p/src/new.ts'), undefined, 'its highlights are gone');
    assert.deepEqual(deleted, ['/p/src/new.ts'], 'its tab is told to close');
    assert.deepEqual(written, [], 'not reported as written (empty)');

    // A write that isn't a deletion refreshes the tab and is marked.
    emit({
      type: 'fileWritten',
      absPath: '/p/src/a.ts',
      before: 'a\n',
      content: 'b\n',
      reviewed: false,
    });
    assert.deepEqual(written, ['/p/src/a.ts']);
    assert.deepEqual(deleted, ['/p/src/new.ts']);
    assert.ok(agentChangesFor('/p/src/a.ts'));
  } finally {
    stopDeleted();
    stopWritten();
  }
});

test("a task's driver at work keeps its chat busy from one of its runs to the next", async () => {
  await initAgentRuns();
  const chat = 'task-1';
  const busy: boolean[] = [];
  const told = (payload: Record<string, unknown>) => {
    emit({ dialogId: chat, ...payload });
    busy.push(isRunning(chat));
  };
  // The order the backend tells it in: the driver at work before the
  // stage's run ends, until the next stage's run has started.
  told({ type: 'taskDriving', driving: true });
  told({ type: 'runStarted', runId: 'plan' });
  told({ type: 'taskDriving', driving: false });
  told({ type: 'taskDriving', driving: true });
  told({ type: 'runEnded', runId: 'plan', outcome: 'done' });
  told({ type: 'runStarted', runId: 'implement' });
  told({ type: 'taskDriving', driving: false });
  told({ type: 'taskDriving', driving: true });
  told({ type: 'runEnded', runId: 'implement', outcome: 'done' });
  // Its delivery, the IDE's own: no run, the driver at work, then done.
  told({ type: 'taskDriving', driving: false });
  assert.deepEqual(busy, [true, true, true, true, true, true, true, true, true, false]);
});

/**
 * test-fixtures/agent-events.json: one event of each type, as the backend
 * sends it. The backend's tests read each into its `AgentEvent` and write it
 * back unchanged (rovik-protocol), so the two sides agree on every shape.
 */
type FixtureEvent = { type: string } & Record<string, any>;
const fixtures = JSON.parse(
  readFileSync(join(process.cwd(), 'test-fixtures/agent-events.json'), 'utf8')
) as { events: FixtureEvent[] };

test("the backend's events, as the shared fixtures have them, reach their stores", async () => {
  await initAgentRuns();
  await chatStore.openChat({
    id: 'fx-chat',
    title: 'Fixtures',
    createdAt: 1,
    updatedAt: 1,
    messageCount: 0,
  });
  const written: [string, string][] = [];
  const learned: string[] = [];
  const routes: [string, string][] = [];
  const unanswered: [string, string][] = [];
  const stops = [
    onFileWritten((path, content) => written.push([path, content])),
    onReasoningLearned((l) => learned.push(l.profileId)),
    onApiRouteLearned((profileId, route) => routes.push([profileId, route])),
    onMessageUnanswered((dialogId, messageId) => unanswered.push([dialogId, messageId])),
  ];
  const message = (id: string) => chatStore.messagesOf('fx-chat').find((m) => m.id === id);
  const last = <T>(list: T[]): T | undefined => list[list.length - 1];
  // What each type of event changes, checked right after it.
  const effects: Record<string, (e: FixtureEvent) => void> = {
    runStarted: (e) => assert.ok(isRunning(e.dialogId)),
    // Over, but for its task's driver at work (told before the run's end).
    runEnded: (e) => assert.equal(isRunning(e.dialogId), !!chatStore.state.driving[e.dialogId]),
    taskDriving: (e) => {
      assert.equal(!!chatStore.state.driving[e.dialogId], e.driving);
      if (e.driving) assert.ok(isRunning(e.dialogId));
    },
    messagesAdded: (e) => {
      for (const m of e.messages) assert.equal(message(m.id)?.content, m.content, m.id);
    },
    messagePatched: (e) => {
      assert.equal(message(e.messageId)?.content, e.patch.content);
      assert.deepEqual(message(e.messageId)?.status, e.patch.status ?? undefined);
    },
    toolNote: (e) => assert.equal(toolNotes(e.dialogId)?.[e.toolCallId], e.note ?? undefined),
    approvalRequested: (e) =>
      assert.equal(
        pendingApproval(e.request.dialogId, e.request.toolCallId)?.edit?.absPath,
        e.request.edit.absPath
      ),
    approvalSettled: (e) => assert.equal(pendingApproval(e.dialogId, e.toolCallId), undefined),
    fileWritten: (e) => assert.deepEqual(last(written), [e.absPath, e.content]),
    questionsAsked: (e) =>
      assert.deepEqual(
        pendingQuestions(e.request.dialogId, e.request.toolCallId)?.questions,
        e.request.questions
      ),
    questionsSettled: (e) => assert.equal(pendingQuestions(e.dialogId, e.toolCallId), undefined),
    checklist: (e) => assert.deepEqual(liveChecklist(e.dialogId), e.items),
    contextUpdated: (e) => assert.deepEqual(liveClmStatus(e.dialogId), e.status),
    subagentStarted: (e) =>
      assert.equal(liveSubagent(e.dialogId, e.toolCallId)?.childDialogId, e.childDialogId),
    subagentProgress: (e) =>
      assert.equal(liveSubagent(e.dialogId, e.toolCallId)?.toolCalls, e.toolCalls),
    subagentEnded: (e) => assert.equal(liveSubagent(e.dialogId, e.toolCallId)?.status, e.status),
    reasoningLearned: (e) => assert.equal(last(learned), e.profileId),
    apiRouteLearned: (e) => assert.deepEqual(last(routes), [e.profileId, e.route]),
    notice: (e) =>
      assert.ok(notifications().some((n) => n.title === e.title && n.message === e.message)),
    userMessageSettled: (e) => assert.deepEqual(last(unanswered), [e.dialogId, e.messageId]),
  };
  try {
    for (const event of fixtures.events) {
      const effect = effects[event.type];
      assert.ok(effect, `nothing checks a ${event.type} event`);
      emit(event);
      effect(event);
    }
    assert.deepEqual(
      new Set(fixtures.events.map((e) => e.type)),
      new Set(Object.keys(effects)),
      'every check has its event'
    );
    // The driver done too: nothing goes on in the chat.
    emit({ type: 'taskDriving', dialogId: 'fx-chat', driving: false });
    assert.ok(!isRunning('fx-chat'));
  } finally {
    for (const stop of stops) stop();
  }
});
