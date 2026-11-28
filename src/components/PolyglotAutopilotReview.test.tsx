import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import PolyglotAutopilotReview from './PolyglotAutopilotReview';
import { planInspection, planProfile } from '../test/polyglotPlanFixtures';
import type { VerifiedDeliverySettings } from '../lib/autopilotSettings';
import { appSettingsPage, closeAppSettings } from '../lib/appSettings';
const pricedProfile = { ...planProfile, harnessRates: { input: 0, cachedInput: 0, output: 0 } };
const cleanups: (() => void)[] = [];
let calls: any[] = [];
let respond: (request: any) => unknown = () => planInspection();
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (command: string, args: any) => {
    assert.equal(command, 'polyglot_harness');
    calls.push(args.request);
    return respond(args.request);
  },
};
afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
  closeAppSettings();
});
const flush = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};
function reviewed(root: HTMLElement) {
  const label = [...root.querySelectorAll('label')].find((entry) =>
    entry.textContent?.includes('I reviewed this task')
  )!;
  const checkbox = label.querySelector<HTMLInputElement>('input')!;
  checkbox.checked = true;
  checkbox.dispatchEvent(new Event('change', { bubbles: true }));
}
function start(root: HTMLElement) {
  return [...root.querySelectorAll('button')].find(
    (entry) => entry.textContent?.trim() === 'Start verified task'
  )!;
}

test('a task reviews its protected checks and freezes saved model prices without a price form', async () => {
  calls = [];
  respond = () => planInspection();
  const root = container();
  const starts: VerifiedDeliverySettings[] = [];
  const [profile, setProfile] = createSignal(pricedProfile);
  cleanups.push(
    render(
      () => (
        <PolyglotAutopilotReview
          target={{ root: '/project', dialogId: 'host-chat' }}
          profile={profile()}
          request="Fix the boundary"
          executionLabel="Build host"
          unsavedFiles={[]}
          onStart={(value) => starts.push(value)}
          onCancel={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  assert.equal(start(root).disabled, true);
  assert.ok(root.querySelector('.polyglot-review-actions')!.contains(start(root)));
  assert.equal(root.querySelector('.polyglot-review-body')!.contains(start(root)), false);
  assert.deepEqual(
    calls.map((call) => call.action),
    ['inspect']
  );
  assert.equal(
    root.querySelector('input[type="number"]'),
    null,
    'pricing stays in provider settings'
  );
  assert.equal(start(root).disabled, true, 'saved prices alone are not task approval');
  const setupEditor = [...root.querySelectorAll('details')].find((entry) =>
    entry.querySelector('summary')?.textContent?.includes('Project checks and files')
  );
  assert.equal(setupEditor?.open, false, 'a configured project shows its compact summary');
  reviewed(root);
  start(root).click();
  assert.deepEqual(starts, [
    {
      setupId: 'setup',
      configurationDigest: 'sha256:configuration',
      untracked: [],
      rates: { input: 0, cachedInput: 0, output: 0 },
    },
  ]);
  assert.doesNotMatch(JSON.stringify(starts), /apiKey|ephemeral-secret/);
  assert.equal(
    calls.some((call) => call.action === 'approve' || call.action === 'implementPlan'),
    false
  );
  setProfile({
    ...pricedProfile,
    harnessRates: { input: 2_500_000, cachedInput: 500_000, output: 8_000_001 },
  });
  assert.match(root.textContent!, /Input 2\.5, cached input 0\.5, output 8\.000001/);
  assert.match(root.textContent!, /billing currency per million tokens/);
  assert.doesNotMatch(root.textContent!, /microunits/);
  assert.equal(start(root).disabled, true, 'changing a price invalidates the reviewed binding');
  assert.equal(starts[0].rates.input, 0, 'the task retains a copy of its original saved prices');
});

test('unsaved inputs and a changed execution context block a previously reviewed task start', async () => {
  calls = [];
  respond = () => planInspection();
  const root = container();
  const starts: VerifiedDeliverySettings[] = [];
  const [unsaved, setUnsaved] = createSignal<string[]>([]);
  const [context, setContext] = createSignal<string | null>(null);
  cleanups.push(
    render(
      () => (
        <PolyglotAutopilotReview
          target={{ root: '/project' }}
          profile={pricedProfile}
          request="Fix it"
          executionLabel="This computer"
          unsavedFiles={unsaved()}
          contextProblem={context()}
          onStart={(value) => starts.push(value)}
          onCancel={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  reviewed(root);
  assert.equal(start(root).disabled, false);
  setUnsaved(['src/changed.ts']);
  assert.equal(start(root).disabled, true);
  setUnsaved([]);
  assert.equal(start(root).disabled, true, 'saving requires a fresh inspection and review');
  setContext('Selected execution host changed');
  assert.match(root.textContent!, /Selected execution host changed/);
  start(root).click();
  assert.deepEqual(starts, []);
});

test('strict Autopilot cannot start its planning stages with a model whose tools are disabled', async () => {
  calls = [];
  respond = () => planInspection();
  const root = container();
  const starts: VerifiedDeliverySettings[] = [];
  cleanups.push(
    render(
      () => (
        <PolyglotAutopilotReview
          target={{ root: '/project' }}
          profile={{ ...pricedProfile, noTools: true }}
          request="Fix it"
          executionLabel="This computer"
          unsavedFiles={[]}
          onStart={(value) => starts.push(value)}
          onCancel={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  reviewed(root);
  assert.match(root.textContent!, /tools enabled for Autopilot planning/);
  assert.equal(start(root).disabled, true);
  start(root).click();
  assert.deepEqual(starts, []);
});

test('missing saved prices block starting and open model provider settings without assuming free calls', async () => {
  calls = [];
  respond = () => planInspection();
  const root = container();
  const starts: VerifiedDeliverySettings[] = [];
  cleanups.push(
    render(
      () => (
        <PolyglotAutopilotReview
          target={{ root: '/project' }}
          profile={{ ...planProfile, harnessRates: undefined }}
          request="Fix it"
          executionLabel="This computer"
          unsavedFiles={[]}
          onStart={(value) => starts.push(value)}
          onCancel={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  reviewed(root);
  assert.equal(start(root).disabled, true);
  assert.match(root.textContent!, /Save verification prices/);
  const settings = [...root.querySelectorAll('button')].find(
    (entry) => entry.textContent?.trim() === 'Model providers'
  )!;
  settings.click();
  assert.equal(appSettingsPage(), 'providers');
  start(root).click();
  assert.deepEqual(starts, []);
});

test('a missing project setup stays expanded and blocks starting even with saved prices', async () => {
  calls = [];
  respond = () => ({ ...planInspection(), configuration: undefined, setups: [] });
  const root = container();
  const starts: VerifiedDeliverySettings[] = [];
  cleanups.push(
    render(
      () => (
        <PolyglotAutopilotReview
          target={{ root: '/project' }}
          profile={pricedProfile}
          request="Fix it"
          executionLabel="This computer"
          unsavedFiles={[]}
          onStart={(value) => starts.push(value)}
          onCancel={() => {}}
        />
      ),
      root
    )
  );
  await flush();
  const setupEditor = [...root.querySelectorAll('details')].find((entry) =>
    entry.querySelector('summary')?.textContent?.includes('Choose project checks')
  );
  assert.equal(setupEditor?.open, true);
  reviewed(root);
  assert.equal(start(root).disabled, true);
  start(root).click();
  assert.deepEqual(starts, []);
});
