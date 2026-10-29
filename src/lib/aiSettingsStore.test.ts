// aiSettingsStore.ts tests (node:test): the Settings dialog's state, read
// from the saved settings and saved on every change. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRoot } from 'solid-js';
import {
  aiAutopilot,
  aiChatModel,
  aiMcpServers,
  aiPermissions,
  aiProviders,
  aiSettingsLoaded,
  aiSettingsLoadFailed,
  aiSettingsSaved,
  aiSubagents,
  followAiSettingsRevision,
  reloadAiSettings,
  updateAutopilot,
  updateMcpServers,
  updatePermissions,
  updateProviders,
  updateSubagents,
  useAiSettingsIo,
} from './aiSettingsStore';
import { aiSettingsRevision, notifyAiSettingsSaved } from './appSettings';
import type { PersistedAiSettings } from './aiSettings';
import { DEFAULT_PERMISSIONS } from './permissions';
import { DEFAULT_SUBAGENT_SETTINGS, type SubagentSettings } from './subagentSettings';
import type { Provider } from '../types';

const provider = (id: string, models: string[]): Provider => ({
  id,
  name: id,
  baseUrl: `http://${id}`,
  apiKey: '',
  stream: true,
  models: models.map((m) => ({ id: m, enabled: true })),
});

/** A settings file in memory, and what was written to it. */
function file(initial: PersistedAiSettings) {
  let contents = initial;
  const saves: PersistedAiSettings[] = [];
  let loads = 0;
  useAiSettingsIo({
    load: async () => {
      loads += 1;
      return contents;
    },
    save: async (s) => {
      saves.push(s);
      contents = s;
    },
  });
  return {
    saves,
    loads: () => loads,
    set: (next: PersistedAiSettings) => void (contents = next),
  };
}

const saved: PersistedAiSettings = {
  version: 2,
  providers: [provider('a', ['m1', 'm2'])],
  active: { providerId: 'a', modelId: 'm2' },
  permissions: { ...DEFAULT_PERMISSIONS, allowAnyCommand: true },
  subagents: { ...DEFAULT_SUBAGENT_SETTINGS, enabled: true },
  mcpServers: [
    {
      id: 's1',
      name: 'docs',
      enabled: true,
      transport: { kind: 'url', url: 'https://docs.example/mcp', headers: [] },
      disabledTools: [],
    },
  ],
};

/** Runs `body` with console.error silenced (a failed save logs). */
async function quietly<T>(body: () => Promise<T>): Promise<T> {
  const original = console.error;
  console.error = () => {};
  try {
    return await body();
  } finally {
    console.error = original;
  }
}

test('ai settings store: a reload takes the saved settings and the chat’s model in', async () => {
  file(saved);
  assert.equal(aiSettingsLoaded(), false);
  await reloadAiSettings();
  assert.equal(aiSettingsLoaded(), true);
  assert.deepEqual(aiProviders(), saved.providers);
  assert.equal(aiPermissions().allowAnyCommand, true);
  assert.equal(aiSubagents().enabled, true);
  assert.deepEqual(aiChatModel(), { providerId: 'a', modelId: 'm2' });
});

test('ai settings store: every change is saved with the latest state, and the chat panel told', async () => {
  const f = file(saved);
  await reloadAiSettings();
  const revision = aiSettingsRevision();
  updatePermissions({ ...aiPermissions(), allowAnyCommand: false });
  updateSubagents({ ...aiSubagents(), maxPerRun: 4 });
  await aiSettingsSaved();
  // Two changes in one go: written once, both in it.
  assert.equal(f.saves.length, 1);
  assert.equal(f.saves[0].permissions.allowAnyCommand, false);
  assert.equal(f.saves[0].subagents.maxPerRun, 4);
  assert.equal(f.saves[0].version, 2);
  assert.equal('loadFailed' in f.saves[0], false);
  assert.equal(aiSettingsRevision(), revision + 1, 'the chat panel reads it again');

  updateAutopilot({ effort: 'thorough' });
  await aiSettingsSaved();
  assert.equal(f.saves.length, 2);
  assert.equal(f.saves[1].subagents.autopilot.effort, 'thorough');
  assert.equal(aiAutopilot().effort, 'thorough');
  assert.equal(aiSettingsRevision(), revision + 2);
  assert.deepEqual(f.saves[1].mcpServers, saved.mcpServers, 'the MCP servers go with every save');

  updateMcpServers([{ ...aiMcpServers()[0], enabled: false }]);
  await aiSettingsSaved();
  assert.equal(f.saves.length, 3);
  assert.equal(f.saves[2].mcpServers[0].enabled, false);
  assert.equal(aiSettingsRevision(), revision + 3);
});

test('ai settings store: a save keeps the chat’s model the chat panel chose meanwhile', async () => {
  const f = file(saved);
  await reloadAiSettings();
  // The chat panel switched the chat to m1 and saved it.
  f.set({ ...saved, active: { providerId: 'a', modelId: 'm1' } });
  updatePermissions({ ...aiPermissions(), checklist: 'always' });
  await aiSettingsSaved();
  assert.deepEqual(f.saves[0].active, { providerId: 'a', modelId: 'm1' });
  assert.deepEqual(aiChatModel(), { providerId: 'a', modelId: 'm1' });
});

test('ai settings store: a removed provider loses the chat’s model, which moves to the next', async () => {
  const f = file(saved);
  await reloadAiSettings();
  const next = [provider('b', ['other'])];
  updateProviders(next);
  assert.deepEqual(aiChatModel(), { providerId: 'b', modelId: 'other' }, 'at once, on the page');
  await aiSettingsSaved();
  assert.deepEqual(f.saves[0].providers, next);
  assert.deepEqual(f.saves[0].active, { providerId: 'b', modelId: 'other' });
});

test('ai settings store: settings that couldn’t be read are never written over (P5.7)', async () => {
  const mine: SubagentSettings = { ...DEFAULT_SUBAGENT_SETTINGS, enabled: true, maxPerRun: 3 };
  const f = file({ ...saved, subagents: DEFAULT_SUBAGENT_SETTINGS, loadFailed: true });
  await reloadAiSettings();
  assert.equal(aiSettingsLoadFailed(), true);
  assert.deepEqual(aiSubagents(), DEFAULT_SUBAGENT_SETTINGS, 'the defaults stand in');
  // A change made on the defaults isn't saved over the file.
  updatePermissions({ ...aiPermissions(), checklist: 'off' });
  await quietly(aiSettingsSaved);
  assert.equal(f.saves.length, 0);
  // Readable now ("Try again"): the saved settings replace the defaults,
  // the change made on them included, and saving resumes.
  f.set({ ...saved, subagents: mine });
  await reloadAiSettings();
  assert.equal(aiSettingsLoadFailed(), false);
  assert.deepEqual(aiSubagents(), mine);
  assert.equal(aiPermissions().checklist, 'auto');
  updatePermissions({ ...aiPermissions(), checklist: 'always' });
  await aiSettingsSaved();
  assert.equal(f.saves.length, 1);
  assert.deepEqual(f.saves[0].subagents, mine);
  assert.equal(f.saves[0].permissions.checklist, 'always');
});

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

test('ai settings store: another window’s save is read in when the revision says so; its own aren’t read again', async () => {
  const f = file(saved);
  await reloadAiSettings();
  const dispose = createRoot((dispose) => {
    followAiSettingsRevision();
    return dispose;
  });
  try {
    const loads = f.loads();
    // Its own save: one read, for the chat's model; none for the bump it makes.
    updatePermissions({ ...aiPermissions(), checklist: 'off' });
    await aiSettingsSaved();
    await flush();
    assert.equal(f.loads(), loads + 1);
    assert.equal(aiPermissions().checklist, 'off');
    // Another window saved, and this one came to the front: the focus
    // re-read bumps the revision, and the store reads the file again.
    f.set({ ...saved, permissions: { ...saved.permissions, checklist: 'always' } });
    notifyAiSettingsSaved();
    await flush();
    assert.equal(f.loads(), loads + 2);
    assert.equal(aiPermissions().checklist, 'always');
  } finally {
    dispose();
  }
});
