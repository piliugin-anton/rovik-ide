import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import PolyglotPlanImplementation from './PolyglotPlanImplementation';
import {
  implementationRun,
  planInspection,
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
function field(root: HTMLElement, label: string, value: string) {
  const node = root.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;
  assert.ok(node, label);
  node.value = value;
  node.dispatchEvent(new Event('input', { bubbles: true }));
}
function reviewed(root: HTMLElement) {
  const label = [...root.querySelectorAll('label')].find((entry) =>
    entry.textContent?.includes('these exact saved plan bytes')
  )!;
  const node = label.querySelector<HTMLInputElement>('input')!;
  node.checked = true;
  node.dispatchEvent(new Event('change', { bubbles: true }));
}
function reset() {
  calls = [];
  respond = (request) =>
    request.action === 'inspectPlan'
      ? savedHarnessPlan
      : request.action === 'inspect'
        ? planInspection()
        : implementationRun();
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
function ready(root: HTMLElement) {
  for (const name of ['Input rate', 'Cached input rate', 'Output rate']) field(root, name, '0');
  reviewed(root);
}

test('plan review starts only the trusted controller with saved bytes and explicit rates', async () => {
  const root = await page();
  assert.match(root.textContent!, /Build host/);
  assert.match(root.textContent!, /Preserve quota on failure/);
  assert.match(root.textContent!, /remaining >= 0/);
  assert.equal(button(root, 'Start verified implementation').disabled, true);
  assert.deepEqual(calls.map((entry) => entry.args.request.action).sort(), [
    'inspect',
    'inspectPlan',
  ]);
  ready(root);
  button(root, 'Start verified implementation').click();
  await flush();
  const request = calls.find((entry) => entry.args.request.action === 'implementPlan')!;
  assert.equal(request.command, 'polyglot_harness');
  assert.equal(request.args.dialogId, 'cloud');
  assert.equal(request.args.request.expectedPlanDigest, savedHarnessPlan.digest);
  assert.equal(request.args.request.expectedSnapshotDigest, 'sha256:source');
  assert.equal(request.args.request.planCriteriaReviewed, true);
  assert.match(root.textContent!, /Implementation blocked/);
  assert.equal(
    calls.some((entry) => entry.command === 'chat_send'),
    false
  );
  assert.doesNotMatch(root.textContent!, /ephemeral-secret/);
});

test('unsaved editor inputs block verified implementation even after plan review', async () => {
  const root = await page(['src/unsaved.ts']);
  ready(root);
  assert.equal(button(root, 'Start verified implementation').disabled, true);
  assert.match(root.textContent!, /Save or undo/);
  assert.equal(
    calls.some((entry) => entry.args.request.action === 'implementPlan'),
    false
  );
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
  ready(root);
  button(root, 'Start verified implementation').click();
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
  ready(root);
  button(root, 'Start verified implementation').click();
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

test('preparation role drafts are labelled advisory and never grant approvals', async () => {
  const root = await page();
  ready(root);
  button(root, 'Start verified implementation').click();
  await flush();
  respond = () => ({
    ...implementationRun(),
    drafts: [
      {
        role: 'domain_analyst',
        draft: { intent: 'Preserve quota', unresolved_questions: ['Who owns retries?'] },
        advisory: true,
        requiresReview: true,
      },
    ],
  });
  button(root, 'Draft domain preparation').click();
  await flush();
  assert.match(root.textContent!, /domain_analyst — advisory draft; review required/);
  assert.match(root.textContent!, /Who owns retries/);
  assert.equal(
    calls.some((entry) => entry.args.request.action === 'approve'),
    false
  );
  assert.doesNotMatch(root.textContent!, /Accepted — verified candidate/);
});

test('late plan reads from a previous execution context never replace the selected saved plan', async () => {
  reset();
  let finishOld: ((value: unknown) => void) | undefined;
  respond = (request) =>
    request.action === 'inspectPlan'
      ? request.planPath === 'old.md'
        ? new Promise((resolve) => {
            finishOld = resolve;
          })
        : { ...savedHarnessPlan, content: '# New host plan', path: 'new.md' }
      : planInspection();
  const [path, setPath] = createSignal('old.md');
  const root = container();
  cleanups.push(
    render(
      () => (
        <PolyglotPlanImplementation
          target={{ root: '/project', dialogId: path() }}
          planPath={path()}
          profile={planProfile}
          unsavedFiles={[]}
          executionLabel="Selected host"
          onClose={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  setPath('new.md');
  await flush();
  finishOld!({ ...savedHarnessPlan, content: '# Old host plan' });
  await flush();
  assert.match(root.textContent!, /New host plan/);
  assert.doesNotMatch(root.textContent!, /Old host plan/);
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
  ready(root);
  button(root, 'Start verified implementation').click();
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

test('changing selected input files invalidates criteria review before inspection finishes', async () => {
  const root = await page();
  ready(root);
  assert.equal(button(root, 'Start verified implementation').disabled, false);
  let finish: ((value: unknown) => void) | undefined;
  respond = (request) =>
    request.action === 'inspect'
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : savedHarnessPlan;
  const file = [...root.querySelectorAll('label')]
    .find((entry) => entry.textContent?.includes('src/new.ts'))!
    .querySelector<HTMLInputElement>('input')!;
  file.checked = true;
  file.dispatchEvent(new Event('change', { bubbles: true }));
  assert.equal(button(root, 'Start verified implementation').disabled, true);
  finish!({ ...planInspection(), snapshotDigest: 'sha256:with-new-file' });
  await flush();
  assert.equal(button(root, 'Start verified implementation').disabled, true);
  reviewed(root);
  assert.equal(button(root, 'Start verified implementation').disabled, false);
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
  ready(root);
  button(root, 'Start verified implementation').click();
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
  ready(root);
  button(root, 'Start verified implementation').click();
  setProblem('The chat execution host changed. Reopen verified implementation.');
  await flush();
  finish!(implementationRun('verified'));
  await flush();
  assert.doesNotMatch(root.textContent!, /Accepted — verified candidate/);
  assert.match(root.textContent!, /execution host changed/);
});
