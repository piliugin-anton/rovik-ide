import { invoke } from '@tauri-apps/api/core';

export async function loadLastProject(): Promise<string | null> {
  try {
    const raw = await invoke<string>('load_last_project');
    const parsed = JSON.parse(raw) as { path: string | null };

    if (typeof parsed.path !== 'string') {
      return null;
    }

    return parsed.path;
  } catch (err) {
    console.error('Failed to load last project:', err);
    return null;
  }
}

export async function saveLastProject(path: string): Promise<void> {
  await invoke<void>('save_last_project', { path });
}