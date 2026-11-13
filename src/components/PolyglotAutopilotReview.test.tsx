import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import PolyglotAutopilotReview from './PolyglotAutopilotReview';
import { planInspection, planProfile } from '../test/polyglotPlanFixtures';
import type { VerifiedDeliverySettings } from '../lib/autopilotSettings';
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
});
const flush = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};
function field(root: HTMLElement, label: string, value: string) {
  const node = root.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;
  node.value = value;
  node.dispatchEvent(new Event('input', { bubbles: true }));
}
function reviewed(root: HTMLElement) {
  const label = [...root.querySelectorAll('label')].find((entry) =>
    entry.textContent?.includes("I reviewed this task's")
  )!;
  const checkbox = label.querySelector<HTMLInputElement>('input')!;
  checkbox.checked = true;
  checkbox.dispatchEvent(new Event('change', { bubbles: true }));
}
function start(root: HTMLElement) {
  return [...root.querySelectorAll('button')].find(
    (entry) => entry.textContent?.trim() === 'Start reviewed verified Autopilot task'
  )!;
}

test('each task needs fresh explicit setup and price review before freezing non-secret inputs', async () => {
  calls = [];
  respond = () => planInspection();
  const root = container();
  const starts: VerifiedDeliverySettings[] = [];
  cleanups.push(
    render(
      () => (
        <PolyglotAutopilotReview
          target={{ root: '/project', dialogId: 'host-chat' }}
          profile={planProfile}
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
  for (const label of ['Input rate', 'Cached input rate', 'Output rate']) field(root, label, '0');
  assert.equal(start(root).disabled, true, 'rates alone are not approval');
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
  field(root, 'Input rate', '10');
  assert.equal(start(root).disabled, true, 'changing a price invalidates the reviewed binding');
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
          profile={planProfile}
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
  for (const label of ['Input rate', 'Cached input rate', 'Output rate']) field(root, label, '1');
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
          profile={{ ...planProfile, noTools: true }}
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
  for (const label of ['Input rate', 'Cached input rate', 'Output rate']) field(root, label, '0');
  reviewed(root);
  assert.match(root.textContent!, /tools enabled for Autopilot planning/);
  assert.equal(start(root).disabled, true);
  start(root).click();
  assert.deepEqual(starts, []);
});
