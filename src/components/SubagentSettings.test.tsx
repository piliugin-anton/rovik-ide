// Component tests: Settings → Subagents and what it says when virtual files
// (and so isolated chats) aren't there. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import SubagentSettings from './SubagentSettings';
import { AUTOPILOT_KINDS, DEFAULT_AUTOPILOT_DENYLIST } from '../lib/autopilotSettings';
import { SUBAGENT_KINDS } from '../lib/subagentSettings';
import type { Provider } from '../types';
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

test('subagent settings: the kinds Autopilot uses say so, and where their code goes (D17)', () => {
  const providers = [
    {
      id: 'a',
      name: 'Alpha',
      baseUrl: 'http://a',
      apiKey: '',
      stream: true,
      models: [{ id: 'm', enabled: true }],
    },
    {
      id: 'b',
      name: 'Beta',
      baseUrl: 'http://b',
      apiKey: '',
      stream: true,
      models: [{ id: 'other', enabled: true }],
    },
  ] as Provider[];
  const onBeta = { model: { providerId: 'b', modelId: 'other' } };
  const kinds = DEFAULT_SUBAGENT_SETTINGS.kinds;
  const settings: Settings = {
    ...DEFAULT_SUBAGENT_SETTINGS,
    kinds: {
      ...kinds,
      reviewer: { ...kinds.reviewer, ...onBeta },
      scout: { ...kinds.scout, ...onBeta },
    },
  };
  const root = container();
  render(
    () => (
      <SubagentSettings
        settings={settings}
        providers={providers}
        chatModel={{ providerId: 'a', modelId: 'm' }}
        onChange={() => {}}
      />
    ),
    root
  );
  const rows = [...root.querySelectorAll('.subagent-kind-row')];
  assert.equal(rows.length, SUBAGENT_KINDS.length);
  assert.deepEqual(
    rows.map((r) => r.querySelector('.autopilot-kind-tag')?.textContent ?? null),
    SUBAGENT_KINDS.map((k) =>
      ['planner', 'judge', 'breaker'].includes(k.id)
        ? 'Used by Autopilot (Thorough)'
        : AUTOPILOT_KINDS.includes(k.id)
          ? 'Used by Autopilot'
          : null
    )
  );
  const warning = (id: string) =>
    rows[SUBAGENT_KINDS.findIndex((k) => k.id === id)].querySelector('.warning')?.textContent;
  // Off on the page, and subagents off: Autopilot's chats still send it there.
  assert.equal(warning('reviewer'), 'In Autopilot, code will also be sent to Beta.');
  assert.equal(warning('scout'), undefined, 'nothing uses an off scout');
  // Subagents off: a kind Autopilot uses keeps its model and effort to set,
  // its switch (ordinary chats') doesn't; the others are greyed out whole.
  const effort = (label: string) =>
    root.querySelector(`select[aria-label="${label} reasoning effort"]`) as HTMLSelectElement;
  const toggle = (id: string) =>
    rows[SUBAGENT_KINDS.findIndex((k) => k.id === id)].querySelector(
      'input[type="checkbox"]'
    ) as HTMLInputElement;
  assert.equal(effort('Change reviewer').disabled, false);
  assert.equal(toggle('reviewer').disabled, true);
  assert.equal(effort('Scout').disabled, true);
  assert.equal(toggle('scout').disabled, true);
  // The default model, which they fall back to, too.
  const defaults = [...root.querySelectorAll('.settings-section')].find((section) =>
    section.textContent?.startsWith('Default model')
  )!;
  assert.equal(defaults.classList.contains('disabled'), false);
  const controls = [...defaults.querySelectorAll('select, button')] as HTMLButtonElement[];
  assert.ok(controls.length > 0, 'its picker is there');
  assert.ok(
    controls.every((c) => !c.disabled),
    'and on'
  );
});

test('subagent settings: the commands Autopilot may never run, the defaults until changed (U01)', () => {
  const root = container();
  const [settings, setSettings] = createSignal<Settings>(DEFAULT_SUBAGENT_SETTINGS);
  render(
    () => (
      <SubagentSettings
        settings={settings()}
        providers={[]}
        chatModel={null}
        onChange={setSettings}
      />
    ),
    root
  );
  const shown = () =>
    [...root.querySelectorAll('.command-row-text')].map((c) => c.textContent ?? '');
  const reset = () =>
    [...root.querySelectorAll('button')].find((b) => b.textContent === 'Reset list');
  assert.deepEqual(shown(), DEFAULT_AUTOPILOT_DENYLIST);
  assert.equal(reset(), undefined, 'nothing to reset');

  const input = root.querySelector(
    'input[aria-label="A command Autopilot may never run"]'
  ) as HTMLInputElement;
  const add = input.parentElement!.querySelector('button') as HTMLButtonElement;
  input.value = '  npm   publish ';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  add.click();
  assert.deepEqual(settings().autopilot.deniedCommands, [
    ...DEFAULT_AUTOPILOT_DENYLIST,
    'npm publish',
  ]);
  assert.deepEqual(shown().slice(-1), ['npm publish']);
  assert.equal(input.value, '', 'the input is cleared');

  const remove = root.querySelector('button[aria-label="Remove git push"]') as HTMLButtonElement;
  remove.click();
  assert.ok(!settings().autopilot.deniedCommands!.includes('git push'));
  assert.ok(reset(), 'a changed list can be reset');
  reset()!.click();
  assert.equal(settings().autopilot.deniedCommands, undefined);
  assert.deepEqual(shown(), DEFAULT_AUTOPILOT_DENYLIST);
});

test('subagent settings: a task’s budget, its effort’s defaults until set (U04)', () => {
  const root = container();
  const [settings, setSettings] = createSignal<Settings>(DEFAULT_SUBAGENT_SETTINGS);
  render(
    () => (
      <SubagentSettings
        settings={settings()}
        providers={[]}
        chatModel={null}
        onChange={setSettings}
      />
    ),
    root
  );
  const field = (label: string) =>
    [...root.querySelectorAll('label.subagent-limit')]
      .find((l) => l.textContent?.startsWith(label))!
      .querySelector('input') as HTMLInputElement;
  const hours = () => field('Stop a task after (hours)');
  const cost = () => field('or once it cost');
  // Balanced: 2 hours and 5.
  assert.equal(hours().value, '2');
  assert.equal(cost().value, '5');
  // Thorough: none, while it's not set.
  effortRadio(root, 'Thorough').click();
  assert.equal(hours().value, '');
  assert.equal(cost().placeholder, 'no limit');
  assert.equal(settings().autopilot.budget, undefined);
  // Set: kept, whatever the effort.
  hours().value = '1.5';
  hours().dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(settings().autopilot.budget, { maxHours: 1.5, maxCost: null });
  effortRadio(root, 'Balanced').click();
  assert.equal(hours().value, '1.5');
  assert.equal(cost().value, '');
  // Emptied back to the effort's own: it follows the effort again.
  hours().value = '2';
  hours().dispatchEvent(new Event('change', { bubbles: true }));
  cost().value = '5';
  cost().dispatchEvent(new Event('change', { bubbles: true }));
  assert.equal(settings().autopilot.budget, undefined);
});

test('subagent settings: Prepare before the first check, on until turned off (S03)', () => {
  const root = container();
  const [settings, setSettings] = createSignal<Settings>(DEFAULT_SUBAGENT_SETTINGS);
  render(
    () => (
      <SubagentSettings
        settings={settings()}
        providers={[]}
        chatModel={null}
        onChange={setSettings}
      />
    ),
    root
  );
  const prepare = () => checkbox(root, 'Prepare dependencies before the first check');
  assert.equal(prepare().checked, true);
  assert.equal(prepare().disabled, false, 'Autopilot’s, whatever “Use subagents” says');
  prepare().click();
  assert.equal(settings().autopilot.prepare, false);
  assert.equal(prepare().checked, false);
  // Back on: the default again, kept as nothing.
  prepare().click();
  assert.equal(settings().autopilot.prepare, undefined);
  assert.equal(prepare().checked, true);
});
