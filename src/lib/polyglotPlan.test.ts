import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { harnessProviderProblem, implementVerifiedPlan, parseHarnessRates } from './polyglotPlan';
import {
  implementationRun,
  planInspection,
  planProfile,
  savedHarnessPlan,
} from '../test/polyglotPlanFixtures';
const calls: { command: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (command: string, args: any) => {
    calls.push({ command, args });
    return implementationRun();
  },
};
const input = () => ({
  target: { root: '/project', dialogId: 'cloud-chat' },
  plan: savedHarnessPlan,
  inspection: planInspection(),
  untracked: ['src/new.ts'],
  unsavedFiles: [] as string[] | undefined,
  provider: { profile: planProfile, rates: { input: 1, cachedInput: 0, output: 2 } },
  reviewed: true,
});

test('verified implementation binds exact reviewed host plan and source without starting the ordinary agent', async () => {
  calls.length = 0;
  await implementVerifiedPlan(input());
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, 'polyglot_harness');
  assert.equal(calls[0].args.executionHost, 'chat');
  assert.equal(calls[0].args.dialogId, 'cloud-chat');
  assert.deepEqual(calls[0].args.request, {
    action: 'implementPlan',
    planPath: savedHarnessPlan.path,
    expectedPlanDigest: savedHarnessPlan.digest,
    expectedSnapshotDigest: 'sha256:source',
    untracked: ['src/new.ts'],
    savedBuffers: true,
    planCriteriaReviewed: true,
    provider: input().provider,
  });
  assert.equal(window.localStorage.getItem('ephemeral-secret'), null);
});
test('unreviewed criteria, unsaved buffers and incompatible providers prevent model dispatch', async () => {
  calls.length = 0;
  await assert.rejects(implementVerifiedPlan({ ...input(), reviewed: false }), /Review the exact/);
  await assert.rejects(
    implementVerifiedPlan({ ...input(), unsavedFiles: ['src/quota.ts'] }),
    /unsaved/
  );
  await assert.rejects(
    implementVerifiedPlan({ ...input(), unsavedFiles: undefined }),
    /unavailable/
  );
  await assert.rejects(
    implementVerifiedPlan({
      ...input(),
      provider: { ...input().provider, profile: { ...planProfile, apiRoute: 'responses' } },
    }),
    /Chat Completions/
  );
  assert.match(
    harnessProviderProblem({ ...planProfile, baseUrl: 'connector://codex' })!,
    /Connector/
  );
  assert.equal(calls.length, 0);
});
test('provider billing rates distinguish explicit zero from missing or invalid values', () => {
  assert.deepEqual(parseHarnessRates('0', '0', '0'), { input: 0, cachedInput: 0, output: 0 });
  for (const invalid of ['', '-1', '1.5', 'NaN', '9007199254740992'])
    assert.equal(parseHarnessRates(invalid, '0', '1'), null);
});

test('an editor plan uses its exact project-relative path on a remote host and rejects sibling roots', async () => {
  const { verifiedEditorPlanPath } = await import('./plans');
  const { harnessRequest } = await import('./polyglotHarness');
  calls.length = 0;
  const path = verifiedEditorPlanPath(
    '/desktop/project/.rovik/plans/repair.md',
    '/desktop/project'
  );
  await harnessRequest(
    { root: '/desktop/project', dialogId: 'cloud-chat' },
    { action: 'inspectPlan', planPath: path }
  );
  assert.equal(calls[0].args.request.planPath, '.rovik/plans/repair.md');
  assert.equal(calls[0].args.executionHost, 'chat');
  assert.equal(
    verifiedEditorPlanPath('C:\\Project\\.rovik\\plans\\repair.md', 'C:\\Project'),
    '.rovik/plans/repair.md'
  );
  assert.throws(
    () =>
      verifiedEditorPlanPath('/desktop/project-other/.rovik/plans/repair.md', '/desktop/project'),
    /current project/
  );
  assert.throws(
    () =>
      verifiedEditorPlanPath(
        '/desktop/project/../other/.rovik/plans/repair.md',
        '/desktop/project'
      ),
    /current project/
  );
});

test('a strict session blocks project changes and chat ownership changes without silently retargeting', async () => {
  const { verifiedPlanContextProblem } = await import('./polyglotPlan');
  const captured = {
    root: '/project',
    projectId: 'project',
    owner: 'host-a',
    chatProjectId: 'project',
  };
  assert.equal(verifiedPlanContextProblem(captured, captured), null);
  assert.match(
    verifiedPlanContextProblem(captured, { ...captured, projectId: 'other' })!,
    /project changed/
  );
  assert.match(
    verifiedPlanContextProblem(captured, { ...captured, root: '/other' })!,
    /project changed/
  );
  assert.match(
    verifiedPlanContextProblem(captured, { ...captured, owner: 'host-b' })!,
    /execution host changed/
  );
  assert.match(
    verifiedPlanContextProblem(captured, { ...captured, owner: undefined })!,
    /execution host changed/
  );
  assert.match(
    verifiedPlanContextProblem(captured, { ...captured, chatProjectId: 'previous-project' })!,
    /project changed/
  );
});

test('resumed verified runs accept rotated credentials while preserving provider model and endpoint identity', async () => {
  const { boundHarnessProviderProblem } = await import('./polyglotPlan');
  const binding = {
    profileId: planProfile.id,
    model: planProfile.model,
    endpoint: `${planProfile.baseUrl}/chat/completions`,
    rates: { input: 1, cachedInput: 0, output: 2 },
  };
  assert.equal(
    boundHarnessProviderProblem(binding, { ...planProfile, apiKey: 'rotated-secret' }),
    null
  );
  assert.match(boundHarnessProviderProblem(binding, undefined)!, /unavailable/);
  for (const changed of [
    { id: 'other::model' },
    { model: 'different' },
    { baseUrl: 'https://elsewhere.invalid/v1' },
  ]) {
    assert.match(
      boundHarnessProviderProblem(binding, { ...planProfile, ...changed })!,
      /different provider, model or endpoint/
    );
  }
});

test('strict provider review rejects credential-bearing endpoint URLs before their identity is persisted', () => {
  for (const baseUrl of [
    'https://user:secret@example.invalid/v1',
    'https://example.invalid/v1?api_key=secret',
    'https://example.invalid/v1#secret',
  ]) {
    assert.match(harnessProviderProblem({ ...planProfile, baseUrl })!, /API key field/);
  }
});

test('reviewed task dispatch rechecks chat, project, host and editor state after asynchronous preparation', async () => {
  const { dispatchReviewedTask } = await import('./polyglotPlan');
  const captured = { root: '/project', projectId: 'project', owner: 'host-a', dialogId: 'chat' };
  for (const mutation of ['root', 'project', 'host', 'chat', 'unsaved']) {
    let current = {
      root: '/project',
      projectId: 'project',
      chatProjectId: 'project',
      owner: 'host-a',
      dialogId: 'chat',
    };
    let unsaved: string[] = [];
    let finish: (value: string) => void = () => {};
    let dispatched = 0;
    const pending = dispatchReviewedTask({
      captured,
      current: () => current,
      unsavedFiles: () => unsaved,
      prepare: () =>
        new Promise<string>((resolve) => {
          finish = resolve;
        }),
      dispatch: async () => {
        dispatched++;
        return true;
      },
    });
    if (mutation === 'root') current = { ...current, root: '/other-checkout' };
    if (mutation === 'project') current = { ...current, projectId: 'other-project' };
    if (mutation === 'host') current = { ...current, owner: 'host-b' };
    if (mutation === 'chat') current = { ...current, dialogId: 'other-chat' };
    if (mutation === 'unsaved') unsaved = ['src/new.ts'];
    finish('prepared');
    await assert.rejects(pending, /changed|Save or undo/);
    assert.equal(dispatched, 0, mutation);
  }
});
