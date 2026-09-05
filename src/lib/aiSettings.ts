import { invoke } from '@tauri-apps/api/core';
import type { ModelProfile } from '../types';

interface PersistedAiSettings {
  profiles: ModelProfile[];
  activeProfileId: string | null;
}

const EMPTY: PersistedAiSettings = { profiles: [], activeProfileId: null };

export async function loadAiSettings(): Promise<PersistedAiSettings> {
  try {
    const raw = await invoke<string>('load_ai_settings');
    const parsed = JSON.parse(raw);
    return {
      profiles: Array.isArray(parsed.profiles) ? parsed.profiles : [],
      activeProfileId: parsed.activeProfileId ?? null,
    };
  } catch (err) {
    console.error('Failed to load AI settings:', err);
    return EMPTY;
  }
}

export async function saveAiSettings(settings: PersistedAiSettings): Promise<void> {
  await invoke<void>('save_ai_settings', { json: JSON.stringify(settings) });
}
