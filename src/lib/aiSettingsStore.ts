// aiSettingsStore.ts
//
// The AI settings as the Settings dialog edits them: the model providers,
// the agent's permissions, and the subagent settings (with Autopilot's).
// They are the app's actual, saved state, not a draft: the dialog reads
// them again from the backend's file each time it opens, every change is
// saved at once, and each save bumps `aiSettingsRevision` so the chat
// panel, which keeps its own copy for its runs, reads it again.
//
// Saves are serialized and coalesced: a change made while one is being
// written waits for it, and several quick changes are written once, with
// the latest state. Each save goes with the copy it was made of, for the
// backend to save it onto the settings as they are now (another window
// may have saved meanwhile: lib/aiSettings.ts, ProviderKeys::save_onto),
// and a save another window made is read in when this one comes to the
// front (appSettings.ts rereadAiSettingsOnFocus bumps the revision). The
// chat's model (`active`) is the chat panel's to choose: a save keeps what
// the file has, reconciled with the providers. While the file can't be
// read (P5.7), the store holds defaults and saves nothing: the dialog says
// so and offers to read it again.
import { createEffect, createSignal, on } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import { DEFAULT_PERMISSIONS, type AgentPermissions } from './permissions';
import { DEFAULT_SUBAGENT_SETTINGS, type SubagentSettings } from './subagentSettings';
import type { AutopilotSettings } from './autopilotSettings';
import {
  createAiSettingsHolder,
  loadAiSettings,
  reconcileSelection,
  type PersistedAiSettings,
} from './aiSettings';
import { aiSettingsRevision, notifyAiSettingsSaved } from './appSettings';

const [providers, setProviders] = createSignal<Provider[]>([]);
const [permissions, setPermissions] = createSignal<AgentPermissions>(DEFAULT_PERMISSIONS);
const [subagents, setSubagents] = createSignal<SubagentSettings>(DEFAULT_SUBAGENT_SETTINGS);
/** The chat's model, as saved: the Subagents page's "code also goes to…" warning uses it. */
const [chatModel, setChatModel] = createSignal<ModelSelection | null>(null);
/** False until the first load answered (the pages show nothing stale meanwhile). */
const [loaded, setLoaded] = createSignal(false);
/**
 * The last load couldn't read the file (P5.7): what the store holds are
 * defaults, not what's saved, so nothing is saved over the file until a
 * load reads it.
 */
const [loadFailed, setLoadFailed] = createSignal(false);

/** What the store reads and writes: the backend's file, unless a test says otherwise. */
export interface AiSettingsIo {
  load: () => Promise<PersistedAiSettings>;
  save: (settings: PersistedAiSettings) => Promise<void>;
}

let io: AiSettingsIo | undefined;
let holder = createAiSettingsHolder({ setProviders, setPermissions, setSubagents }, io);

/** Tests: where the store reads and writes, instead of the backend. */
export function useAiSettingsIo(next: AiSettingsIo | undefined): void {
  io = next;
  holder = createAiSettingsHolder({ setProviders, setPermissions, setSubagents }, io);
}

export const aiProviders = providers;
export const aiPermissions = permissions;
export const aiSubagents = subagents;
export const aiAutopilot = (): AutopilotSettings => subagents().autopilot;
export const aiChatModel = chatModel;
export const aiSettingsLoaded = loaded;
export const aiSettingsLoadFailed = loadFailed;

/**
 * Reads the saved settings in (the dialog opening, say). A change still
 * being saved is written first, so the file read has it.
 */
export async function reloadAiSettings(): Promise<void> {
  await saving;
  const settings = await (io?.load ?? loadAiSettings)();
  holder.apply(settings);
  setChatModel(reconcileSelection(settings.providers, settings.active));
  setLoadFailed(!!settings.loadFailed);
  setLoaded(true);
}

export function updateProviders(next: Provider[]): void {
  setProviders(next);
  setChatModel(reconcileSelection(next, chatModel()));
  scheduleSave();
}

export function updatePermissions(next: AgentPermissions): void {
  setPermissions(next);
  scheduleSave();
}

export function updateSubagents(next: SubagentSettings): void {
  setSubagents(next);
  scheduleSave();
}

export function updateAutopilot(next: AutopilotSettings): void {
  updateSubagents({ ...subagents(), autopilot: next });
}

let saving: Promise<void> = Promise.resolve();
/** A save is queued and hasn't read the state yet: it will take this change too. */
let queued = false;
/** The revision the store's own last save bumped to: not another window's. */
let ownRevision = -1;

function scheduleSave(): void {
  if (queued) return;
  queued = true;
  saving = saving
    .then(async () => {
      queued = false;
      if (loadFailed()) {
        console.error('AI settings not saved: the saved ones couldn’t be read.');
        return;
      }
      // The chat's model is the chat panel's choice: keep the file's,
      // against the providers as they are now.
      const current = await (io?.load ?? loadAiSettings)().catch(() => null);
      const active = reconcileSelection(providers(), current?.active ?? null);
      setChatModel(active);
      const saved = await holder.save({
        providers: providers(),
        active,
        permissions: permissions(),
        subagents: subagents(),
      });
      if (saved) {
        // Before the bump: the effect following the revision runs with it.
        ownRevision = aiSettingsRevision() + 1;
        notifyAiSettingsSaved();
      }
    })
    .catch((err) => console.error('Failed to save AI settings:', err));
}

/** Resolves once every change made so far is saved (or failed to be). */
export function aiSettingsSaved(): Promise<void> {
  return saving;
}

/**
 * Reads the settings again whenever the revision says they were saved by
 * someone else: another window, as this one comes to the front
 * (rereadAiSettingsOnFocus). The store's own saves bump it too, and are
 * skipped: it has what it wrote. Call once, within a component.
 */
export function followAiSettingsRevision(): void {
  createEffect(
    on(
      aiSettingsRevision,
      (revision) => {
        if (revision === ownRevision) return;
        void reloadAiSettings().catch((err) => console.error(err));
      },
      { defer: true }
    )
  );
}
