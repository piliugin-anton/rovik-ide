// appSettings.ts
//
// The app-wide Settings dialog (owned by App.tsx, opened from the gear
// button next to the project selector). ChatPanel used to own its own
// "AI Chat settings" modal; it now routes through here so settings stay
// open whichever sidebar tab is active.

import { createSignal } from 'solid-js';
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
