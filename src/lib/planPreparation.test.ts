import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { preparationReplyMetadata } from './planPreparation';
import { planPreparation } from '../test/polyglotPlanFixtures';
import type { ToolMeta } from '../types';

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
