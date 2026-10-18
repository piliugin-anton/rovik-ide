// appSettings.ts
//
// The app-wide Settings dialog (owned by App.tsx, opened from the gear
// button next to the project selector). ChatPanel used to own its own
// "AI Chat settings" modal; it now routes through here so settings stay
// open whichever sidebar tab is active.

import { createSignal } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import type { AiSettingsPage } from '../components/AiSettingsModal';

const [page, setPage] = createSignal<AiSettingsPage | null>(null);

/** The open settings page, or null when the dialog is closed. */
export function appSettingsPage(): AiSettingsPage | null {
  return page();
}

/** Open the Settings dialog on a page (default: the menu). */
export function openAppSettings(next: AiSettingsPage = 'menu'): void {
  setPage(next);
}

export function closeAppSettings(): void {
  setPage(null);
}

// The AI settings (providers, permissions) still live in ChatPanel and are
// persisted from there. When the app-wide dialog saves them, it bumps this
// counter so ChatPanel reloads its copy.
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
