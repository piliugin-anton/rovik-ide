import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import type { DirEntryInfo } from '../types';

/** Opens the native "select a folder" dialog. Returns null if the user cancels. */
export async function pickProjectFolder(): Promise<string | null> {
  const result = await open({ directory: true, multiple: false });
  if (Array.isArray(result)) return result[0] ?? null;
  return result ?? null;
}

/** Lists the immediate children of a directory (non-recursive — call again per expand). */
export async function readDirEntries(path: string): Promise<DirEntryInfo[]> {
  return invoke<DirEntryInfo[]>('read_dir_entries', { path });
}

export async function readFileContent(path: string): Promise<string> {
  return invoke<string>('read_file_content', { path });
}

export async function writeFileContent(path: string, content: string): Promise<void> {
  return invoke<void>('write_file_content', { path, content });
}
