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

let root: string | null = null;
let registered = false;
let timer: ReturnType<typeof setTimeout> | undefined;
const pending = new Set<string>();
/** URIs flushed recently: onDidChangeMarkers fires per owner, so the same
 * file reports several times in a row — skip the echo. */
const recent = new Map<string, number>();
const ECHO_MS = 2_000;
const DEBOUNCE_MS = 400;
const MAX_PER_FLUSH = 100;

function fsPathOf(uri: monaco.Uri): string {
  return (uri as unknown as { fsPath?: string }).fsPath ?? uri.path;
}

function underRoot(abs: string): boolean {
  if (!root) return false;
  if (abs === root) return true;
  const prefix = root.endsWith('/') ? root : `${root}/`;
  return abs.startsWith(prefix);
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
  const now = Date.now();
  if ((recent.get(uriStr) ?? 0) + ECHO_MS > now) return;
  recent.set(uriStr, now);
  const uri = monaco.Uri.parse(uriStr);
  if (uri.scheme !== 'file') return;
  const abs = fsPathOf(uri);
  if (!underRoot(abs)) return;
  try {
    await invoke('report_editor_diagnostics', { root, path: abs, markers: markersOf(uri) });
  } catch {
    // The backend is gone (or the project closed): markers keep working.
  }
}

function schedule(): void {
  if (timer !== undefined) return;
  timer = setTimeout(() => {
    timer = undefined;
    const batch = [...pending].slice(0, MAX_PER_FLUSH);
    pending.clear();
    void Promise.all(batch.map((u) => push(u)));
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
