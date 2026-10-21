// models.ts
//
// The editor's Monaco models: kept to what their files are, and cleaned up
// once idle.
//
// A model is what the TypeScript worker and the language servers read for
// its file, in place of the copy on disk (lib/intellisense). So each holds
// its file as the app has it:
//
//   - an open tab's model, its tab's text. The shown tab's editor keeps its
//     own in step (Editor.tsx); a tab in the background has no editor, and
//     its model kept the text it had when last shown: a change to its file
//     (on disk, the agent's) reached the worker only once the tab was shown
//     again, and imports of what it now exports were errors meanwhile;
//   - a model no tab holds (a closed tab's, go to definition's), the file on
//     disk: it follows the file's changes, and goes with the file. A tab
//     closed without saving gives its model the file's text back.
//
// The text goes in as one edit that can be undone (replaceKeepingUndo),
// so the undo history stays with the model, and where the editor was
// (scroll, cursor) stays with the path (lib/viewStates.ts).
//
// Idle models are cleaned up. Models are cached by URI and kept when a tab
// closes (Editor.tsx: disposing on close caused a "reopen -> empty editor"
// race), and go-to-definition creates models for files that were never
// opened (lsp/client.ts). Without a limit a long session keeps one model
// per file ever opened or peeked, each also open in rust-analyzer. A model
// is idle when no editor shows it and no tab holds it. Beyond
// MAX_IDLE_MODELS idle models, the least recently used are disposed.
// Reopening a file creates a fresh model, as for any file opened the first
// time.
import * as monaco from 'monaco-editor';
import { readFileUnlessGone } from './commands';
import { modelUriFor } from './intellisense/uris';

/** Idle models kept, at most (recently used first). */
export const MAX_IDLE_MODELS = 30;

/** URI → last use (a counter, so ties can't happen). */
const lastUsed = new Map<string, number>();
let clock = 0;
let registered = false;
let openTabUris: () => Set<string> = () => new Set();
let pruneTimer: ReturnType<typeof setTimeout> | undefined;

/**
 * Puts outside text (an agent's write, a revert, a reload from disk) into
 * the model as one undoable edit. `setValue` would also clear the undo
 * history, so Ctrl+Z could no longer reach what was typed before.
 */
export function replaceKeepingUndo(model: monaco.editor.ITextModel, text: string): void {
  model.pushStackElement();
  // Line endings come with the text, as setValue would take them.
  const crlf = text.includes('\r\n');
  if (model.getEOL() !== (crlf ? '\r\n' : '\n')) {
    model.pushEOL(crlf ? monaco.editor.EndOfLineSequence.CRLF : monaco.editor.EndOfLineSequence.LF);
  }
  model.pushEditOperations([], [{ range: model.getFullModelRange(), text }], () => null);
  model.pushStackElement();
}

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

/** A model, as keeping it to its file sees it. */
export interface FileModel {
  /** Its URI, as open tabs are keyed (modelUriFor). */
  readonly key: string;
  /** Its file on disk. */
  readonly path: string;
  getValue(): string;
  isDisposed(): boolean;
  isAttachedToEditor(): boolean;
  /** Takes outside text as one undoable edit. */
  replace(text: string): void;
  dispose(): void;
}

/** The models of files: Monaco's (tests pass their own). */
export interface FileModels {
  keyOf(path: string): string;
  forPath(path: string): FileModel | undefined;
  all(): FileModel[];
}

function fileModel(model: monaco.editor.ITextModel): FileModel {
  return {
    key: model.uri.toString(),
    path: model.uri.fsPath,
    getValue: () => model.getValue(),
    isDisposed: () => model.isDisposed(),
    isAttachedToEditor: () => model.isAttachedToEditor(),
    replace: (text) => replaceKeepingUndo(model, text),
    dispose: () => model.dispose(),
  };
}

const monacoModels: FileModels = {
  keyOf: (path) => modelUriFor(path).toString(),
  forPath: (path) => {
    const model = monaco.editor.getModel(modelUriFor(path));
    return model ? fileModel(model) : undefined;
  },
  all: () =>
    monaco.editor
      .getModels()
      .filter((m) => m.uri.scheme === 'file')
      .map(fileModel),
};

/** The tab text each background tab's model was last given, by URI. */
const tabText = new Map<string, string>();

/**
 * Each open tab's model holds its tab's text, the tabs in the background
 * too. Call whenever the tabs change; a tab whose text didn't is skipped
 * without reading its model.
 */
export function syncTabModels(
  tabs: readonly { path: string; content: string }[],
  models: FileModels = monacoModels
): void {
  const open = new Set<string>();
  for (const { path, content } of tabs) {
    const key = models.keyOf(path);
    open.add(key);
    if (tabText.get(key) === content) continue;
    const model = models.forPath(path);
    // None yet (its editor makes it), or shown: its editor keeps it.
    if (!model || model.isDisposed() || model.isAttachedToEditor()) {
      tabText.delete(key);
      continue;
    }
    if (model.getValue() !== content) model.replace(content);
    tabText.set(key, content);
  }
  for (const key of tabText.keys()) if (!open.has(key)) tabText.delete(key);
}

/**
 * Something changed on disk at `path` (a file, or a folder and what's in
 * it): the models no tab holds take their files' text again, and those
 * whose file is gone are disposed, so the worker and the language servers
 * read what's there now. Also for a tab just closed, whose model may hold
 * edits that weren't saved.
 */
export async function followDisk(
  path: string,
  {
    models = monacoModels,
    read = readFileUnlessGone,
    openTabs = () => openTabUris(),
  }: {
    models?: FileModels;
    /** A file's text; null when it's gone, undefined when it can't be read. */
    read?: (path: string) => Promise<string | null | undefined>;
    /** The URIs open tabs hold. */
    openTabs?: () => Set<string>;
  } = {}
): Promise<void> {
  const at = models.keyOf(path).replace(/\/+$/, '');
  const open = openTabs();
  const idle = models
    .all()
    .filter(
      (m) => !m.isDisposed() && !open.has(m.key) && (m.key === at || m.key.startsWith(`${at}/`))
    );
  await Promise.all(
    idle.map(async (model) => {
      const text = await read(model.path);
      // Opened in a tab meanwhile: the tab has it now.
      if (model.isDisposed() || openTabs().has(model.key)) return;
      if (text === null) model.dispose();
      else if (text !== undefined && model.getValue() !== text) model.replace(text);
    })
  );
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
