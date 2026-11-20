import '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import {
  closePreparationReply,
  visiblePreparationMessages,
  preparationPrompt,
  reopenPreparationReply,
  restorePreparationReply,
  preparationReplyMetadata,
  preparationToRetry,
} from './planPreparation';
import { chatApi } from './chatApi';
import { planPreparation } from '../test/polyglotPlanFixtures';
import type { ChatMessage, ToolMeta } from '../types';

const getMessages = chatApi.getMessages;
afterEach(() => {
  chatApi.getMessages = getMessages;
});

const preparationRequest: ChatMessage = {
  id: 'preparation',
  role: 'user',
  content: 'Prepare the plan',
  meta: { verifiedPreparation: 'preparation-1', planPath: 'plan.md' },
};

test('Retry ignores steering and system-injected requests but never revives an older preparation', async () => {
  const messages: ChatMessage[] = [
    preparationRequest,
    { id: 'ordinary', role: 'user', content: 'A different task' },
    { id: 'steering', role: 'user', content: 'Keep going', meta: { steer: { queuedAt: 1 } } },
    { id: 'check', role: 'user', content: 'Check failed', meta: { harness: 'acceptance' } },
  ];
  chatApi.getMessages = async () => ({ messages, hasOlder: true, hasNewer: false });
  assert.equal(await preparationToRetry('chat'), undefined);
  messages.splice(1, 1);
  assert.deepEqual(await preparationToRetry('chat'), {
    preparationId: 'preparation-1',
    planPath: 'plan.md',
  });
});

test('missing preparation bindings and failed history reads never fall back to an ordinary retry', async () => {
  chatApi.getMessages = async () => ({
    messages: [{ ...preparationRequest, meta: { verifiedPreparation: 'preparation-1' } }],
    hasOlder: false,
    hasNewer: false,
  });
  await assert.rejects(preparationToRetry('chat'), /request is incomplete/);
  chatApi.getMessages = async () => {
    throw new Error('Disconnected');
  };
  await assert.rejects(preparationToRetry('chat'), /Disconnected/);
});

const plan = {
  root: '/project',
  projectId: 'project',
  owner: 'build-host',
  dialogId: 'plan-chat',
  path: '/project/.rovik/plans/repair.md',
};
const context = { ...plan, chatProjectId: 'project' };
const original: ToolMeta = Object.freeze({
  mode: 'autopilot',
  sentContent: 'The clarification with explicit file references.',
  planFile: '/project/.rovik/plans/another.md',
});

test('idle preparing and blocked replies retain their exact private session and user metadata', () => {
  for (const status of ['preparing', 'blocked'] as const) {
    const session = planPreparation(status);
    const meta = preparationReplyMetadata(session, plan, context, original);
    assert.deepEqual(meta, {
      ...original,
      mode: 'agent',
      verifiedPreparation: session.preparationId,
      planPath: plan.path,
    });
    assert.equal(original.mode, 'autopilot', 'the original request is not mutated');
  }
});

test('closed, cancelled, stale, or completed preparation panels do not capture ordinary messages', () => {
  for (const status of ['ready', 'cancelled', 'stale'] as const)
    assert.equal(
      preparationReplyMetadata(planPreparation(status), plan, context, original),
      original
    );
  assert.equal(preparationReplyMetadata(undefined, plan, context, original), original);
  assert.equal(
    preparationReplyMetadata(planPreparation('preparing'), undefined, context, original),
    original
  );
  assert.equal(
    preparationReplyMetadata(
      planPreparation('preparing'),
      { ...plan, path: undefined },
      context,
      original
    ),
    original
  );
});

test('navigation never attaches a preparation from another chat, project, or execution host', () => {
  for (const changed of [
    { dialogId: 'another-chat' },
    { dialogId: null },
    { root: '/another-project' },
    { projectId: 'another-project' },
    { chatProjectId: 'another-project' },
    { owner: 'local' },
    { owner: undefined },
  ])
    assert.equal(
      preparationReplyMetadata(
        planPreparation('preparing'),
        plan,
        { ...context, ...changed },
        original
      ),
      original
    );
});

test('explicit commands and explicit session bindings remain unchanged for service validation', () => {
  for (const meta of [
    { ...original, command: true },
    { ...original, verifiedPreparation: 'a-different-session', planPath: '/another-plan.md' },
    { ...original, verifiedPreparation: 'preparation-1', planPath: plan.path },
  ])
    assert.equal(preparationReplyMetadata(planPreparation('preparing'), plan, context, meta), meta);
});

test('restored active preparation retains its durable identity after an app restart', async () => {
  localStorage.clear();
  for (const state of ['preparing', 'blocked'] as const) {
    const session = planPreparation(state);
    const restored = await restorePreparationReply(
      { root: '/project', dialogId: 'reopened' },
      {},
      () => true,
      async () => ({ preparationId: session.preparationId, planPath: session.plan.path }),
      async () => session
    );
    assert.equal(restored, session);
  }
});

test('explicit Close, completed preparation and newer ordinary requests are not recaptured', async () => {
  localStorage.clear();
  const target = { root: '/project', dialogId: 'closed' };
  const session = planPreparation('preparing');
  const load = async () => ({ preparationId: session.preparationId, planPath: session.plan.path });
  closePreparationReply(target.dialogId, session.preparationId);
  assert.equal(
    await restorePreparationReply(
      target,
      {},
      () => true,
      load,
      async () => {
        throw new Error('must not fetch closed session');
      }
    ),
    undefined
  );
  reopenPreparationReply(target.dialogId, session.preparationId);
  assert.equal(
    await restorePreparationReply(
      target,
      {},
      () => true,
      load,
      async () => session
    ),
    session
  );
  localStorage.clear();
  assert.equal(
    await restorePreparationReply(
      target,
      {},
      () => true,
      async () => undefined,
      async () => {
        throw new Error('must not revive older request');
      }
    ),
    undefined
  );
  for (const state of ['ready', 'cancelled', 'stale'] as const)
    assert.equal(
      await restorePreparationReply(
        target,
        {},
        () => true,
        load,
        async () => planPreparation(state)
      ),
      undefined
    );
  for (const meta of [{ command: true }, { verifiedPreparation: 'explicit' }])
    assert.equal(
      await restorePreparationReply(
        target,
        meta,
        () => true,
        async () => {
          throw new Error('explicit request must bypass history');
        }
      ),
      undefined
    );
});

test('restoration rejects lost context and failed status instead of ordinary dispatch', async () => {
  localStorage.clear();
  const session = planPreparation('preparing');
  const target = { root: '/project', dialogId: 'reopened' };
  const load = async () => ({ preparationId: session.preparationId, planPath: session.plan.path });
  await assert.rejects(
    restorePreparationReply(target, {}, () => false, load),
    /chat or project changed/
  );
  let current = true;
  await assert.rejects(
    restorePreparationReply(
      target,
      {},
      () => current,
      load,
      async () => {
        current = false;
        return session;
      }
    ),
    /chat or project changed/
  );
  await assert.rejects(
    restorePreparationReply(
      target,
      {},
      () => true,
      load,
      async () => {
        throw new Error('Disconnected');
      }
    ),
    /Disconnected/
  );
});

test('private preparation work leaves questions, failures and later conversations visible', () => {
  const request = { ...preparationRequest, content: preparationPrompt('plan.md') };
  const messages: ChatMessage[] = [
    { id: 'plan', role: 'assistant', content: 'Saved plan' },
    request,
    { id: 'tool', role: 'tool', content: 'Internal JSON' },
    { id: 'question', role: 'assistant', content: 'Which target should this use?' },
  ];
  const visible = (running: boolean) =>
    visiblePreparationMessages(messages, 'preparation-1', running).map((m) => m.id);
  assert.deepEqual(visible(true), ['plan']);
  assert.deepEqual(visible(false), ['plan', 'question']);
  messages.push(
    { id: 'error', role: 'assistant', content: '', error: 'Disconnected' },
    { id: 'next', role: 'user', content: 'Another task' },
    { id: 'reply', role: 'assistant', content: 'Done' }
  );
  assert.deepEqual(visible(false), ['plan', 'error', 'next', 'reply']);
  assert.equal(visiblePreparationMessages(messages, undefined, true), messages);
});
