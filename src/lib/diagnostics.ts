// diagnostics.ts
//
// Reports what the editor shows to the backend, so the agent's
// `diagnostics` tool sees the same errors the user sees (TypeScript
// worker and rust-analyzer markers), not just what it can parse itself.
//
// One marker listener covers every owner: both the TS worker and the LSP
// client publish through monaco.editor.setModelMarkers, which fires
// onDidChangeMarkers. Reports are debounced and fire-and-forget; the
// backend caps, evicts and marks them stale on its side. A file's markers
// are reported under the folder the file is in: a workspace's folders are
// kept apart, and a project is the list of one (D14).
import * as monaco from 'monaco-editor';
import { invoke } from '@tauri-apps/api/core';
import { typesScopeCovers } from './intellisense';
import { innermostFolder } from './openFolders';

let folders: readonly string[] = [];
let registered = false;
let timer: ReturnType<typeof setTimeout> | undefined;
const pending = new Set<string>();
/**
 * What was last sent per URI. onDidChangeMarkers fires per owner, so the
 * same file can come up several times in a row with nothing new: those are
 * skipped. Any real change is sent, however soon after the last one (a
 * time window here would drop the update that clears a fixed error).
 */
const lastSent = new Map<string, string>();
const DEBOUNCE_MS = 400;
const MAX_PER_FLUSH = 100;

function fsPathOf(uri: monaco.Uri): string {
  return (uri as unknown as { fsPath?: string }).fsPath ?? uri.path;
}

/** Forward slashes, and a lower-case drive letter (Windows paths). */
function normalizePath(p: string): string {
  const slashed = p.replace(/\\/g, '/');
  return /^[A-Za-z]:\//.test(slashed) ? slashed[0].toLowerCase() + slashed.slice(1) : slashed;
}

/** The folder whose diagnostics a file's markers are (its own), or null. */
function rootOf(abs: string): string | null {
  return innermostFolder(folders, normalizePath(abs));
}

interface Marker {
  startLine: number;
  startCol: number;
  endLine: number;
  endCol: number;
  severity: string;
  message: string;
  source?: string;
  code?: string;
}

function markersOf(uri: monaco.Uri): Marker[] {
  return monaco.editor.getModelMarkers({ resource: uri }).map((m) => {
    const code =
      m.code === undefined || m.code === null
        ? undefined
        : typeof m.code === 'object'
          ? String((m.code as { value?: unknown }).value ?? '')
          : String(m.code);
    return {
      startLine: m.startLineNumber,
      startCol: m.startColumn,
      endLine: m.endLineNumber,
      endCol: m.endColumn,
      severity: monaco.MarkerSeverity[m.severity]?.toLowerCase() ?? 'info',
      message: m.message,
      source: m.source,
      code: code || undefined,
    };
  });
}

async function push(uriStr: string): Promise<void> {
  if (folders.length === 0) return;
  const uri = monaco.Uri.parse(uriStr);
  if (uri.scheme !== 'file') return;
  const abs = fsPathOf(uri);
  const root = rootOf(abs);
  if (!root) return;
  // Outside the worker's program (a file of another config is active, and
  // the program is that config's): its markers are stale. They are
  // recomputed, and reported, once one of its own config's files is active.
  if (!typesScopeCovers(abs)) return;
  const markers = markersOf(uri);
  const key = `${root}\n${JSON.stringify(markers)}`;
  if (lastSent.get(uriStr) === key) return;
  lastSent.set(uriStr, key);
  try {
    await invoke('report_editor_diagnostics', { root, path: abs, markers });
  } catch {
    // The backend is gone (or the project closed): markers keep working.
    // Not sent, so a later identical report isn't skipped.
    lastSent.delete(uriStr);
  }
}

function schedule(): void {
  if (timer !== undefined) return;
  timer = setTimeout(() => {
    timer = undefined;
    // At most MAX_PER_FLUSH per flush; the rest wait for the next one.
    const batch = [...pending].slice(0, MAX_PER_FLUSH);
    for (const u of batch) pending.delete(u);
    void Promise.all(batch.map((u) => push(u)));
    if (pending.size > 0) schedule();
  }, DEBOUNCE_MS);
}

/**
 * Reports the window's editor markers to the backend, each file's under the
 * folder the file is in. Call from every Editor mount (like
 * ensureOutlineIntelliSense); the listener registers once, the folders follow
 * the window's. Existing markers under the new folders are pushed so the
 * agent sees them without waiting for an edit.
 */
export function ensureDiagnosticsReporting(projectDirs: readonly string[]): void {
  if (projectDirs.length > 0) folders = [...projectDirs];
  if (!registered) {
    registered = true;
    monaco.editor.onDidChangeMarkers((uris) => {
      for (const uri of uris) pending.add(uri.toString());
      schedule();
    });
  }
  if (folders.length === 0) return;
  for (const model of monaco.editor.getModels()) {
    if (model.uri.scheme !== 'file') continue;
    if (rootOf(fsPathOf(model.uri))) pending.add(model.uri.toString());
  }
  schedule();
}
