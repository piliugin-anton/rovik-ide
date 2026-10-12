// models.ts
//
// Idle Monaco models, cleaned up. Models are cached by URI and kept when a
// tab closes (Editor.tsx: disposing on close caused a "reopen -> empty
// editor" race), and go-to-definition creates models for files that were
// never opened (lsp/client.ts). Without a limit a long session keeps one
// model per file ever opened or peeked, each also open in rust-analyzer.
//
// A model is idle when no editor shows it and no tab holds it. Beyond
// MAX_IDLE_MODELS idle models, the least recently used are disposed.
// Tabs with unsaved changes can't be closed, so an idle model matches its
// file on disk, whose copy the TypeScript worker keeps (lib/intellisense).
// Reopening a file creates a fresh model, as for any file opened the first
// time.
import * as monaco from 'monaco-editor';

/** Idle models kept, at most (recently used first). */
export const MAX_IDLE_MODELS = 30;

/** URI → last use (a counter, so ties can't happen). */
const lastUsed = new Map<string, number>();
let clock = 0;
let registered = false;
let openTabUris: () => Set<string> = () => new Set();
let pruneTimer: ReturnType<typeof setTimeout> | undefined;

/** A model was shown or created: it's the most recently used. */
export function touchModel(uri: monaco.Uri): void {
  lastUsed.set(uri.toString(), ++clock);
}

/**
 * Starts tracking (once) and says which URIs open tabs hold. Call from
 * the app with a getter for the current tabs.
 */
export function trackModels(getOpenTabUris: () => Set<string>): void {
  openTabUris = getOpenTabUris;
  if (registered) return;
  registered = true;
  monaco.editor.onDidCreateModel((model) => touchModel(model.uri));
  monaco.editor.onWillDisposeModel((model) => lastUsed.delete(model.uri.toString()));
}

/**
 * Disposes idle models beyond the cap, soon: after the current update, so
 * an editor being torn down (a tab just closed) has let go of its model.
 */
export function scheduleModelPrune(): void {
  clearTimeout(pruneTimer);
  pruneTimer = setTimeout(pruneIdleModels, 0);
}

/** Idle models beyond the cap, least recently used first, disposed. */
export function pruneIdleModels(): number {
  const open = openTabUris();
  const idle = monaco.editor
    .getModels()
    .filter(
      (m) =>
        !m.isDisposed() &&
        m.uri.scheme === 'file' &&
        !m.isAttachedToEditor() &&
        !open.has(m.uri.toString())
    )
    .sort((a, b) => (lastUsed.get(b.uri.toString()) ?? 0) - (lastUsed.get(a.uri.toString()) ?? 0));
  const extra = idle.slice(MAX_IDLE_MODELS);
  if (extra.length === 0) return 0;
  for (const model of extra) model.dispose();
  return extra.length;
}
