// aiSettings.ts tests (node:test). Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  loadAiSettings,
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
