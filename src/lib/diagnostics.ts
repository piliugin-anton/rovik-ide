// diagnostics.ts
//
// Reports what the editor shows to the backend, so the agent's
// `diagnostics` tool sees the same errors the user sees (TypeScript
// worker and rust-analyzer markers), not just what it can parse itself.
//
// One marker listener covers every owner: both the TS worker and the LSP
// client publish through monaco.editor.setModelMarkers, which fires
// onDidChangeMarkers. Reports are debounced and fire-and-forget; the
// backend caps, evicts and marks them stale on its side.
import * as monaco from 'monaco-editor';
import { invoke } from '@tauri-apps/api/core';
import { typesScopeCovers } from './intellisense';

let root: string | null = null;
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

function underRoot(abs: string): boolean {
  if (!root) return false;
  const path = normalizePath(abs);
  const base = normalizePath(root).replace(/\/+$/, '');
  return path === base || path.startsWith(`${base}/`);
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
  if (!root) return;
  const uri = monaco.Uri.parse(uriStr);
  if (uri.scheme !== 'file') return;
  const abs = fsPathOf(uri);
  if (!underRoot(abs)) return;
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
 * Reports this project's editor markers to the backend. Call from every
 * Editor mount (like ensureOutlineIntelliSense); the listener registers
 * once, the root follows the open project. Existing markers under the new
 * root are pushed so the agent sees them without waiting for an edit.
 */
export function ensureDiagnosticsReporting(projectDir: string | null): void {
  if (projectDir) root = projectDir;
  if (!registered) {
    registered = true;
    monaco.editor.onDidChangeMarkers((uris) => {
      for (const uri of uris) pending.add(uri.toString());
      schedule();
    });
  }
  if (!root) return;
  for (const model of monaco.editor.getModels()) {
    if (model.uri.scheme !== 'file') continue;
    if (underRoot(fsPathOf(model.uri))) pending.add(model.uri.toString());
  }
  schedule();
}
