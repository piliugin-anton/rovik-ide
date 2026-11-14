import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import { appSettingsPage, closeAppSettings } from '../lib/appSettings';
import PlanPreparationCard from './PlanPreparationCard';
import {
  implementationRun,
  planPreparation,
  planProfile,
  savedHarnessPlan,
} from '../test/polyglotPlanFixtures';
import type { PlanPreparation } from '../lib/planPreparation';
import type { ModelProfile } from '../types';

let calls: any[] = [];
let respond: (request: any) => unknown;
const cleanup: (() => void)[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (command: string, args: any) => {
    assert.equal(command, 'polyglot_harness');
    calls.push(args);
    return respond(args.request);
  },
};
const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};
afterEach(() => {
  for (const dispose of cleanup.splice(0)) dispose();
});
function button(root: HTMLElement, text: string): HTMLButtonElement {
  const found = [...root.querySelectorAll('button')].find(
    (value) => value.textContent?.trim() === text
  );
  assert.ok(found, text);
  return found;
}
function mount(
  options: {
    profile?: () => ModelProfile;
    unsaved?: () => string[];
    context?: () => string | null;
    path?: () => string;
    resumed?: string;
    running?: boolean;
    prepare?: (id: string) => Promise<boolean>;
  } = {}
) {
  calls = [];
  const root = container();
  const prepared: string[] = [];
  const started: unknown[] = [];
  const observed: PlanPreparation[] = [];
  const stops: string[] = [];
  cleanup.push(
    render(
      () => (
        <PlanPreparationCard
          target={{ root: '/project', dialogId: 'chat' }}
          planPath={options.path?.() ?? savedHarnessPlan.path}
          preparationId={options.resumed}
          profile={options.profile?.() ?? planProfile}
          unsavedFiles={options.unsaved?.() ?? []}
          contextProblem={options.context?.()}
          executionLabel="This computer"
          agentRunning={options.running}
          onPrepare={async (id) => {
            prepared.push(id);
            return options.prepare ? options.prepare(id) : true;
          }}
          onStop={async () => {
            stops.push('stop');
          }}
          onPreparation={(value) => observed.push(value)}
          onStart={(value) => started.push(value)}
          onClose={() => {}}
        />
      ),
      root
    )
  );
  return { root, prepared, started, observed, stops };
}

test('opening a plan automatically starts preparation in chat without forms or implementation', async () => {
  respond = () => planPreparation('preparing');
  const page = mount();
  await flush();
  assert.deepEqual(page.prepared, ['preparation-1']);
  assert.deepEqual(
    calls.map((call) => call.request.action),
    ['beginPlanPreparation']
  );
  assert.equal(calls[0].request.planPath, savedHarnessPlan.path);
  assert.equal(calls[0].request.dialogId, 'chat');
  assert.deepEqual(calls[0].request.provider.rates, planProfile.harnessRates);
  assert.equal(page.root.querySelectorAll('input, textarea, select').length, 0);
  assert.match(page.root.textContent!, /private workspace/);
  assert.equal(page.started.length, 0);
  assert.doesNotMatch(page.root.textContent!, /ephemeral-secret|Accepted|Verified candidate/);
});

test('a rejected preparation can open saved prices and retry with corrected decimal rates', async () => {
  const corrected = { input: 225_000, cachedInput: 4_500, output: 900_000 };
  const [profile, setProfile] = createSignal<ModelProfile>({
    ...planProfile,
    harnessRates: { ...corrected, input: 225_000_000 },
  });
  respond = () => {
    throw new Error(
      'The implementation spending limit is 10, but reserving one complete model request requires 18.770798 in the provider’s billing currency.'
    );
  };
  const page = mount({ profile });
  await flush();
  assert.equal(page.prepared.length, 0);
  assert.match(page.root.querySelector('[role="alert"]')!.textContent!, /18\.770798/);
  closeAppSettings();
  try {
    button(page.root, 'Review model prices').click();
    assert.equal(appSettingsPage(), 'providers');
    setProfile({ ...planProfile, harnessRates: corrected });
    respond = () => {
      const preparation = planPreparation('preparing');
      return {
        ...preparation,
        providerBinding: { ...preparation.providerBinding!, rates: corrected },
      };
    };
    button(page.root, 'Prepare plan').click();
    await flush();
    assert.deepEqual(calls.at(-1).request.provider.rates, corrected);
    assert.deepEqual(page.prepared, ['preparation-1']);
    assert.equal(page.root.querySelectorAll('[role="alert"]').length, 0);
    assert.equal(page.started.length, 0, 'correcting prices does not approve implementation');
  } finally {
    closeAppSettings();
  }
});

test('one compact review starts the controller with the exact prepared identity', async () => {
  respond = (request) =>
    request.action === 'startPreparedImplementation'
      ? implementationRun('running')
      : planPreparation();
  const page = mount();
  await flush();
  assert.equal(
    page.prepared.length,
    0,
    'a ready reusable preparation does not start a second agent'
  );
  assert.match(page.root.textContent!, /Preserve quota on failure/);
  assert.match(page.root.textContent!, /remaining >= 0/);
  assert.match(page.root.textContent!, /16 model calls/);
  assert.match(page.root.textContent!, /\$10\.00/);
  assert.match(page.root.textContent!, /billed separately/);
  assert.equal(page.root.querySelector('details')?.open, false);
  assert.ok(
    page.root
      .querySelector('.plan-preparation-actions')
      ?.contains(button(page.root, 'Implement plan'))
  );
  assert.equal(
    calls.some((call) => call.request.action === 'startPreparedImplementation'),
    false
  );
  button(page.root, 'Implement plan').click();
  await flush();
  const request = calls.find(
    (call) => call.request.action === 'startPreparedImplementation'
  ).request;
  assert.equal(request.preparationId, 'preparation-1');
  assert.equal(request.expectedReviewDigest, 'sha256:review');
  assert.equal(request.savedBuffers, true);
  assert.deepEqual(request.provider.rates, planProfile.harnessRates);
  assert.equal(page.started.length, 1);
  assert.equal(
    calls.some((call) =>
      ['approve', 'implementPlan', 'applyApplication'].includes(call.request.action)
    ),
    false
  );
});

test('missing saved prices prevent model calls and saving prices starts preparation once', async () => {
  respond = () => planPreparation('preparing');
  const [profile, setProfile] = createSignal<ModelProfile>({
    ...planProfile,
    harnessRates: undefined,
  });
  const page = mount({ profile });
  await flush();
  assert.equal(calls.length, 0);
  assert.equal(button(page.root, 'Prepare plan').disabled, true);
  assert.ok(button(page.root, 'Set model prices'));
  setProfile({ ...planProfile });
  await flush();
  assert.deepEqual(page.prepared, ['preparation-1']);
  setProfile({ ...planProfile });
  await flush();
  assert.equal(calls.length, 1);
});

test('unsaved editor contents block preparation and invalidate an already displayed review', async () => {
  respond = () => planPreparation();
  const [unsaved, setUnsaved] = createSignal(['src/quota.ts']);
  const page = mount({ unsaved });
  await flush();
  assert.equal(calls.length, 0);
  setUnsaved([]);
  await flush();
  assert.equal(button(page.root, 'Implement plan').disabled, false);
  setUnsaved(['src/quota.ts']);
  await flush();
  assert.equal(button(page.root, 'Implement plan').disabled, true);
  assert.equal(page.started.length, 0);
});

test('changed saved prices block the old review and offer automatic preparation again', async () => {
  respond = () => planPreparation();
  const [profile, setProfile] = createSignal<ModelProfile>(planProfile);
  const page = mount({ profile });
  await flush();
  setProfile({ ...planProfile, harnessRates: { input: 1, cachedInput: 0, output: 2 } });
  await flush();
  assert.equal(button(page.root, 'Implement plan').disabled, true);
  assert.match(page.root.textContent!, /prices changed/);
  respond = () => ({ ...planPreparation('preparing'), providerBinding: undefined });
  button(page.root, 'Prepare again').click();
  await flush();
  assert.deepEqual(calls.at(-1).request.provider.rates, profile().harnessRates);
  assert.deepEqual(page.prepared, ['preparation-1']);
  assert.equal(page.started.length, 0);
});

test('a preparation arriving after the project changes cannot start the agent', async () => {
  let finish: (value: PlanPreparation) => void = () => assert.fail('begin was not called');
  respond = () =>
    new Promise((resolve) => {
      finish = resolve;
    });
  const [context, setContext] = createSignal<string | null>(null);
  const page = mount({ context });
  await flush();
  setContext('Return to the original project.');
  await flush();
  finish(planPreparation('preparing'));
  await flush();
  assert.equal(page.prepared.length, 0);
  assert.equal(page.observed.length, 0);
  assert.match(page.root.textContent!, /original project/);
});

test('switching plans ignores the previous capture and starts only the new preparation', async () => {
  const pending: ((value: PlanPreparation) => void)[] = [];
  respond = () => new Promise((resolve) => pending.push(resolve));
  const [path, setPath] = createSignal('plans/first.md');
  const page = mount({ path });
  await flush();
  setPath('plans/second.md');
  await flush();
  assert.equal(pending.length, 2);
  pending[0]({ ...planPreparation('preparing'), preparationId: 'first' });
  await flush();
  assert.equal(page.prepared.length, 0);
  pending[1]({ ...planPreparation('preparing'), preparationId: 'second' });
  await flush();
  assert.deepEqual(page.prepared, ['second']);
});

test('preparation can be stopped while the agent is still installing its environment', async () => {
  let finish!: (value: boolean) => void;
  respond = (request) =>
    planPreparation(request.action === 'cancelPreparation' ? 'cancelled' : 'preparing');
  const page = mount({ prepare: () => new Promise((resolve) => (finish = resolve)) });
  await flush();
  assert.equal(button(page.root, 'Stop preparation').disabled, false);
  button(page.root, 'Stop preparation').click();
  await flush();
  assert.deepEqual(page.stops, ['stop']);
  assert.match(page.root.textContent!, /Preparation stopped/);
  finish(false);
  await flush();
  assert.match(page.root.textContent!, /Preparation stopped/);
  assert.equal(page.root.querySelector('[role="alert"]'), null);
});

test('a stale status blocks implementation and lets the user prepare again', async () => {
  respond = () => planPreparation();
  const page = mount();
  await flush();
  respond = () => ({ ...planPreparation('stale'), reason: 'The saved plan changed.' });
  button(page.root, 'Refresh').click();
  await flush();
  assert.match(page.root.textContent!, /saved plan changed/);
  assert.ok(button(page.root, 'Prepare again'));
  assert.equal(
    [...page.root.querySelectorAll('button')].some(
      (entry) => entry.textContent === 'Implement plan'
    ),
    false
  );
});

test('Stop cancels the ordinary agent and durable preparation without a late poll resurrecting it', async () => {
  respond = () => planPreparation('preparing');
  const page = mount({ running: true });
  await flush();
  let finish: (value: PlanPreparation) => void = () => assert.fail('poll not called');
  respond = (request) =>
    request.action === 'preparationStatus'
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : planPreparation('cancelled');
  button(page.root, 'Refresh').click();
  await flush();
  button(page.root, 'Stop preparation').click();
  await flush();
  assert.deepEqual(page.stops, ['stop']);
  finish(planPreparation());
  await flush();
  assert.match(page.root.textContent!, /Preparation stopped/);
  assert.doesNotMatch(page.root.textContent!, /Ready to implement/);
  assert.equal(page.started.length, 0);
});

test('reopening a promoted preparation loads the existing run without preparing or starting again', async () => {
  respond = (request) =>
    request.action === 'preparationStatus'
      ? { ...planPreparation(), runId: 'implementation-1' }
      : implementationRun('verified');
  const page = mount({ resumed: 'preparation-1' });
  await flush();
  assert.deepEqual(
    calls.map((call) => call.request.action),
    ['preparationStatus', 'status']
  );
  assert.equal(page.prepared.length, 0);
  assert.equal(page.started.length, 1);
});
