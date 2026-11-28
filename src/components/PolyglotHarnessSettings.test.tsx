import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import PolyglotHarnessSettings from './PolyglotHarnessSettings';
import type { HarnessChatContext, HarnessInspection } from '../lib/polyglotHarness';

const chat: HarnessChatContext = {
  dialogId: 'remote-chat',
  projectId: 'project-1',
  hostId: 'linux',
  label: 'Linux builder — API repair',
};
const inspection = (projectId: string): HarnessInspection => ({
  projectId,
  snapshotDigest: `sha256:${projectId}`,
  inventory: {
    units: [{ root: '', manifest: `${projectId}/Cargo.toml`, languages: ['rust'], locks: [] }],
    unresolved: [],
  },
  untracked: [],
  host: { supported: false, platform: 'darwin', engines: [] },
  configuration: null,
  setups: [],
  runs: [],
  issues: [],
});
let calls: any[];
let answer: (args: any) => unknown;
let dispose: (() => void) | undefined;
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (command: string, args: any) => {
    assert.equal(command, 'polyglot_harness');
    calls.push(args);
    return answer(args);
  },
};
const flush = async () => {
  for (let index = 0; index < 6; index++) await new Promise((resolve) => setTimeout(resolve, 0));
};
afterEach(() => {
  dispose?.();
  dispose = undefined;
});
const selectHost = (root: HTMLElement, id: string) => {
  const select = root.querySelector(
    'select[aria-label="Verification execution host"]'
  ) as HTMLSelectElement;
  select.value = id;
  select.dispatchEvent(new Event('change', { bubbles: true }));
};

test('verification starts locally and routes to the named chat host only after explicit selection', async () => {
  calls = [];
  answer = (args) => inspection(args.executionHost);
  const root = container();
  dispose = render(
    () => <PolyglotHarnessSettings root="/project" unsavedFiles={[]} chat={chat} />,
    root
  );
  await flush();
  assert.equal(calls[0].executionHost, 'local');
  assert.equal(calls[0].dialogId, null);
  assert.match(root.textContent!, /Linux builder — API repair/);
  selectHost(root, 'remote-chat');
  await flush();
  assert.equal(calls.at(-1).executionHost, 'chat');
  assert.equal(calls.at(-1).dialogId, 'remote-chat');
  assert.match(root.textContent!, /Execution: Linux builder — API repair/);
});

test('losing the selected project chat blocks verification without falling back to this computer', async () => {
  calls = [];
  answer = (args) => inspection(args.executionHost);
  const [context, setContext] = createSignal<HarnessChatContext | undefined>(chat);
  const root = container();
  dispose = render(
    () => <PolyglotHarnessSettings root="/project" unsavedFiles={[]} chat={context()} />,
    root
  );
  await flush();
  selectHost(root, 'remote-chat');
  await flush();
  const count = calls.length;
  setContext(undefined);
  await flush();
  assert.equal(calls.length, count);
  assert.match(root.textContent!, /selected execution context changed or is unavailable/);
  assert.equal(root.querySelector('.polyglot-harness'), null);
  selectHost(root, 'local');
  await flush();
  assert.equal(calls.at(-1).executionHost, 'local');
});

test('an old local response cannot overwrite the explicitly selected remote project context', async () => {
  calls = [];
  let release!: (value: HarnessInspection) => void;
  answer = (args) =>
    args.executionHost === 'local'
      ? new Promise<HarnessInspection>((resolve) => {
          release = resolve;
        })
      : inspection('remote-project');
  const root = container();
  dispose = render(
    () => <PolyglotHarnessSettings root="/project" unsavedFiles={[]} chat={chat} />,
    root
  );
  await flush();
  selectHost(root, 'remote-chat');
  await flush();
  release(inspection('old-local-project'));
  await flush();
  assert.match(root.textContent!, /remote-project\/Cargo.toml/);
  assert.doesNotMatch(root.textContent!, /old-local-project/);
});
