// appSettings.ts
//
// The app-wide Settings dialog (components/SettingsDialog.tsx, owned by
// App.tsx, opened from the gear button next to the project selector): which
// page is open, and the signal that tells the chat panel when the AI
// settings were saved. ChatPanel used to own its own "AI Chat settings"
// modal; it now routes through here so settings stay open whichever
// sidebar tab is active.

import { createSignal } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';

/** The dialog's pages: the menu, and one page per thing to set. */
export type SettingsPage =
  | 'menu'
  | 'editor'
  | 'providers'
  | 'permissions'
  | 'agent'
  | 'subagents'
  | 'autopilot'
  | 'project'
  | 'workload'
  | 'vfs'
  | 'network'
  | 'hosts'
  | 'wsl';

export const PAGE_TITLE: Record<SettingsPage, string> = {
  menu: 'Settings',
  editor: 'Editor',
  providers: 'Model providers',
  permissions: 'Permissions & approval',
  agent: 'Agent runs',
  subagents: 'Subagents',
  autopilot: 'Autopilot',
  project: 'Project checks',
  workload: 'Heavy commands',
  vfs: 'Virtual files',
  network: 'Network',
  hosts: 'Cloud hosts',
  wsl: 'WSL',
};

const [page, setPage] = createSignal<SettingsPage | null>(null);

/** The page the dialog was opened on, or null when it's closed. */
export function appSettingsPage(): SettingsPage | null {
  return page();
}

/** Open the Settings dialog on a page (default: the menu). */
export function openAppSettings(next: SettingsPage = 'menu'): void {
  setPage(next);
}

export function closeAppSettings(): void {
  setPage(null);
}

// The AI settings (providers, permissions, subagents) are edited through
// lib/aiSettingsStore.ts and saved at once; ChatPanel keeps its own copy
// for its runs. Each save bumps this counter so ChatPanel reads the file
// again.
const [aiRevision, setAiRevision] = createSignal(0);

export function aiSettingsRevision(): number {
  return aiRevision();
}

export function notifyAiSettingsSaved(): void {
  setAiRevision((n) => n + 1);
}

/**
 * Another of Rovik's windows may have saved the AI settings meanwhile: when
 * this one comes to the front and the file was written since it last
 * looked, its copies are read again. The look doesn't touch the keychain,
 * which reading the settings does.
 */
export function rereadAiSettingsOnFocus(
  stamp: () => Promise<number> = () => invoke<number>('ai_settings_stamp')
): () => void {
  let seen: number | null = null;
  const look = async () => {
    try {
      const now = await stamp();
      if (seen !== null && now !== seen) notifyAiSettingsSaved();
      seen = now;
    } catch {
      // Not known: read when this window saves, as before.
    }
  };
  void look();
  const onFocus = () => void look();
  window.addEventListener('focus', onFocus);
  return () => window.removeEventListener('focus', onFocus);
}
