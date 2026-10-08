// intellisense.ts
//
// What Monaco's TypeScript service knows about the project. It only sees
// files it's given, and it used to get just the open tabs and node_modules
// typings: anything defined in a file that wasn't open couldn't be found,
// so Ctrl+click on a class from another file did nothing. Now:
//
//   - the project's own JS/TS sources are loaded once per project
//     (src-tauri/src/typings/sources.rs) and kept current from the file
//     watcher (updateProjectSource), in batches: every change to the set
//     resends all of it to the worker, so a dev server rewriting dozens of
//     files froze the app when each was its own update;
//   - compiler options follow the project's tsconfig/jsconfig (baseUrl,
//     paths, jsx), and JavaScript is understood too (allowJs);
//   - go-to-definition into another file opens it in a tab at that line
//     (Monaco can't open other files by itself: registerEditorOpener).
//
// Open tabs are Monaco models; for those, the worker reads the model (what
// the user is typing), not the copy loaded from disk.
import * as monaco from 'monaco-editor';
import { invoke } from '@tauri-apps/api/core';
import type { MonacoLibEntry } from '../types';
import { readFileContent } from './commands';
import { requestOpenFile } from './editorBridge';

const ts = monaco.typescript.typescriptDefaults;
const js = monaco.typescript.javascriptDefaults;

/** The model URI for a file path; libraries are keyed the same way. */
export function modelUriFor(filePath: string): monaco.Uri {
  let normalized = filePath.replace(/\\/g, '/');
  while (normalized.startsWith('/')) normalized = normalized.substring(1);
  return monaco.Uri.parse(`file:///${normalized}`);
}

const SOURCE_FILE = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/i;
/** Same folders the backend skips. */
const SKIPPED_DIR =
  /[\\/](node_modules|\.git|dist|build|out|target|\.next|\.svelte-kit|coverage)[\\/]/;

const BASE_OPTIONS: monaco.typescript.CompilerOptions = {
  target: monaco.typescript.ScriptTarget.ESNext,
  module: monaco.typescript.ModuleKind.ESNext,
  moduleResolution: monaco.typescript.ModuleResolutionKind.NodeJs,
  jsx: monaco.typescript.JsxEmit.Preserve,
  allowJs: true,
  checkJs: false,
  allowNonTsExtensions: true,
  esModuleInterop: true,
  allowSyntheticDefaultImports: true,
};
ts.setCompilerOptions(BASE_OPTIONS);
js.setCompilerOptions(BASE_OPTIONS);

let loadedRoot: string | null = null;
let loading: Promise<void> | null = null;

/** Loads the project into Monaco, once per project root. */
export function ensureProjectIntelliSense(root: string | null): Promise<void> {
  if (!root) return Promise.resolve();
  if (root === loadedRoot && loading) return loading;
  // Changes queued for the previous project don't apply to this one.
  queued.clear();
  firstQueuedAt = null;
  clearTimeout(flushTimer);
  clearTimeout(reloadTimer);
  stale = false;
  loadedRoot = root;
  loading = load(root);
  return loading;
}

async function load(root: string) {
  const [typings, sources, options] = await Promise.all([
    invoke<MonacoLibEntry[]>('load_nodejs_project_typings', { projectRoot: root }).catch((err) => {
      console.error('Failed to load types:', err);
      return [] as MonacoLibEntry[];
    }),
    invoke<{ files: { path: string; content: string }[]; truncated: boolean }>(
      'load_project_sources',
      { root }
    ).catch((err) => {
      console.error('Failed to load project sources:', err);
      return { files: [], truncated: false };
    }),
    projectCompilerOptions(root),
  ]);
  if (root !== loadedRoot) return; // another project was opened meanwhile

  ts.setCompilerOptions(options);
  js.setCompilerOptions(options);
  // One call each: adding thousands of files one by one would resync the
  // worker every time.
  const libs = [
    ...typings.map((t) => ({ filePath: t.name, content: t.content })),
    ...sources.files.map((f) => ({ filePath: modelUriFor(f.path).toString(), content: f.content })),
  ];
  ts.setExtraLibs(libs);
  js.setExtraLibs(libs);
  if (sources.truncated) {
    console.warn('Project too large: not every source file was loaded for IntelliSense.');
  }
}

/*
 * Changes from disk, applied together: once QUIET_MS pass without another
 * (or MAX_WAIT_MS after the first, while they keep coming), in one update.
 * A burst of more than BURST_LIMIT files (a build, a branch switch) isn't
 * read file by file: the sources are loaded again once RELOAD_QUIET_MS pass
 * without changes.
 */
const QUIET_MS = 500;
const MAX_WAIT_MS = 3_000;
const BURST_LIMIT = 20;
const RELOAD_QUIET_MS = 5_000;

/** Path → content (null: deleted; undefined: read it when applying). */
const queued = new Map<string, string | null | undefined>();
let firstQueuedAt: number | null = null;
let flushTimer: ReturnType<typeof setTimeout> | undefined;
/** A burst was skipped: changes wait for the reload. */
let stale = false;
let reloadTimer: ReturnType<typeof setTimeout> | undefined;

/**
 * A source file changed on disk: `content` if known, null when deleted,
 * undefined to read it. Applied with others (see above).
 */
export function updateProjectSource(path: string, content?: string | null) {
  const root = loadedRoot;
  if (!root || !isProjectSource(path) || !path.startsWith(root)) return;
  if (stale) {
    scheduleReload();
    return;
  }
  queued.set(path, content);
  firstQueuedAt ??= Date.now();
  clearTimeout(flushTimer);
  const wait = Math.max(0, Math.min(QUIET_MS, firstQueuedAt + MAX_WAIT_MS - Date.now()));
  flushTimer = setTimeout(() => void flush(), wait);
}

async function flush() {
  const root = loadedRoot;
  const changes = [...queued];
  queued.clear();
  firstQueuedAt = null;
  if (!root || changes.length === 0) return;
  if (changes.length > BURST_LIMIT) {
    stale = true;
    scheduleReload();
    return;
  }
  const read = await Promise.all(
    changes.map(async ([path, content]) => {
      const text = content !== undefined ? content : await readFileContent(path).catch(() => null);
      // A deleted file stays known but empty: an empty file defines nothing.
      return [modelUriFor(path).toString(), text ?? ''] as const;
    })
  );
  if (root !== loadedRoot || stale) return;
  const libs = ts.getExtraLibs();
  const changed = read.filter(([uri, text]) => (libs[uri]?.content ?? '') !== text);
  if (changed.length === 0) return;
  const next = new Map(Object.entries(libs).map(([uri, lib]) => [uri, lib.content]));
  for (const [uri, text] of changed) next.set(uri, text);
  const all = [...next].map(([filePath, content]) => ({ filePath, content }));
  ts.setExtraLibs(all);
  js.setExtraLibs(all);
}

/** Loads the project's sources again once changes stop. */
function scheduleReload() {
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(() => {
    stale = false;
    const root = loadedRoot;
    if (root) loading = load(root);
  }, RELOAD_QUIET_MS);
}

export function isProjectSource(path: string): boolean {
  return SOURCE_FILE.test(path) && !SKIPPED_DIR.test(path);
}

/* ---------------------------------------------------------------------------
 * tsconfig.json / jsconfig.json
 * ------------------------------------------------------------------------- */

const JSX: Record<string, monaco.typescript.JsxEmit> = {
  preserve: monaco.typescript.JsxEmit.Preserve,
  react: monaco.typescript.JsxEmit.React,
  'react-native': monaco.typescript.JsxEmit.ReactNative,
  'react-jsx': monaco.typescript.JsxEmit.ReactJSX,
  'react-jsxdev': monaco.typescript.JsxEmit.ReactJSXDev,
};

/** The options the project's config sets that matter for navigation. */
async function projectCompilerOptions(root: string): Promise<monaco.typescript.CompilerOptions> {
  const options = { ...BASE_OPTIONS };
  const config = await readConfig(root);
  const co = (config?.compilerOptions ?? {}) as Record<string, unknown>;
  const join = (rel: string) => {
    const clean = rel.replace(/^\.\/?/, '').replace(/[\\/]+$/, '');
    return clean ? `${root.replace(/[\\/]+$/, '')}/${clean}` : root;
  };
  if (typeof co.baseUrl === 'string') {
    options.baseUrl = modelUriFor(join(co.baseUrl)).toString();
  }
  if (co.paths && typeof co.paths === 'object') {
    options.paths = co.paths as Record<string, string[]>;
    // Since TS 4.1, paths resolve from the config's folder without a baseUrl.
    options.baseUrl ??= modelUriFor(root).toString();
  }
  if (typeof co.jsx === 'string' && JSX[co.jsx.toLowerCase()] !== undefined) {
    options.jsx = JSX[co.jsx.toLowerCase()];
  }
  if (typeof co.jsxImportSource === 'string') options.jsxImportSource = co.jsxImportSource;
  return options;
}

async function readConfig(root: string): Promise<Record<string, unknown> | null> {
  for (const name of ['tsconfig.json', 'jsconfig.json']) {
    try {
      const text = await readFileContent(`${root.replace(/[\\/]+$/, '')}/${name}`);
      return JSON.parse(jsonc(text));
    } catch {
      // missing or unreadable: try the next
    }
  }
  return null;
}

/** tsconfig files allow comments and trailing commas; JSON.parse doesn't. */
export function jsonc(text: string): string {
  let out = '';
  let inString = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inString) {
      out += ch;
      if (ch === '\\') out += text[++i] ?? '';
      else if (ch === '"') inString = false;
    } else if (ch === '"') {
      inString = true;
      out += ch;
    } else if (ch === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i++;
      out += '\n';
    } else if (ch === '/' && text[i + 1] === '*') {
      i += 2;
      while (i < text.length && !(text[i] === '*' && text[i + 1] === '/')) i++;
      i++;
    } else {
      out += ch;
    }
  }
  return out.replace(/,(\s*[}\]])/g, '$1');
}

/* ---------------------------------------------------------------------------
 * Go to definition in another file
 * ------------------------------------------------------------------------- */

monaco.editor.registerEditorOpener({
  openCodeEditor(source, resource, selectionOrPosition) {
    const root = loadedRoot;
    if (resource.scheme !== 'file' || !root) return false;
    // In the same file Monaco moves the cursor itself (and keeps its
    // back/forward history): only other files need opening.
    if (source.getModel()?.uri.toString() === resource.toString()) return false;
    // Typings are loaded under a virtual /node_modules root: the real files
    // are in the project's node_modules.
    const path = resource.path.startsWith('/node_modules/')
      ? `${root.replace(/[\\/]+$/, '')}${resource.path}`
      : resource.fsPath;
    const line = !selectionOrPosition
      ? 1
      : 'startLineNumber' in selectionOrPosition
        ? selectionOrPosition.startLineNumber
        : selectionOrPosition.lineNumber;
    requestOpenFile({ path, line });
    return true;
  },
});
