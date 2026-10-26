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

/**
 * What the window shows is kept for its next start: a folder's path, or the
 * workspace's ID (with no path: a workspace has none), and its tabs
 * (docs/design/workspaces-plan.md, D17).
 */
export async function saveLastProject(
  path: string,
  workspace: string | null,
  openedFiles: OpenFile[],
  expandedPaths: string[],
  terminalCollapsed: boolean
): Promise<void> {
  await invoke<void>('save_last_project', {
    path,
    workspace,
    openedFiles: openedFiles.map((f) => ({
      id: f.id,
      path: f.path,
      name: f.name,
      language: f.language,
    })),
    expandedPaths,
    terminalCollapsed,
  });
}
