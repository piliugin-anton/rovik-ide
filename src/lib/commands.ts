import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import type {
  BrowsedPage,
  BrowserScreenshot,
  DirEntryInfo,
  ToolDirEntry,
  ToolSearchResult,
  WebSearchHit,
} from '../types';

/** Opens the native "select a folder" dialog. Returns null if the user cancels. */
export async function pickProjectFolder(): Promise<string | null> {
  const result = await open({ directory: true, multiple: false });
  if (Array.isArray(result)) return result[0] ?? null;
  return result ?? null;
}

/** Lists the immediate children of a directory (non-recursive — call again per expand). */
export async function readDirEntries(rootPath: string, path: string): Promise<DirEntryInfo[]> {
  return invoke<DirEntryInfo[]>('read_dir_entries', { rootPath, path });
}

export async function readFileContent(path: string): Promise<string> {
  return invoke<string>('read_file_content', { path });
}

/** Starts the error of a save refused because the file changed on disk. */
export const CHANGED_ON_DISK = 'changed-on-disk:';

/**
 * Saves a file. With `expected` (the content the tab loaded), the save is
 * refused if the file on disk is no longer that: the error starts with
 * CHANGED_ON_DISK, followed by who changed it.
 */
export async function writeFileContent(
  path: string,
  content: string,
  expected?: string
): Promise<void> {
  return invoke<void>('write_file_content', { path, content, expected });
}

export async function pathExists(path: string): Promise<boolean> {
  return invoke<boolean>('path_exists', { path });
}

/**
 * Entries directly in a folder, hidden ones and .git included, counted up to
 * `limit`; null when the path isn't a folder.
 */
export async function dirEntryCount(path: string, limit = 10_000): Promise<number | null> {
  return invoke<number | null>('dir_entry_count', { path, limit });
}

// Bootstraps an id for a path the frontend knows directly (e.g. the project
// root), which never comes through read_dir_entries on its own.
export async function getPathId(path: string): Promise<string> {
  return invoke('get_path_id', { path });
}

/** Creates an empty file; resolves to its path. */
export async function createFile(dirId: string, name: string): Promise<string> {
  return invoke<string>('create_file', { dirId, name });
}

export async function createFolder(dirId: string, name: string): Promise<void> {
  await invoke('create_dir', { dirId, name });
}

/**
 * Moves a file or folder to the system trash. Fails with an error that
 * isTrashUnavailable() recognizes where there's no trash; `permanent` then
 * deletes it for good (only after the user agreed).
 */
export async function deletePath(id: string, permanent = false): Promise<void> {
  await invoke('delete_path', { id, permanent });
}

/** The delete failed because the trash can't be used there (see deletePath). */
export function isTrashUnavailable(err: unknown): boolean {
  return String(err).startsWith('trash-unavailable: ');
}

/**
 * Backend error kinds from the hardened rename/move/create commands
 * (src-tauri/src/filesystem.rs): a name the target already has, or a name
 * no OS (or Windows in particular) accepts. Matched the same way as
 * isTrashUnavailable so the UI can phrase them instead of dumping Rust text.
 */
export function isAlreadyExists(err: unknown): boolean {
  return String(err).startsWith('already-exists: ');
}

export function isInvalidName(err: unknown): boolean {
  return String(err).startsWith('invalid-name: ');
}

/** Strips the backend's `kind: ` prefixes for display in an alert. */
export function fsErrorMessage(err: unknown): string {
  return String(err).replace(/^(already-exists|invalid-name|trash-unavailable):\s*/, '');
}

// Rename in place (same parent dir, new name). Returns the new absolute
// path so callers can update any open tab.
export async function renamePath(id: string, newName: string): Promise<string> {
  return invoke('rename_path', { id, newName });
}

// Move into a different directory (optionally renaming), i.e. cut + paste.
// destDirId must be a directory's id. Returns the new absolute path.
export async function movePath(id: string, destDirId: string, newName?: string): Promise<string> {
  return invoke('move_path', { id, destDirId, newName });
}

// Copy into a different directory (optionally renaming), i.e. copy + paste.
// destDirId must be a directory's id.
export async function copyPath(id: string, destDirId: string, newName?: string): Promise<void> {
  await invoke('copy_path', { id, destDirId, newName });
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

export interface SearchCodeOptions {
  /** ripgrep-style globs, comma-separated; "!" excludes. */
  pathGlob?: string;
  maxResults?: number;
  regex?: boolean;
  caseSensitive?: boolean;
  contextLines?: number;
}

/** Project search on ripgrep's engine (src-tauri/src/code_search.rs). */
export function toolSearchCode(
  root: string,
  query: string,
  options: SearchCodeOptions = {}
): Promise<ToolSearchResult> {
  return invoke<ToolSearchResult>('tool_search_code', { root, query, ...options });
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

export interface EditFileArgs {
  path: string;
  oldText: string;
  newText: string;
  /** Confine the edit to these lines (1-based, inclusive). */
  startLine?: number;
  endLine?: number;
}

export interface EditOutcome {
  /** The whole file after the edit. */
  content: string;
  /** The tool result for the model. */
  summary: string;
}

/**
 * Replaces the one exact occurrence of `oldText` (src-tauri/src/edit_file.rs).
 * With `dryRun`, returns the result without writing it (for review).
 */
export function toolEditFile(
  root: string,
  args: EditFileArgs,
  dryRun = false
): Promise<EditOutcome> {
  return invoke<EditOutcome>('tool_edit_file', { root, ...args, dryRun });
}

/** Result of `tool_run_command` (src-tauri/src/command_tool.rs). */
export interface CommandOutput {
  exitCode: number | null;
  stdout: string;
  stderr: string;
  timedOut: boolean;
  /** Killed by the agent's Stop button. */
  stopped: boolean;
  truncated: boolean;
  durationMs: number;
}

/** Runs a shell command in the project root (permission checks happen before this). */
export async function toolRunCommand(
  root: string,
  command: string,
  timeoutMs?: number
): Promise<CommandOutput> {
  return invoke<CommandOutput>('tool_run_command', { root, command, timeoutMs });
}

/*
 * Web tools (src-tauri/src/browser_tools.rs). The first call starts a
 * headless Chrome, so it can take a few seconds.
 */

export function toolWebSearch(query: string, maxResults?: number): Promise<WebSearchHit[]> {
  return invoke('web_search', { query, maxResults });
}

export function toolBrowseUrl(url: string, maxChars?: number): Promise<BrowsedPage> {
  return invoke('browse_url', { url, maxChars });
}

export function toolBrowserScreenshot(url?: string): Promise<BrowserScreenshot> {
  return invoke('browser_screenshot', { url });
}

export interface SearchInFilesOptions {
  regex?: boolean;
  caseSensitive?: boolean;
  wholeWord?: boolean;
  /** Comma-separated ripgrep-style globs, e.g. "src/**, *.ts". */
  include?: string;
  exclude?: string;
  maxResults?: number;
}

/** Files-tab search (src-tauri/src/code_search.rs, ripgrep engine). */
export function searchInFiles(
  root: string,
  query: string,
  options: SearchInFilesOptions = {}
): Promise<ToolSearchResult> {
  return invoke<ToolSearchResult>('search_in_files', { root, query, ...options });
}
