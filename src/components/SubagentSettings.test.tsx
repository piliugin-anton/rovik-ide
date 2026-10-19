// Component tests: Settings → Subagents and what it says when virtual files
// (and so isolated chats) aren't there. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import SubagentSettings from './SubagentSettings';
import { AUTOPILOT_KINDS } from '../lib/autopilotSettings';
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
        onOpenPage={() => {}}
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
  [...root.querySelectorAll('.settings-status')].find((p) =>
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
  const hint = noVfsHint(root);
  assert.ok(hint, 'says why they have no effect');
  assert.match(hint!.textContent!, /^No effect now/);
  assert.ok(hint!.querySelector('button'), 'and links to Virtual files');
});

test('subagent settings: mounted, the isolated options are on offer; no token limit by default', async () => {
  vfsStatus = async () => ({ mountpoint: '/mnt/vfs' });
  const root = await settingsPage();
  for (const label of ISOLATED) assert.equal(checkbox(root, label).disabled, false, label);
  assert.equal(noVfsHint(root), undefined);
  const tokens = [...root.querySelectorAll('label.setting-number-field')]
    .find((l) => l.textContent?.includes('Tokens per run'))!
    .querySelector('input') as HTMLInputElement;
  assert.equal(tokens.placeholder, 'no limit');
  assert.equal(tokens.value, '');
});

test('subagent settings: the page is the master switch, the default model, the kinds and the limits', () => {
  const root = container();
  render(
    () => (
      <SubagentSettings
        settings={DEFAULT_SUBAGENT_SETTINGS}
        providers={[]}
        chatModel={null}
        onChange={() => {}}
        onOpenPage={() => {}}
      />
    ),
    root
  );
  const titles = [...root.querySelectorAll('.settings-section-title')].map((t) => t.textContent);
  assert.deepEqual(titles, ['Default model', 'Kinds']);
  // Autopilot has a page of its own: nothing of it is set here.
  assert.equal(root.querySelector('input[name="autopilot-effort"]'), null);
  assert.equal(
    [...root.querySelectorAll('label')].find((l) =>
      l.textContent?.includes('Prepare dependencies')
    ),
    undefined
  );
  // The kinds say where Autopilot's settings are.
  const kinds = [...root.querySelectorAll('.settings-section')].find((section) =>
    section.querySelector('.settings-section-title')?.textContent?.startsWith('Kinds')
  )!;
  assert.match(kinds.textContent!, /also run in Autopilot’s chats/);
  assert.ok(
    [...kinds.querySelectorAll('button')].some((b) => b.textContent === 'Autopilot'),
    'with a link to its page'
  );
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
        onOpenPage={() => {}}
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
  // T09: nothing picks another provider's model for a row on Default.
  assert.equal(warning('acceptance'), undefined);
  assert.equal(warning('judge'), undefined, 'no model chosen: none to send code to');
  assert.equal(warning('triage'), undefined);
  assert.ok(
    root.textContent?.includes('Thorough tasks don’t start until each has one'),
    'the hint says Thorough’s kinds need a model chosen'
  );
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

test('subagent settings: Thorough’s kinds have no Default, and say a model is still to choose (T09)', () => {
  const [providers, setProviders] = createSignal([
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
  ] as Provider[]);
  const kinds = DEFAULT_SUBAGENT_SETTINGS.kinds;
  const [settings, setSettings] = createSignal<Settings>({
    ...DEFAULT_SUBAGENT_SETTINGS,
    autopilot: { effort: 'thorough' },
    // Saved before: "Same as the chat" is no choice for it now.
    kinds: { ...kinds, breaker: { ...kinds.breaker, model: 'chat' } },
  });
  const root = container();
  render(
    () => (
      <SubagentSettings
        settings={settings()}
        providers={providers()}
        chatModel={{ providerId: 'a', modelId: 'm' }}
        onChange={setSettings}
        onOpenPage={() => {}}
      />
    ),
    root
  );
  const row = (id: string) =>
    [...root.querySelectorAll('.subagent-kind-row')][SUBAGENT_KINDS.findIndex((k) => k.id === id)];
  const picker = (id: string) => row(id).querySelector('select') as HTMLSelectElement;
  const options = (id: string) => [...picker(id).options].map((o) => o.textContent);
  const choose = (id: string) =>
    [...row(id).querySelectorAll('.setting-explain')].find((p) =>
      p.textContent?.startsWith('Choose a model for it')
    );
  for (const id of ['planner', 'judge', 'breaker']) {
    assert.deepEqual(options(id), ['Choose a model…', 'Alpha/m', 'Beta/other'], id);
    assert.equal(picker(id).selectedOptions[0]?.textContent, 'Choose a model…', id);
    assert.ok(picker(id).selectedOptions[0]?.disabled, `${id}: the placeholder can't be chosen`);
    const hint = choose(id);
    assert.equal(
      hint?.textContent,
      'Choose a model for it: Thorough tasks don’t start until it has one.'
    );
    assert.ok(hint!.classList.contains('warning'), `${id}: a warning, the effort being Thorough`);
  }
  // The others keep Default and "Same as the chat".
  assert.deepEqual(options('acceptance').slice(0, 2), ['Default', 'Same as the chat']);
  assert.equal(choose('acceptance'), undefined);
  // Chosen: the hint goes, and the warning says where its code goes.
  const judge = picker('judge');
  judge.value = [...judge.options].find((o) => o.textContent === 'Beta/other')!.value;
  judge.dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(settings().kinds.judge.model, { providerId: 'b', modelId: 'other' });
  assert.equal(choose('judge'), undefined);
  assert.equal(
    row('judge').querySelector('.warning')?.textContent,
    'In Autopilot, code will also be sent to Beta.'
  );
  // Balanced: still said, though not as a warning.
  setSettings({ ...settings(), autopilot: { effort: 'balanced' } });
  assert.equal(choose('planner')!.classList.contains('warning'), false);
  // Its model turned off: nothing but its own fallbacks stands in for it.
  setProviders(
    providers().map((p) =>
      p.id === 'b' ? { ...p, models: p.models.map((m) => ({ ...m, enabled: false })) } : p
    )
  );
  const off = [...row('judge').querySelectorAll('.setting-explain')].map((p) => p.textContent);
  assert.ok(
    off.includes(
      'This model is turned off, removed, or can’t use tools, so it’s skipped: only the fallbacks here stand in for it.'
    ),
    off.join(' | ')
  );
  assert.ok(choose('judge'), 'and a model is to choose again');
});
