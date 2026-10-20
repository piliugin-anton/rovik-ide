// aiSettings.ts tests (node:test). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createAiSettingsHolder,
  loadAiSettings,
  mergeRemoteModels,
  saveAiSettings,
  saveKeepingSubagents,
  type PersistedAiSettings,
} from './aiSettings';
import { DEFAULT_PERMISSIONS } from './permissions';
import { DEFAULT_SUBAGENT_SETTINGS, type SubagentSettings } from './subagentSettings';

/* The backend's commands, answered here: none unless a test says so. */
let backend: (cmd: string, args: any) => Promise<unknown> = async (cmd) => {
  throw new Error(`no app: ${cmd}`);
};
(globalThis as any).window = {
  __TAURI_INTERNALS__: { invoke: (cmd: string, args: unknown) => backend(cmd, args) },
};

/** Runs `body` with console.error silenced (failed loads log). */
async function quietly<T>(body: () => Promise<T>): Promise<T> {
  const original = console.error;
  console.error = () => {};
  try {
    return await body();
  } finally {
    console.error = original;
  }
}

test('settings that could not be read are marked, not passed off as saved', async () => {
  // No app here: reading the file fails.
  const settings = await quietly(loadAiSettings);
  assert.equal(settings.loadFailed, true);
  assert.equal(settings.subagents.enabled, false, 'the defaults stand in');
});

test('save_ai_settings_payload_never_contains_load_failed', async () => {
  const sent: string[] = [];
  backend = async (cmd, args) => {
    if (cmd === 'save_ai_settings') sent.push(args.json);
  };
  await saveAiSettings({
    version: 2,
    providers: [],
    active: null,
    permissions: DEFAULT_PERMISSIONS,
    subagents: DEFAULT_SUBAGENT_SETTINGS,
    loadFailed: true,
  });
  assert.equal(sent.length, 1);
  const saved = JSON.parse(sent[0]);
  assert.equal('loadFailed' in saved, false);
  assert.equal(saved.version, 2);
  assert.deepEqual(saved.subagents, JSON.parse(JSON.stringify(DEFAULT_SUBAGENT_SETTINGS)));
});

const mine: SubagentSettings = { ...DEFAULT_SUBAGENT_SETTINGS, enabled: true, maxPerRun: 3 };
const edited = {
  providers: [],
  active: null,
  permissions: DEFAULT_PERMISSIONS,
  // Defaults: what a failed load left in place of the saved ones.
  subagents: DEFAULT_SUBAGENT_SETTINGS,
};

test('save_keeping_subagents: unknown and the reload fails → nothing is saved (P5.7)', async () => {
  const saves: PersistedAiSettings[] = [];
  const result = await saveKeepingSubagents(edited, true, {
    load: async () => ({ ...edited, version: 2, loadFailed: true }),
    save: async (s) => void saves.push(s),
  });
  assert.equal(result, null);
  assert.equal(saves.length, 0, 'the saved subagent settings aren’t written over');
});

test('save_keeping_subagents: unknown and the reload works → saved with the reloaded subagents', async () => {
  const saves: PersistedAiSettings[] = [];
  const result = await saveKeepingSubagents(edited, true, {
    load: async () => ({ ...edited, version: 2, subagents: mine }),
    save: async (s) => void saves.push(s),
  });
  assert.deepEqual(result, mine);
  assert.equal(saves.length, 1);
  assert.deepEqual(saves[0].subagents, mine);
  assert.equal(saves[0].version, 2);
});

test('save_keeping_subagents: known settings are saved as they are, without a reload', async () => {
  const saves: PersistedAiSettings[] = [];
  let loads = 0;
  const result = await saveKeepingSubagents({ ...edited, subagents: mine }, false, {
    load: async () => {
      loads += 1;
      return { ...edited, version: 2 };
    },
    save: async (s) => void saves.push(s),
  });
  assert.deepEqual(result, mine);
  assert.equal(loads, 0);
  assert.deepEqual(saves[0].subagents, mine);
});

test('ai_settings_holder: a failed load is remembered until a save reads the saved ones', async () => {
  const taken: SubagentSettings[] = [];
  const saves: PersistedAiSettings[] = [];
  let reload: PersistedAiSettings = { ...edited, version: 2, loadFailed: true };
  const holder = createAiSettingsHolder(
    { setProviders() {}, setPermissions() {}, setSubagents: (s) => void taken.push(s) },
    { load: async () => reload, save: async (s) => void saves.push(s) }
  );
  // Loaded, but the file couldn't be read: defaults stand in.
  holder.apply({ ...edited, version: 2, loadFailed: true });
  assert.deepEqual(taken, [DEFAULT_SUBAGENT_SETTINGS]);
  // Saving reads again; still unreadable: nothing is saved.
  assert.equal(await quietly(() => holder.save(edited)), false);
  assert.equal(saves.length, 0);
  // Readable now: saved with the saved subagent settings, which are taken in.
  reload = { ...edited, version: 2, subagents: mine };
  assert.equal(await holder.save(edited), true);
  assert.deepEqual(saves[0].subagents, mine);
  assert.deepEqual(taken[1], mine);
  // Known from then on: saved as they are, without a reload.
  reload = { ...edited, version: 2, loadFailed: true };
  assert.equal(await holder.save({ ...edited, subagents: mine }), true);
  assert.deepEqual(saves[1].subagents, mine);
  assert.equal(taken.length, 2);
});

test('save_keeping_subagents: the real load failing saves nothing', async () => {
  const sent: string[] = [];
  backend = async (cmd, args) => {
    if (cmd === 'save_ai_settings') sent.push(args.json);
    else throw new Error('disk on fire');
  };
  const result = await quietly(() => saveKeepingSubagents(edited, true));
  assert.equal(result, null);
  assert.equal(sent.length, 0);
});

test('ai_settings_holder: each save goes with the copy it was made of, for the backend to save it onto the settings as they are now', async () => {
  const saves: { settings: PersistedAiSettings; base?: PersistedAiSettings }[] = [];
  const holder = createAiSettingsHolder(
    { setProviders() {}, setPermissions() {}, setSubagents() {} },
    {
      load: async () => ({ ...edited, version: 2 }),
      save: async (settings, base) => void saves.push({ settings, base }),
    }
  );
  const loaded: PersistedAiSettings = { ...edited, version: 2, subagents: mine };
  holder.apply(loaded);
  const first = { ...edited, subagents: mine, active: { providerId: 'p', modelId: 'm' } };
  assert.equal(await holder.save(first), true);
  assert.deepEqual(saves[0].base, loaded, 'the copy as loaded');
  // The next save was made of the first.
  assert.equal(await holder.save({ ...first, active: null }), true);
  assert.deepEqual(saves[1].base, { ...first, version: 2 });
  // After a load that failed, no copy to go by: saved as it is.
  holder.apply({ ...edited, version: 2, loadFailed: true });
  await holder.save(edited);
  assert.equal(saves[2].base, undefined);
});

test('fetched models: the first fetch enables all, later ones add new models disabled', () => {
  const first = mergeRemoteModels(
    [],
    [
      { id: 'a', owned_by: 'x', context_length: 1000 },
      { id: 'b', context_length: null },
    ]
  );
  assert.equal(first.added, 2);
  assert.deepEqual(first.models, [
    { id: 'a', ownedBy: 'x', contextLength: 1000, enabled: true },
    { id: 'b', ownedBy: undefined, contextLength: undefined, enabled: true },
  ]);

  // A refresh keeps the choices made, adds new models disabled, and keeps
  // models added by hand that the list doesn't have.
  const current = [
    { id: 'a', enabled: false, contextLength: 1000, noTools: true },
    { id: 'mine', enabled: true },
  ];
  const later = mergeRemoteModels(current, [{ id: 'c' }, { id: 'a', context_length: 2000 }]);
  assert.equal(later.added, 1);
  assert.deepEqual(
    later.models.map((m) => [m.id, m.enabled, m.contextLength, m.noTools]),
    [
      ['c', false, undefined, undefined],
      ['a', false, 2000, true],
      ['mine', true, undefined, undefined],
    ]
  );
});

test('fetched models: vision and PDF come from the provider where it says, unless already set', () => {
  const { models } = mergeRemoteModels(
    [
      { id: 'set-off', enabled: true, vision: false, pdf: false },
      { id: 'unset', enabled: true },
    ],
    [
      { id: 'set-off', vision: true, pdf: true },
      { id: 'unset', vision: true, pdf: false },
      { id: 'new', vision: true, pdf: true },
      { id: 'silent', vision: null },
    ]
  );
  const inputs = Object.fromEntries(models.map((m) => [m.id, [m.vision, m.pdf]]));
  assert.deepEqual(inputs, {
    'set-off': [false, false],
    unset: [true, false],
    new: [true, true],
    silent: [undefined, undefined],
  });
});
