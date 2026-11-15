import { container } from '../test/dom';
import { flush, installTauri } from '../test/chatPanel';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import ChatPanel from './ChatPanel';
import { chatStore } from '../lib/chatStore';
import type { MessageRow } from '../lib/chatApi';
import type { PlanPreparation } from '../lib/planPreparation';
import {
  implementationRun,
  planPreparation,
  planProfile,
  savedHarnessPlan,
} from '../test/polyglotPlanFixtures';

const cleanups: (() => void)[] = [];
afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
  chatStore.resetExecution();
});

async function page(
  status?: PlanPreparation['status'],
  options: {
    paginated?: boolean;
    promoted?: boolean;
    runningImplementation?: boolean;
    deferHistory?: boolean;
    skipRetry?: boolean;
    interrupted?: boolean;
    closedPreparation?: boolean;
  } = {}
) {
  localStorage.clear();
  installTauri([]);
  const invoke = (window as any).__TAURI_INTERNALS__.invoke;
  const calls: { command: string; args: any }[] = [];
  const dialog = {
    id: `${options.interrupted ? 'interrupted' : 'retry'}-${status ?? 'ordinary'}${options.closedPreparation ? '-closed' : ''}`,
    title: 'Plan',
    createdAt: 1,
    updatedAt: 2,
    messageCount: 2,
  };
  if (options.closedPreparation)
    localStorage.setItem(`rovik.preparation.closed.${dialog.id}`, 'preparation-1');
  const request: MessageRow = {
    id: 'request',
    dialogId: dialog.id,
    seq: 1,
    role: 'user',
    content: 'Prepare my plan',
    meta: status
      ? { mode: 'agent', verifiedPreparation: 'preparation-1', planPath: savedHarnessPlan.path }
      : { mode: 'agent' },
    bodyExternal: false,
    bodyBytes: 15,
    toolCalls: null,
    toolCallId: null,
    error: null,
    failed: false,
    finalized: true,
    attachments: [],
    createdAt: 1,
  };
  const failure: MessageRow = {
    ...request,
    id: 'failure',
    seq: 2,
    role: 'assistant',
    content: '',
    error: 'Interrupted preparation',
    meta: { failure: { kind: 'network', message: 'Disconnected', attempts: 1 } },
  };
  const interruptedCall = (id: string, seq: number): MessageRow => ({
    ...failure,
    id,
    seq,
    error: null,
    meta: null,
    toolCalls: [
      {
        id,
        name: 'prepare_verified_plan',
        arguments: '{"input_path":".rovik-preparation/submission.json"}',
      },
    ],
  });
  const messages = options.interrupted
    ? [request, interruptedCall('old-call', 2), interruptedCall('latest-call', 3)]
    : [request, failure];
  let finishHistory: (() => void) | undefined;
  (window as any).__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
    calls.push({ command, args });
    if (command === 'load_ai_settings')
      return JSON.stringify({
        version: 2,
        providers: [
          {
            id: 'provider',
            name: 'Worker',
            baseUrl: planProfile.baseUrl,
            apiKey: '',
            stream: false,
            models: [{ id: 'worker', enabled: true, harnessRates: planProfile.harnessRates }],
          },
        ],
        active: { providerId: 'provider', modelId: 'worker' },
      });
    if (command === 'chat_list_dialogs') return { items: [dialog], nextCursor: null };
    if (command === 'chat_get_messages') {
      const result =
        options.paginated && args.limit === 500
          ? args.beforeSeq === undefined
            ? { messages: [failure], hasOlder: true, hasNewer: false }
            : { messages: [request], hasOlder: false, hasNewer: false }
          : { messages, hasOlder: false, hasNewer: false };
      if (options.deferHistory && args.limit === 500)
        return new Promise((resolve) => {
          finishHistory = () => resolve(result);
        });
      return result;
    }
    if (command === 'agent_start') return 'retried-run';
    if (command === 'desktop_event_status') return { runs: [], approvals: [], questions: [] };
    if (command === 'polyglot_harness') {
      if (args.request.action === 'preparationStatus')
        return {
          ...planPreparation(status),
          ...(options.promoted ? { runId: 'implementation-1' } : {}),
        };
      if (args.request.action === 'beginPlanPreparation')
        return { ...planPreparation('preparing'), preparationId: 'preparation-2' };
      if (args.request.action === 'status')
        return implementationRun(options.runningImplementation ? 'running' : 'blocked');
      assert.fail(`Unexpected harness operation: ${args.request.action}`);
    }
    return invoke(command, args);
  };
  const [rootPath, setRootPath] = createSignal('/project');
  const root = container();
  cleanups.push(
    render(
      () => (
        <ChatPanel
          rootPath={rootPath}
          projectId={() => `project-${dialog.id}`}
          activeFileContext={() => null}
          files={() => []}
          unsavedFiles={() => []}
        />
      ),
      root
    )
  );
  await flush();
  await chatStore.openChat(dialog);
  await flush();
  const retry = [...root.querySelectorAll('button')].find(
    (button) => button.textContent?.trim() === 'Retry'
  );
  if (!options.interrupted) assert.ok(retry, root.textContent ?? 'Retry is visible');
  assert.equal(
    calls.filter((call) => call.command === 'agent_start').length,
    options.interrupted && !status ? 1 : 0
  );
  if (!options.skipRetry) {
    retry!.click();
    await flush();
  }
  return {
    dialog,
    messages,
    root,
    calls,
    setRootPath,
    finishHistory: () => {
      assert.ok(finishHistory);
      finishHistory();
    },
    starts: () =>
      calls.filter((call) => call.command === 'agent_start').map((call) => call.args.request),
    operations: () =>
      calls.filter((call) => call.command === 'polyglot_harness').map((call) => call.args.request),
  };
}

for (const status of ['preparing', 'blocked', 'cancelled', 'stale', 'ready'] as const) {
  test(`opening interrupted ${status} preparation restores its controls without replaying tool calls`, async () => {
    const result = await page(status, {
      interrupted: true,
      skipRetry: true,
      paginated: status === 'blocked',
    });
    assert.deepEqual(result.starts(), []);
    assert.deepEqual(
      result.operations().map((operation) => operation.action),
      ['preparationStatus']
    );
    assert.ok(result.root.querySelector('[aria-label="Plan preparation"]'));
    assert.equal(result.root.querySelectorAll('.tool-call-spinner').length, 0);
    if (status === 'preparing') {
      assert.match(result.root.textContent!, /Preparation paused/);
      const button = [...result.root.querySelectorAll('button')].find(
        (button) => button.textContent?.trim() === 'Continue preparation'
      );
      assert.ok(button);
      button.click();
      await flush();
      assert.equal(result.starts().length, 1);
      assert.equal(result.starts()[0].start.message.meta.verifiedPreparation, 'preparation-1');
    }
  });
}

test('opening interrupted ordinary work still offers its pending calls through resume', async () => {
  const result = await page(undefined, { interrupted: true, skipRetry: true });
  assert.equal(result.starts().length, 1);
  assert.equal(result.starts()[0].start.kind, 'resume');
  assert.deepEqual(result.operations(), []);
});

test('opening an explicitly closed preparation neither reopens its card nor replays its tools', async () => {
  const result = await page('preparing', {
    interrupted: true,
    skipRetry: true,
    closedPreparation: true,
  });
  assert.deepEqual(result.starts(), []);
  assert.deepEqual(result.operations(), []);
  assert.equal(result.root.querySelector('[aria-label="Plan preparation"]'), null);
  assert.equal(result.root.querySelectorAll('.tool-call-spinner').length, 0);
});

for (const status of ['cancelled', 'stale'] as const) {
  test(`chat Retry replaces a ${status} preparation with a fresh session before starting the agent`, async () => {
    const result = await page(status);
    assert.deepEqual(
      result.operations().map((request) => request.action),
      ['preparationStatus', 'beginPlanPreparation']
    );
    assert.equal(result.operations()[0].preparationId, 'preparation-1');
    assert.equal(result.operations()[1].planPath, savedHarnessPlan.path);
    assert.equal(result.starts().length, 1);
    assert.equal(result.starts()[0].start.kind, 'send');
    assert.equal(result.starts()[0].start.message.meta.verifiedPreparation, 'preparation-2');
  });
}

for (const status of ['preparing', 'blocked'] as const) {
  test(`chat Retry resumes the same ${status} preparation without recreating its workspace`, async () => {
    const result = await page(status, { paginated: true });
    assert.deepEqual(
      result.operations().map((request) => request.action),
      ['preparationStatus']
    );
    assert.equal(result.starts().length, 1);
    assert.equal(result.starts()[0].start.message.meta.verifiedPreparation, 'preparation-1');
    assert.ok(
      result.calls.some((call) => call.command === 'chat_get_messages' && call.args.beforeSeq === 2)
    );
  });
}

test('chat Retry opens a completed preparation for review without model calls or implementation approval', async () => {
  const result = await page('ready');
  assert.deepEqual(
    result.operations().map((request) => request.action),
    ['preparationStatus']
  );
  assert.deepEqual(result.starts(), []);
  assert.match(result.root.textContent!, /Ready to implement/);
});

test('chat Retry reopens an existing implementation without preparing or starting another run', async () => {
  const result = await page('ready', { promoted: true });
  assert.deepEqual(
    result.operations().map((request) => request.action),
    ['preparationStatus', 'status']
  );
  assert.deepEqual(result.starts(), []);
  assert.match(result.root.textContent!, /Implementation blocked/);
});

test('ordinary chat Retry still retries the delivered request', async () => {
  const result = await page();
  assert.deepEqual(result.operations(), []);
  assert.equal(result.starts().length, 1);
  assert.equal(result.starts()[0].start.kind, 'retry');
});

test('an implementation in another chat does not block an ordinary Retry', async () => {
  const result = await page('ready', { promoted: true, runningImplementation: true });
  assert.match(result.root.textContent!, /Implementation running/);
  const other = { ...result.dialog, id: 'ordinary-other-chat' };
  const invoke = (window as any).__TAURI_INTERNALS__.invoke;
  (window as any).__TAURI_INTERNALS__.invoke = (command: string, args: any) => {
    if (command === 'chat_get_messages' && args.dialogId === other.id)
      return Promise.resolve({
        messages: result.messages.map((message) => ({
          ...message,
          id: `other-${message.id}`,
          dialogId: other.id,
          meta: message.role === 'user' ? { mode: 'agent' } : message.meta,
        })),
        hasOlder: false,
        hasNewer: false,
      });
    return invoke(command, args);
  };
  await chatStore.openChat(other);
  await flush();
  const retry = [...result.root.querySelectorAll('button')].find(
    (button) => button.textContent?.trim() === 'Retry'
  );
  assert.ok(retry);
  retry.click();
  await flush();
  assert.equal(result.starts().length, 1);
  assert.equal(result.starts()[0].dialogId, other.id);
  assert.equal(result.starts()[0].start.kind, 'retry');
  assert.deepEqual(
    result.operations().map((request) => request.action),
    ['preparationStatus', 'status']
  );
});

test('leaving the project while Retry reads history prevents preparation and agent calls', async () => {
  const result = await page('cancelled', { deferHistory: true });
  result.setRootPath('/another-project');
  result.finishHistory();
  await flush();
  assert.deepEqual(result.operations(), []);
  assert.deepEqual(result.starts(), []);
});

test('first ordinary follow-up after reopening restores the private preparation before dispatch', async () => {
  const result = await page('blocked', { skipRetry: true });
  const input = result.root.querySelector('.chat-input-contenteditable') as HTMLElement;
  input.textContent = 'Correct the submission and continue';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
  (result.root.querySelector('.chat-send-button') as HTMLButtonElement).click();
  await flush();
  assert.equal(result.starts().length, 1);
  assert.equal(result.starts()[0].start.message.meta.verifiedPreparation, 'preparation-1');
  assert.equal(result.starts()[0].start.message.content, 'Correct the submission and continue');
  assert.equal(result.starts()[0].mode, 'agent');
  assert.ok(!result.operations().some((operation) => operation.action === 'beginPlanPreparation'));
});
