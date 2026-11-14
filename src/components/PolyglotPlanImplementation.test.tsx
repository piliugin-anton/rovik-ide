import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import PolyglotPlanImplementation from './PolyglotPlanImplementation';
import {
  implementationRun,
  planPreparation,
  planProfile,
  savedHarnessPlan,
} from '../test/polyglotPlanFixtures';
import type { HarnessApproval } from '../lib/polyglotHarness';

let calls: { command: string; args: any }[] = [];
let respond: (request: any) => unknown;
const cleanups: (() => void)[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (command: string, args: any) => {
    calls.push({ command, args });
    if (command !== 'polyglot_harness') throw new Error(`Unexpected ${command}`);
    return respond(args.request);
  },
};
const flush = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};
afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
});
function button(root: HTMLElement, label: string) {
  const value = [...root.querySelectorAll('button')].find(
    (entry) => entry.textContent?.trim() === label
  );
  assert.ok(value, label);
  return value;
}
function reset() {
  calls = [];
  respond = (request) =>
    request.action === 'beginPlanPreparation' ? planPreparation() : implementationRun();
}
async function page(unsavedFiles: readonly string[] | undefined = []) {
  reset();
  const root = container();
  cleanups.push(
    render(
      () => (
        <PolyglotPlanImplementation
          target={{ root: '/project', dialogId: 'cloud' }}
          planPath=".rovik/plans/repair.md"
          profile={planProfile}
          unsavedFiles={unsavedFiles}
          executionLabel="Build host"
          onClose={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  return root;
}
test('automatic preparation keeps its review controls outside the scrollable evidence', async () => {
  const root = await page();
  const body = root.querySelector<HTMLElement>('.plan-preparation-body')!;
  const footer = root.querySelector('.plan-preparation-actions')!;
  assert.equal(body.tabIndex, 0);
  assert.equal(body.getAttribute('role'), 'region');
  assert.ok(footer.contains(button(root, 'Implement plan')));
  assert.equal(body.contains(footer), false);
  assert.equal(root.querySelectorAll('input, textarea, select').length, 0);
  respond = () => implementationRun('running');
  button(root, 'Implement plan').click();
  await flush();
  const resultFooter = root.querySelector('.polyglot-review-actions')!;
  assert.ok(resultFooter.contains(button(root, 'Cancel implementation')));
  assert.equal(button(root, 'Close verified implementation').disabled, true);
});

test('plan implementation consumes the exact preparation review and saved provider prices', async () => {
  const root = await page();
  assert.match(root.textContent!, /Build host/);
  assert.match(root.textContent!, /Preserve quota on failure/);
  assert.match(root.textContent!, /remaining >= 0/);
  assert.deepEqual(
    calls.map((entry) => entry.args.request.action),
    ['beginPlanPreparation']
  );
  button(root, 'Implement plan').click();
  await flush();
  const request = calls.find(
    (entry) => entry.args.request.action === 'startPreparedImplementation'
  )!;
  assert.equal(request.command, 'polyglot_harness');
  assert.equal(request.args.dialogId, 'cloud');
  assert.equal(request.args.request.expectedReviewDigest, 'sha256:review');
  assert.equal(request.args.request.preparationId, 'preparation-1');
  assert.deepEqual(request.args.request.provider.rates, planProfile.harnessRates);
  assert.match(root.textContent!, /Implementation blocked/);
  assert.doesNotMatch(root.textContent!, /ephemeral-secret/);
});

test('unsaved editor inputs block automatic preparation before any model or controller request', async () => {
  const root = await page(['src/unsaved.ts']);
  assert.equal(button(root, 'Prepare plan').disabled, true);
  assert.match(root.textContent!, /Save or undo/);
  assert.equal(calls.length, 0);
});

test('implementation approval requires inspecting actual subject and resumes with provider reentry', async () => {
  const root = await page();
  const approval: HarnessApproval = {
    approvalType: 'domain-contract',
    summary: 'Preserve quota',
    subjectKind: 'contract',
    subject: { invariant: 'remaining >= 0' },
    assets: [
      {
        path: 'tests/quota.mjs',
        digest: 'sha256:test',
        content: 'assert.equal(remaining, before)',
      },
    ],
    subjectDigest: 'sha256:contract',
    scopeDigest: 'sha256:scope',
    policyDigest: 'sha256:policy',
    request: {
      run_id: 'implementation-1',
      subject_digest: 'sha256:contract',
      scope_digest: 'sha256:scope',
      policy_digest: 'sha256:policy',
      approval_type: 'domain-contract',
      expires_ms: null,
    },
  };
  respond = () => ({ ...implementationRun(), pendingApprovals: [approval] });
  button(root, 'Implement plan').click();
  await flush();
  assert.equal(
    calls.some((entry) => entry.args.request.action === 'approve'),
    false
  );
  button(root, 'Review domain-contract').click();
  assert.match(root.textContent!, /assert.equal\(remaining, before\)/);
  button(root, 'Approve domain-contract').click();
  await flush();
  assert.deepEqual(
    calls.find((entry) => entry.args.request.action === 'approve')!.args.request.request,
    approval.request
  );
  button(root, 'Resume verified implementation').click();
  await flush();
  const resume = calls.find((entry) => entry.args.request.action === 'resumeImplementation')!;
  assert.equal(resume.args.request.provider.profile.id, planProfile.id);
  assert.equal(
    calls.some((entry) => entry.args.request.action === 'resume'),
    false
  );
});

test('a rejected freshness read stops claiming the current implementation is verified', async () => {
  const root = await page();
  respond = () => implementationRun('verified');
  button(root, 'Implement plan').click();
  await flush();
  assert.match(root.textContent!, /Accepted — verified candidate/);
  respond = () => {
    throw new Error('Remote source changed');
  };
  button(root, 'Refresh implementation result').click();
  await flush();
  assert.doesNotMatch(root.textContent!, /Accepted — verified candidate/);
  assert.match(root.textContent!, /Implementation stale/);
});

test('a late implementation poll cannot overwrite a newer cancellation', async () => {
  const originalTimeout = window.setTimeout;
  let poll: (() => Promise<void>) | undefined;
  window.setTimeout = ((callback: TimerHandler, timeout?: number, ...args: unknown[]) => {
    if (timeout === 1500) {
      poll = callback as () => Promise<void>;
      return 98765;
    }
    return originalTimeout.call(window, callback, timeout, ...args);
  }) as typeof window.setTimeout;
  cleanups.push(() => {
    window.setTimeout = originalTimeout;
  });
  const root = await page();
  respond = () => implementationRun('running');
  button(root, 'Implement plan').click();
  await flush();
  assert.ok(poll);
  let finishStatus: ((value: unknown) => void) | undefined;
  respond = (request) =>
    request.action === 'status'
      ? new Promise((resolve) => {
          finishStatus = resolve;
        })
      : implementationRun('cancelled');
  const pending = poll();
  button(root, 'Cancel implementation').click();
  await flush();
  finishStatus!(implementationRun('running'));
  await pending;
  await flush();
  assert.match(root.textContent!, /Implementation cancelled/);
  assert.doesNotMatch(root.textContent!, /Implementation running/);
});

test('changing project context preserves historical evidence but blocks actions and current verified claims', async () => {
  reset();
  const [problem, setProblem] = createSignal<string | null>(null);
  const root = container();
  cleanups.push(
    render(
      () => (
        <PolyglotPlanImplementation
          target={{ root: '/project', dialogId: 'cloud' }}
          contextProblem={problem()}
          planPath={savedHarnessPlan.path}
          profile={planProfile}
          unsavedFiles={[]}
          executionLabel="Build host"
          onClose={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  respond = () => implementationRun('verified');
  button(root, 'Implement plan').click();
  await flush();
  assert.match(root.textContent!, /Accepted — verified candidate/);
  setProblem('The selected project changed. Return to the original project.');
  await flush();
  assert.match(root.textContent!, /Implementation stale/);
  assert.match(root.textContent!, /implementation-1/);
  assert.doesNotMatch(root.textContent!, /Accepted — verified candidate/);
  assert.equal(button(root, 'Refresh implementation result').disabled, true);
  assert.equal(button(root, 'Review candidate changes').disabled, true);
  assert.equal(button(root, 'Close verified implementation').disabled, false);
});

test('a start response arriving after the chat changes owners cannot claim an accepted result', async () => {
  reset();
  const [problem, setProblem] = createSignal<string | null>(null);
  const root = container();
  cleanups.push(
    render(
      () => (
        <PolyglotPlanImplementation
          target={{ root: '/project', dialogId: 'cloud' }}
          contextProblem={problem()}
          planPath={savedHarnessPlan.path}
          profile={planProfile}
          unsavedFiles={[]}
          executionLabel="Build host"
          onClose={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  let finish: ((value: unknown) => void) | undefined;
  respond = () =>
    new Promise((resolve) => {
      finish = resolve;
    });
  button(root, 'Implement plan').click();
  setProblem('The chat execution host changed. Reopen verified implementation.');
  await flush();
  finish!(implementationRun('verified'));
  await flush();
  assert.doesNotMatch(root.textContent!, /Accepted — verified candidate/);
  assert.match(root.textContent!, /execution host changed/);
});

test('reopening a durable strict run uses its current bound credentials and never starts a new plan', async () => {
  reset();
  const binding = {
    profileId: planProfile.id,
    model: planProfile.model,
    endpoint: `${planProfile.baseUrl}/chat/completions`,
    rates: { input: 3, cachedInput: 1, output: 7 },
  };
  const [credential, setCredential] = createSignal('rotated-secret');
  respond = () => ({ ...implementationRun(), providerBinding: binding });
  const root = container();
  cleanups.push(
    render(
      () => (
        <PolyglotPlanImplementation
          target={{ root: '/project', dialogId: 'owner' }}
          runId="durable-run"
          profile={{ ...planProfile, model: 'unrelated-active-model' }}
          resolveProvider={() => ({ ...planProfile, apiKey: credential() })}
          unsavedFiles={[]}
          executionLabel="Owner host"
          onClose={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  assert.deepEqual(
    calls.map((entry) => entry.args.request.action),
    ['status']
  );
  assert.equal(calls[0].args.request.runId, 'durable-run');
  assert.equal(root.querySelector('[aria-label="Input rate"]'), null);
  assert.match(root.textContent!, /Bound provider identity/);
  setCredential('newest-secret');
  button(root, 'Resume verified implementation').click();
  await flush();
  const resume = calls.find((entry) => entry.args.request.action === 'resumeImplementation')!.args
    .request;
  assert.equal(resume.provider.profile.apiKey, 'newest-secret');
  assert.equal(resume.provider.profile.model, planProfile.model);
  assert.deepEqual(resume.provider.rates, binding.rates);
  assert.equal(
    calls.some((entry) =>
      ['implementPlan', 'resume', 'chat_send'].includes(entry.args.request.action)
    ),
    false
  );
  assert.doesNotMatch(root.textContent!, /newest-secret|rotated-secret/);
});

test('a missing bound provider blocks strict resume while leaving decisions and historical results readable', async () => {
  reset();
  respond = () => ({
    ...implementationRun(),
    providerBinding: {
      profileId: planProfile.id,
      model: planProfile.model,
      endpoint: `${planProfile.baseUrl}/chat/completions`,
      rates: { input: 0, cachedInput: 0, output: 0 },
    },
  });
  const root = container();
  cleanups.push(
    render(
      () => (
        <PolyglotPlanImplementation
          target={{ root: '/project' }}
          runId="durable-run"
          resolveProvider={() => undefined}
          unsavedFiles={[]}
          executionLabel="This computer"
          onClose={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  assert.match(root.textContent!, /bound provider is unavailable/);
  assert.equal(button(root, 'Resume verified implementation').disabled, true);
  assert.equal(button(root, 'Refresh implementation result').disabled, false);
});

test('applying a verified candidate requires the exact delivery review and labels host-only receipts honestly', async () => {
  reset();
  let current = implementationRun('verified');
  const approval: HarnessApproval = {
    approvalType: 'delivery',
    summary: 'Apply candidate to host checkout',
    subjectKind: 'delivery',
    subjectDigest: 'sha256:delivery',
    scopeDigest: 'sha256:scope',
    policyDigest: 'sha256:policy',
    subject: {
      candidateDigest: 'sha256:candidate',
      root: '/host/project',
      location: 'host',
      paths: ['src/fix.ts'],
    },
    assets: [],
    request: {
      run_id: 'implementation-1',
      subject_digest: 'sha256:delivery',
      scope_digest: 'sha256:scope',
      policy_digest: 'sha256:policy',
      approval_type: 'delivery',
      expires_ms: null,
    },
  };
  respond = (request) => {
    if (request.action === 'reviewChanges')
      return {
        runId: 'implementation-1',
        candidateDigest: 'sha256:candidate',
        patch: '+ corrected behavior',
      };
    if (request.action === 'prepareApplication')
      current = {
        ...current,
        application: { subjectDigest: 'sha256:delivery', subject: approval.subject },
        pendingApprovals: [approval],
      };
    if (request.action === 'approve') current = { ...current, pendingApprovals: [] };
    if (request.action === 'applyApplication')
      current = {
        ...current,
        status: 'applied',
        applicationReceipt: {
          subjectDigest: 'sha256:delivery',
          candidateDigest: 'sha256:candidate',
          root: '/host/project',
          head: 'head',
          location: 'host',
          applied: true,
          deliveredLocally: false,
          paths: ['src/fix.ts'],
          appliedAtMs: 1,
          current: true,
        },
      };
    if (request.action === 'releaseApplication')
      current = {
        ...current,
        applicationReceipt: { ...current.applicationReceipt!, synchronizationReleased: true },
      };
    return current;
  };
  const root = container();
  cleanups.push(
    render(
      () => (
        <PolyglotPlanImplementation
          target={{ root: '/project', dialogId: 'host' }}
          runId="implementation-1"
          unsavedFiles={[]}
          executionLabel="Build host"
          onClose={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  assert.equal(button(root, 'Prepare application review').disabled, true);
  button(root, 'Review verified changes').click();
  await flush();
  button(root, 'Prepare application review').click();
  await flush();
  assert.equal(button(root, 'Apply reviewed verified changes').disabled, true);
  assert.equal(
    calls.some((entry) => entry.args.request.action === 'approve'),
    false
  );
  button(root, 'Review delivery').click();
  assert.match(root.textContent!, /\/host\/project/);
  button(root, 'Approve delivery').click();
  await flush();
  assert.equal(button(root, 'Apply reviewed verified changes').disabled, false);
  button(root, 'Apply reviewed verified changes').click();
  await flush();
  const applied = calls.find((entry) => entry.args.request.action === 'applyApplication')!.args
    .request;
  assert.equal(applied.subjectDigest, 'sha256:delivery');
  assert.equal(applied.savedBuffers, true);
  assert.match(root.textContent!, /Applied on execution host/);
  assert.match(root.textContent!, /Desktop delivery remains a separate step/);
  assert.doesNotMatch(root.textContent!, /Applied to local checkout/);
  assert.equal(button(root, 'Allow synchronization').disabled, true);
  const acknowledgement = [...root.querySelectorAll('label')]
    .find((entry) => entry.textContent?.includes('preserved or manually integrated'))!
    .querySelector<HTMLInputElement>('input')!;
  acknowledgement.checked = true;
  acknowledgement.dispatchEvent(new Event('change', { bubbles: true }));
  button(root, 'Allow synchronization').click();
  await flush();
  assert.deepEqual(
    calls.find((entry) => entry.args.request.action === 'releaseApplication')!.args.request,
    {
      action: 'releaseApplication',
      runId: 'implementation-1',
      subjectDigest: 'sha256:delivery',
      acknowledged: true,
    }
  );
});
