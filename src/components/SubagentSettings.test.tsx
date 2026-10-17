// Component tests: Settings → Subagents and what it says when virtual files
// (and so isolated chats) aren't there. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import SubagentSettings from './SubagentSettings';
import {
  DEFAULT_SUBAGENT_SETTINGS,
  type SubagentSettings as Settings,
} from '../lib/subagentSettings';

/** What `vfs_status` answers in the test running now. */
let vfsStatus: () => Promise<unknown> = async () => ({ mountpoint: '/mnt/vfs' });
(window as any).__TAURI_INTERNALS__ = {
  invoke: (cmd: string) =>
    cmd === 'vfs_status' ? vfsStatus() : Promise.reject(new Error(`no app: ${cmd}`)),
};

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const ISOLATED = [
  'Run plan groups in parallel',
  'Red check for the test writer’s tests',
  'Fix a parallel group’s failures in parallel',
];

/** Renders the page, subagents on, with its limits open. */
async function settingsPage(): Promise<HTMLElement> {
  const root = container();
  render(
    () => (
      <SubagentSettings
        settings={{ ...DEFAULT_SUBAGENT_SETTINGS, enabled: true }}
        providers={[]}
        chatModel={null}
        onChange={() => {}}
      />
    ),
    root
  );
  [...root.querySelectorAll('button')].find((b) => b.textContent?.includes('Limits'))!.click();
  await flush();
  return root;
}

const checkbox = (root: HTMLElement, label: string) =>
  [...root.querySelectorAll('label.setting-checkbox')]
    .find((l) => l.textContent?.includes(label))!
    .querySelector('input') as HTMLInputElement;

const noVfsHint = (root: HTMLElement) =>
  [...root.querySelectorAll('.settings-hint')].find((p) =>
    p.textContent?.includes('virtual files aren’t mounted')
  );

test('subagent settings: a failed vfsStatus isn’t “not mounted” (P5.7)', async () => {
  vfsStatus = () => Promise.reject(new Error('vfs_status unavailable'));
  const root = await settingsPage();
  for (const label of ISOLATED) {
    const input = checkbox(root, label);
    assert.equal(input.disabled, false, `${label}: not disabled for an unknown reason`);
    assert.ok(!input.title, `${label}: no reason given that may be wrong`);
  }
  assert.equal(noVfsHint(root), undefined);
});

test('subagent settings: no virtual files mounted turns off what needs isolated chats', async () => {
  vfsStatus = async () => ({ mountpoint: null });
  const root = await settingsPage();
  for (const label of ISOLATED) {
    const input = checkbox(root, label);
    assert.equal(input.disabled, true, label);
    assert.match(input.title, /virtual files aren’t mounted/, label);
  }
  assert.ok(noVfsHint(root));
});

test('subagent settings: mounted, the isolated options are on offer; no token limit by default', async () => {
  vfsStatus = async () => ({ mountpoint: '/mnt/vfs' });
  const root = await settingsPage();
  for (const label of ISOLATED) assert.equal(checkbox(root, label).disabled, false, label);
  assert.equal(noVfsHint(root), undefined);
  const tokens = [...root.querySelectorAll('label.subagent-limit')]
    .find((l) => l.textContent?.includes('Tokens per run'))!
    .querySelector('input') as HTMLInputElement;
  assert.equal(tokens.placeholder, 'no limit');
  assert.equal(tokens.value, '');
});

/** The Autopilot section's radios, by label. */
const effortRadio = (root: HTMLElement, label: string) =>
  [...root.querySelectorAll('label.setting-checkbox')]
    .find((l) => l.textContent?.startsWith(label))!
    .querySelector('input[type="radio"]') as HTMLInputElement;

test('subagent settings: Autopilot’s effort, Balanced by default, whatever the master switch', () => {
  const root = container();
  const saved: Settings[] = [];
  const [settings, setSettings] = createSignal<Settings>(DEFAULT_SUBAGENT_SETTINGS);
  render(
    () => (
      <SubagentSettings
        settings={settings()}
        providers={[]}
        chatModel={null}
        onChange={(next) => {
          saved.push(next);
          setSettings(next);
        }}
      />
    ),
    root
  );
  const title = [...root.querySelectorAll('.settings-section-title')].map((t) => t.textContent);
  assert.deepEqual(title.slice(0, 3), ['Default model', 'Autopilot', 'Kinds'], 'above the kinds');
  const balanced = effortRadio(root, 'Balanced');
  const thorough = effortRadio(root, 'Thorough');
  assert.equal(balanced.checked, true);
  assert.equal(thorough.checked, false);
  // Subagents are off: Autopilot's setting is still there to set.
  assert.equal(balanced.disabled, false);
  assert.equal(thorough.disabled, false);
  assert.match(
    balanced.closest('label')!.textContent!,
    /Each stage once, with the subagents below\./
  );
  assert.match(
    thorough.closest('label')!.textContent!,
    /Several candidates, a second model for every judgment, an adversarial tester; takes longer and costs more\./
  );
  const notBuilt = () =>
    [...root.querySelectorAll('.settings-hint')].find((p) =>
      p.textContent?.includes('aren’t built yet')
    );
  assert.equal(notBuilt(), undefined);

  thorough.click();
  assert.equal(saved.length, 1);
  assert.deepEqual(saved[0], { ...DEFAULT_SUBAGENT_SETTINGS, autopilot: { effort: 'thorough' } });
  assert.equal(effortRadio(root, 'Thorough').checked, true);
  assert.ok(notBuilt(), 'what Thorough does today, said');
});
