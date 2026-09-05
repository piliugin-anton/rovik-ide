import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import type { DirEntryInfo, ToolDirEntry, ToolSearchMatch } from '../types';

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

// --- Agent file tools -------------------------------------------------
// Backed by src-tauri/src/agent_tools.rs. All paths are relative to `root`
// (the currently opened project folder) and confined to it on the Rust side.

export async function toolListDir(
  root: string,
  path?: string,
  maxDepth?: number
): Promise<ToolDirEntry[]> {
  return invoke<ToolDirEntry[]>('tool_list_dir', { root, path, maxDepth });
}

export async function toolSearchCode(
  root: string,
  query: string,
  pathGlob?: string,
  maxResults?: number
): Promise<ToolSearchMatch[]> {
  return invoke<ToolSearchMatch[]>('tool_search_code', { root, query, pathGlob, maxResults });
}

export async function toolReadFileRange(
  root: string,
  path: string,
  startLine?: number,
  endLine?: number
): Promise<string> {
  return invoke<string>('tool_read_file_range', { root, path, startLine, endLine });
}

export async function toolWriteFile(root: string, path: string, content: string): Promise<string> {
  return invoke<string>('tool_write_file', { root, path, content });
}
