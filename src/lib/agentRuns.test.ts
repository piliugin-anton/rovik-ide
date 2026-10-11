// agentRuns.ts tests (node:test): the backend's events and a reloaded page's
// status, as they reach the stores. The backend is answered here: `listen`
// hands over the event handler, and `agent_status` says what's running.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initAgentRuns, isRunning } from './agentRuns';
import { agentChangesFor, recordAgentEdit } from './agentChanges';
import { onFileDeleted, onFileWritten } from './approvals';
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
      return null;
    },
  },
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
