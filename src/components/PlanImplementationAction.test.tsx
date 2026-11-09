import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import PlanImplementationAction from './PlanImplementationAction';
import { initAgentRuns, onShowChat } from '../lib/agentRuns';
import { chatStore } from '../lib/chatStore';
import { refreshPlanImplementations, type PlanImplementation } from '../lib/planImplementations';

const file = '/p/.rovik/plans/contact.md';
const calls: { cmd: string; args: any }[] = [];
let record: PlanImplementation | null = null;
let lookupError = false;
let callbackId = 0;
let eventHandler: ((event: { payload: unknown }) => void) | undefined;
const callbacks = new Map<number, (event: any) => void>();
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: any) => void) => {
    callbacks.set(++callbackId, callback);
    return callbackId;
  },
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    if (cmd === 'plugin:event|listen') {
      if (args.event === 'agent-event') eventHandler = callbacks.get(args.handler);
      return 1;
    }
    if (cmd === 'desktop_event_status') return { runs: [], approvals: [], questions: [] };
    if (cmd === 'chat_read_plan') return '# Contact form\n';
    if (cmd === 'chat_plan_implementation') {
      if (lookupError) throw new Error('Database unavailable');
      return record;
    }
    if (cmd === 'chat_get_dialog')
      return {
        id: 'implementation-chat',
        title: 'Contact form',
        createdAt: 1,
        updatedAt: 1,
        messageCount: 1,
      };
    if (cmd === 'chat_get_messages') return { messages: [], hasOlder: false, hasNewer: false };
    return null;
  },
};

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const evidence = (status: PlanImplementation['status']): PlanImplementation => ({
  dialogId: 'implementation-chat',
  messageId: 'request',
  runId: 'implementation-run',
  status,
});

test('an implemented plan replaces the action with a chat link that activates its chat and tab', async () => {
  record = evidence('implemented');
  const shown: unknown[] = [];
  const stop = onShowChat((target) => shown.push(target));
  const root = container();
  const dispose = render(
    () => <PlanImplementationAction path={file} content="# Contact form\n" canImplement />,
    root
  );
  try {
    await tick();
    assert.match(root.textContent ?? '', /Implemented in chat/);
    assert.doesNotMatch(root.textContent ?? '', /Implement this plan/);
    root.querySelector<HTMLButtonElement>('.link-button')!.click();
    await tick();
    assert.equal(chatStore.state.activeId, 'implementation-chat');
    assert.deepEqual(shown, [{ dialogId: 'implementation-chat' }]);
  } finally {
    dispose();
    stop();
  }
});

test('editing a completed plan queries the new revision and makes it implementable', async () => {
  record = evidence('implemented');
  const [text, setText] = createSignal('# Contact form\n');
  const root = container();
  let implemented = 0;
  const dispose = render(
    () => (
      <PlanImplementationAction
        path={file}
        content={text()}
        canImplement
        onImplement={() => implemented++}
      />
    ),
    root
  );
  try {
    await tick();
    assert.match(root.textContent ?? '', /Implemented in chat/);
    record = null;
    setText('# Updated contact form\n');
    await tick();
    assert.match(root.textContent ?? '', /Implement this plan/);
    assert.equal(
      calls.filter((c) => c.cmd === 'chat_plan_implementation').slice(-1)[0].args.content,
      text()
    );
    root.querySelector<HTMLButtonElement>('.primary')!.click();
    assert.equal(implemented, 1);
  } finally {
    dispose();
  }
});

test('an active matching run shows implementing and a different later run does not', async () => {
  await initAgentRuns();
  record = evidence('implementing');
  eventHandler!({
    payload: { type: 'runStarted', dialogId: 'implementation-chat', runId: 'implementation-run' },
  });
  const root = container();
  const dispose = render(
    () => <PlanImplementationAction path={file} content="# Contact form\n" canImplement />,
    root
  );
  try {
    await tick();
    assert.match(root.textContent ?? '', /Implementing in chat/);
    assert.equal(root.querySelector('.primary'), null);
    eventHandler!({
      payload: { type: 'runStarted', dialogId: 'implementation-chat', runId: 'unrelated-run' },
    });
    await tick();
    assert.match(root.textContent ?? '', /Implementation interrupted in chat/);
    assert.ok(root.querySelector('.primary'));
  } finally {
    dispose();
    chatStore.setRunning('implementation-chat', false);
  }
});

test('review-needed and interrupted attempts keep their chat link and offer implementation again', async () => {
  for (const status of ['review-needed', 'interrupted'] as const) {
    record = evidence(status);
    const root = container();
    const dispose = render(
      () => <PlanImplementationAction path={file} dialogId="planner" canImplement />,
      root
    );
    try {
      await tick();
      assert.ok(root.querySelector('.link-button'));
      assert.ok(root.querySelector('.primary'));
      assert.match(
        root.textContent ?? '',
        status === 'review-needed' ? /review needed/ : /interrupted/
      );
      assert.ok(calls.some((c) => c.cmd === 'chat_read_plan' && c.args.dialogId === 'planner'));
    } finally {
      dispose();
    }
  }
});

test('run completion refreshes the visible saved status', async () => {
  record = null;
  const root = container();
  const dispose = render(
    () => <PlanImplementationAction path={file} content="# Contact form\n" canImplement />,
    root
  );
  try {
    await tick();
    record = evidence('implemented');
    eventHandler!({
      payload: {
        type: 'runEnded',
        dialogId: 'implementation-chat',
        runId: 'unrelated-run',
        outcome: 'done',
      },
    });
    await tick();
    assert.match(root.textContent ?? '', /Implemented in chat/);
  } finally {
    dispose();
  }
});

test('a status lookup failure is visible and can be retried without crashing the view', async () => {
  lookupError = true;
  const root = container();
  const dispose = render(
    () => <PlanImplementationAction path={file} content="# Contact form\n" />,
    root
  );
  try {
    await tick();
    assert.match(root.textContent ?? '', /Implementation status unavailable/);
    lookupError = false;
    record = evidence('implemented');
    root.querySelector<HTMLButtonElement>('.link-button')!.click();
    await tick();
    assert.match(root.textContent ?? '', /Implemented in chat/);
  } finally {
    lookupError = false;
    dispose();
    refreshPlanImplementations();
  }
});
