import { invoke } from '@tauri-apps/api/core';
import { LastProjectData, OpenFile } from '../types';

export async function loadLastProject(): Promise<LastProjectData | null> {
  try {
    const raw = await invoke<string>('load_last_project');
    const parsed = JSON.parse(raw) as LastProjectData;

    if (typeof parsed.path !== 'string') {
      return null;
    }

    return parsed;
  } catch (err) {
    console.error('Failed to load last project:', err);
    return null;
  }
}

export async function saveLastProject(path: string, opened_files: OpenFile[]): Promise<void> {
  await invoke<void>('save_last_project', {
    path,
    openedFiles: opened_files.map((f) => ({
      id: f.id,
      path: f.path,
      name: f.name,
      language: f.language,
    })),
  });
}
