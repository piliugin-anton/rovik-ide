import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import PolyglotHarnessPanel from './PolyglotHarnessPanel';
import type { HarnessApproval, HarnessInspection, HarnessRun } from '../lib/polyglotHarness';

const engine = { kind: 'docker' as const, executable: '/usr/bin/docker' };
const approval: HarnessApproval = {
  approvalType: 'domain-contract',
  subjectDigest: 'sha256:subject',
  scopeDigest: 'sha256:scope',
  policyDigest: 'sha256:policy',
  summary: 'The reservation API must leave quota unchanged when an allocation fails.',
  subjectKind: 'contract',
  subject: {
    intent: 'Preserve remaining quota on failure',
    invariants: [{ statement: 'remaining >= 0' }],
  },
  assets: [
    { path: 'tests/quota.rs', digest: 'sha256:test', content: 'assert_eq!(remaining, before);' },
  ],
  request: {
    run_id: 'run-1',
    subject_digest: 'sha256:subject',
    scope_digest: 'sha256:scope',
    policy_digest: 'sha256:policy',
    approval_type: 'domain-contract',
    expires_ms: null,
  },
};
function makeRun(status: HarnessRun['status'] = 'blocked'): HarnessRun {
  return {
    runId: 'run-1',
    state: status === 'verified' ? 'ACCEPTED' : 'BLOCKED',
    status,
    snapshotDigest: 'sha256:input',
    pendingApprovals: status === 'blocked' ? [approval] : [],
    canCancel: status === 'blocked',
    canResume: status === 'blocked',
    canRecover: false,
  };
}
function makeInspection(): HarnessInspection {
  return {
    projectId: 'project',
    snapshotDigest: 'sha256:input',
    inventory: {
      units: [
        {
          root: 'src-tauri',
          manifest: 'src-tauri/Cargo.toml',
          languages: ['rust'],
          locks: ['src-tauri/Cargo.lock'],
        },
      ],
      unresolved: [],
    },
    untracked: ['src/new.ts'],
    issues: [],
    host: { supported: true, platform: 'linux', engines: [engine] },
    setups: [
      {
        id: 'setup-1',
        label: 'Reservation contract',
        contractSummary: 'A failed reservation preserves quota.',
        checks: [{ id: 'quota', command: 'quota-check', phase: 'final' }],
        configurations: ['rust-debug'],
        scope: { allow_write: ['src/**'], protected: ['tests/**'] },
      },
    ],
    configuration: {
      digest: 'sha256:config',
      setupId: 'setup-1',
      label: 'Reservation contract',
      engine,
    },
    runs: [],
  };
}
let calls: { command: string; args: any }[];
let answer: (args: any) => unknown;
let cleanup: (() => void)[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (command: string, args: any) => {
    calls.push({ command, args });
    if (command === 'plugin:dialog|open') return '/prepared/setup.json';
    if (command !== 'polyglot_harness') throw new Error(`unexpected command ${command}`);
    return answer(args);
  },
};
const flush = async () => {
  for (let n = 0; n < 8; n++) await new Promise((resolve) => setTimeout(resolve, 0));
};
afterEach(() => {
  cleanup.forEach((dispose) => dispose());
  cleanup = [];
});
function reset(info = makeInspection()) {
  calls = [];
  answer = ({ request }) =>
    request.action === 'inspect' ||
    request.action === 'configure' ||
    request.action === 'importSetup'
      ? info
      : makeRun();
}
function button(root: HTMLElement, text: string) {
  const result = [...root.querySelectorAll('button')].find(
    (item) => item.textContent?.trim() === text
  );
  assert.ok(result, `button ${text} exists`);
  return result;
}
async function page(props: { unsavedFiles?: string[]; dialogId?: string } = { unsavedFiles: [] }) {
  const root = container();
  cleanup.push(render(() => <PolyglotHarnessPanel root="/project" {...props} />, root));
  await flush();
  return root;
}

test('opening project verification discovers inputs without configuring or approving them', async () => {
  reset({ ...makeInspection(), configuration: null });
  const root = await page();
  assert.match(root.textContent!, /src-tauri\/Cargo.toml/);
  assert.match(root.textContent!, /A failed reservation preserves quota/);
  assert.match(root.textContent!, /Protected paths: tests\/\*\*/);
  assert.equal(button(root, 'Verify changes').disabled, true);
  assert.deepEqual(
    calls.map((call) => call.args.request.action),
    ['inspect']
  );
  button(root, 'Use this setup').click();
  await flush();
  assert.deepEqual(calls.at(-1)?.args.request, {
    action: 'configure',
    setupId: 'setup-1',
    engine,
    expectedSnapshotDigest: 'sha256:input',
  });
});

test('unsaved editor buffers block verification and make a previous accepted snapshot stale', async () => {
  const info = makeInspection();
  info.runs = [makeRun('verified')];
  reset(info);
  const [files, setFiles] = createSignal(['src/api.ts']);
  const root = container();
  cleanup.push(render(() => <PolyglotHarnessPanel root="/project" unsavedFiles={files()} />, root));
  await flush();
  assert.equal(button(root, 'Verify changes').disabled, true);
  assert.match(root.textContent!, /Verification is stale/);
  setFiles([]);
  await flush();
  assert.equal(button(root, 'Verify changes').disabled, false);
  assert.equal(
    root.querySelector('[aria-label="Verification result"] [role="status"]')?.textContent?.trim(),
    'Verified'
  );
});

test('including a new file re-inspects its bytes before starting exact-snapshot verification', async () => {
  reset();
  answer = ({ request }) =>
    request.action === 'inspect'
      ? {
          ...makeInspection(),
          snapshotDigest: request.untracked.length ? 'sha256:new-file' : 'sha256:input',
        }
      : makeRun();
  const root = await page();
  (root.querySelector('input[aria-label="Include src/new.ts"]') as HTMLInputElement).click();
  await flush();
  button(root, 'Verify changes').click();
  await flush();
  assert.deepEqual(calls.at(-1)?.args.request, {
    action: 'verify',
    untracked: ['src/new.ts'],
    expectedSnapshotDigest: 'sha256:new-file',
    savedBuffers: true,
  });
});

test('an approval requires reviewing its concrete subject and an explicit user decision', async () => {
  const info = makeInspection();
  info.runs = [makeRun()];
  reset(info);
  const root = await page();
  assert.equal(
    calls.some((call) => call.args.request?.action === 'approve'),
    false
  );
  button(root, 'Review domain-contract').click();
  await flush();
  assert.match(root.textContent!, /leave quota unchanged/);
  assert.match(root.textContent!, /sha256:subject/);
  assert.match(root.textContent!, /Preserve remaining quota on failure/);
  assert.match(root.textContent!, /assert_eq!\(remaining, before\)/);
  button(root, 'Approve domain-contract').click();
  await flush();
  assert.deepEqual(calls.at(-1)?.args.request, {
    action: 'approve',
    runId: 'run-1',
    request: approval.request,
  });
});

test('unsupported hosts remain blocked and importing a prepared setup never approves it', async () => {
  reset({
    ...makeInspection(),
    host: {
      supported: false,
      platform: 'darwin',
      engines: [],
      reason: 'Select a supported Linux execution host.',
    },
  });
  const root = await page();
  assert.match(root.textContent!, /Select a supported Linux execution host/);
  assert.equal(button(root, 'Verify changes').disabled, true);
  button(root, 'Choose prepared setup…').click();
  await flush();
  assert.deepEqual(
    calls.find((call) => call.args.request?.action === 'importSetup')?.args.request,
    {
      action: 'importSetup',
      path: '/prepared/setup.json',
    }
  );
  assert.equal(
    calls.some((call) => call.args.request?.action === 'approve'),
    false
  );
});

test('a remote setup uses an explicit host path rather than a local file picker path', async () => {
  reset();
  const root = await page({ unsavedFiles: [], dialogId: 'remote-chat' });
  assert.equal(root.textContent?.includes('Choose prepared setup…'), false);
  const path = root.querySelector(
    'input[aria-label="Prepared setup path on execution host"]'
  ) as HTMLInputElement;
  path.value = '/host/prepared/setup.json';
  path.dispatchEvent(new Event('input', { bubbles: true }));
  button(root, 'Import from execution host').click();
  await flush();
  assert.equal(calls.at(-1)?.args.executionHost, 'chat');
  assert.equal(calls.at(-1)?.args.dialogId, 'remote-chat');
  assert.deepEqual(
    calls.find((call) => call.args.request?.action === 'importSetup')?.args.request,
    {
      action: 'importSetup',
      path: '/host/prepared/setup.json',
    }
  );
  assert.equal(
    calls.some((call) => call.command === 'plugin:dialog|open'),
    false
  );
});

test('late project discovery cannot replace the setup for a newly selected project', async () => {
  reset();
  let release!: (value: HarnessInspection) => void;
  answer = ({ root }) =>
    root === '/old'
      ? new Promise<HarnessInspection>((resolve) => {
          release = resolve;
        })
      : {
          ...makeInspection(),
          setups: [{ ...makeInspection().setups[0], id: 'new', label: 'New project setup' }],
        };
  const [project, setProject] = createSignal('/old');
  const root = container();
  cleanup.push(render(() => <PolyglotHarnessPanel root={project()} unsavedFiles={[]} />, root));
  await flush();
  setProject('/new');
  await flush();
  release(makeInspection());
  await flush();
  assert.match(root.textContent!, /New project setup/);
  assert.doesNotMatch(
    root.querySelector('select[aria-label="Prepared verification setup"]')!.textContent!,
    /Reservation contract/
  );
});

/** Run observation timers explicitly so races do not depend on a product delay. */
function manualPolling() {
  const callbacks: (() => Promise<void>)[] = [];
  const original = window.setTimeout;
  let id = 0;
  window.setTimeout = ((callback: TimerHandler, delay?: number, ...args: any[]) => {
    if (delay === 1500 || delay === 5000) {
      callbacks.push(callback as () => Promise<void>);
      return --id;
    }
    return original.call(window, callback, delay, ...args);
  }) as typeof window.setTimeout;
  cleanup.push(() => {
    window.setTimeout = original;
  });
  return callbacks;
}

test('a status response already in flight cannot overwrite a later cancellation', async () => {
  const info = makeInspection();
  info.runs = [{ ...makeRun('running'), canCancel: true }];
  reset(info);
  const polls = manualPolling();
  let release!: (value: HarnessRun) => void;
  answer = ({ request }) => {
    if (request.action === 'inspect') return info;
    if (request.action === 'status')
      return new Promise<HarnessRun>((resolve) => {
        release = resolve;
      });
    if (request.action === 'cancel') return makeRun('cancelled');
    throw new Error(`unexpected ${request.action}`);
  };
  const root = await page();
  const pending = polls.shift()!();
  await flush();
  button(root, 'Cancel verification').click();
  await flush();
  release(makeRun('verified'));
  await pending;
  await flush();
  assert.equal(
    root.querySelector('[aria-label="Verification result"] [role="status"]')?.textContent?.trim(),
    'Verification cancelled'
  );
});

test('a transient status error is retried without restarting the run', async () => {
  const info = makeInspection();
  info.runs = [makeRun('running')];
  reset(info);
  const polls = manualPolling();
  let statusCalls = 0;
  answer = ({ request }) => {
    if (request.action === 'inspect') return info;
    assert.equal(request.action, 'status');
    if (++statusCalls === 1) throw new Error('connection interrupted');
    return makeRun('blocked');
  };
  const root = await page();
  await polls.shift()!();
  await flush();
  assert.match(root.textContent!, /connection interrupted/);
  assert.equal(polls.length, 1, 'the next status observation is scheduled');
  await polls.shift()!();
  await flush();
  assert.match(root.textContent!, /Verification blocked/);
  assert.doesNotMatch(root.textContent!, /connection interrupted/);
  assert.equal(
    calls.some((call) => ['verify', 'resume'].includes(call.args.request?.action)),
    false
  );
});

test('an older inspection cannot replace configuration after a later run action', async () => {
  const info = makeInspection();
  info.runs = [makeRun()];
  reset(info);
  const root = await page();
  let release!: (value: HarnessInspection) => void;
  answer = ({ request }) =>
    request.action === 'inspect'
      ? new Promise<HarnessInspection>((resolve) => {
          release = resolve;
        })
      : makeRun();
  button(root, 'Refresh project and results').click();
  await flush();
  button(root, 'Refresh result').click();
  await flush();
  release({ ...info, configuration: null });
  await flush();
  assert.equal(
    button(root, 'Verify changes').disabled,
    false,
    'the newer configured context remains usable'
  );
});

test('an unreadable approval subject cannot be approved from its summary alone', async () => {
  const info = makeInspection();
  info.runs = [{ ...makeRun(), pendingApprovals: [{ ...approval, subject: null }] }];
  reset(info);
  const root = await page();
  button(root, 'Review domain-contract').click();
  await flush();
  assert.match(root.textContent!, /approval subject could not be read/);
  assert.equal(button(root, 'Approve domain-contract').disabled, true);
});

test('a freshness read failure removes the current Verified label until binding is rechecked', async () => {
  const info = makeInspection();
  info.runs = [makeRun('verified')];
  reset(info);
  const polls = manualPolling();
  let online = false;
  answer = ({ request }) => {
    if (request.action === 'inspect') return info;
    if (!online) throw new Error('execution host is offline');
    return makeRun('verified');
  };
  const root = await page();
  await polls.shift()!();
  await flush();
  assert.equal(
    root.querySelector('[aria-label="Verification result"] [role="status"]')?.textContent?.trim(),
    'Verification is stale'
  );
  assert.match(root.textContent!, /Current inputs could not be confirmed/);
  online = true;
  await polls.shift()!();
  await flush();
  assert.equal(
    root.querySelector('[aria-label="Verification result"] [role="status"]')?.textContent?.trim(),
    'Verified'
  );
});

test('a failed prepared setup import keeps its error rather than hiding it with a refresh', async () => {
  reset();
  answer = ({ request }) => {
    if (request.action === 'importSetup') throw new Error('protected artifacts are missing');
    return makeInspection();
  };
  const root = await page();
  button(root, 'Choose prepared setup…').click();
  await flush();
  assert.match(root.textContent!, /protected artifacts are missing/);
  assert.equal(calls.filter((call) => call.args.request?.action === 'inspect').length, 1);
});

test('failed verification reports the observed check, cases and actionable source diagnostic', async () => {
  const info = makeInspection();
  info.runs = [
    {
      ...makeRun('failed'),
      checks: [
        {
          check_id: 'architecture',
          status: 'fail',
          executed_test_count: 1,
          failed_case_ids: ['domain-isolation'],
          diagnostics: [
            {
              rule_id: 'ARCH-002',
              message: 'Domain cannot import the database adapter',
              file: 'src/domain.ts',
              line: 4,
            },
          ],
        },
      ],
    },
  ];
  reset(info);
  const root = await page();
  assert.match(root.textContent!, /architecture: fail/);
  assert.match(root.textContent!, /Failed cases: domain-isolation/);
  assert.match(root.textContent!, /Domain cannot import the database adapter \(src\/domain.ts:4\)/);
});
