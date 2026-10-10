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
//     paths, jsx), with extends and references followed (lib/tsconfig.ts),
//     and JavaScript is understood too (allowJs);
//   - go-to-definition into another file opens it in a tab at that line
//     (Monaco can't open other files by itself: registerEditorOpener).
//
// Open tabs are Monaco models; for those, the worker reads the model (what
// the user is typing), not the copy loaded from disk. A file must never be
// both at once: the worker's program would list it twice (once as a synced
// mirror model, once as an extra lib), which upstream reports as breaking
// module resolution (microsoft/monaco-editor#754 — extra-lib sources fail
// while real models resolve). So project sources skip URIs that already
// have a live model, and opening a file drops its extra-lib copy.
import * as monaco from 'monaco-editor';
import { invoke } from '@tauri-apps/api/core';
import type { MonacoLibEntry } from '../types';
import { readDirEntries, readFileContent } from './commands';
import { resolveTsOptions, type ResolvedTsOptions } from './tsconfig';
import { requestOpenFile } from './editorBridge';

const ts = monaco.typescript.typescriptDefaults;
const js = monaco.typescript.javascriptDefaults;

/** The model URI for a file path; libraries are keyed the same way. */
export function modelUriFor(filePath: string): monaco.Uri {
  let normalized = filePath.replace(/\\/g, '/');
  while (normalized.startsWith('/')) normalized = normalized.substring(1);
  return monaco.Uri.parse(`file:///${normalized}`);
}

/**
 * Typings arrive keyed under a virtual `/node_modules` root
 * (`file:///node_modules/solid-js/...`), but models live at real absolute
 * paths (`file:///home/.../src/...`). TypeScript's Node resolution walks
 * `node_modules` folders up from the importing file, so a virtual root
 * never matches and every bare import (`solid-js`, `@tauri-apps/...`)
 * fails with TS2307. Re-anchor typings under the project's real
 * `node_modules` folder so the walk finds them; unknown shapes pass
 * through unchanged.
 *
 * The root prefix goes through `modelUriFor(...).toString()` so it is
 * encoded exactly like open models' URIs (spaces etc.) — TS derives probe
 * paths from the importing model's URI string, so the prefix must match.
 * The package-relative part stays RAW on purpose: TS builds probes from
 * the literal import text, while `Uri.toString()` percent-encodes `@` as
 * `%40` (monaco's `encodeTable`). Encoding it breaks every scoped package
 * (`@tanstack/*`, `@tauri-apps/*`, `@types/*`) with TS2307, because the
 * worker's exact-string `fileExists`/`readFile` lookup never matches.
 */
function libUriFor(root: string, name: string): string {
  const VIRTUAL_ROOT = 'file:///node_modules/';
  if (!name.startsWith(VIRTUAL_ROOT)) return name;
  const clean = root.replace(/[\\/]+$/, '');
  return modelUriFor(`${clean}/node_modules`).toString() + name.slice(VIRTUAL_ROOT.length - 1);
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
// Multi-file project: every open TS/JS model belongs to the worker's
// program (cross-file completions, go-to-definition), not just the model
// that triggered worker creation. Without this the worker syncs models
// lazily per request, which the multi-file guides (e.g. SO #57146485)
// call out as the thing that breaks IDE-style tab setups.
ts.setEagerModelSync(true);
js.setEagerModelSync(true);

// While typings / project sources are still loading, the worker resolves
// every bare import (and cross-file symbol) to nothing, so it reports
// TS2307 on each import line. Those squiggles are false positives that
// vanish once the libs arrive — suppress semantic diagnostics until the
// first load finishes, then turn them back on.
function setSemanticValidationEnabled(enabled: boolean): void {
  for (const defaults of [ts, js]) {
    try {
      defaults.setDiagnosticsOptions({
        ...defaults.getDiagnosticsOptions(),
        noSemanticValidation: !enabled,
      });
    } catch {
      // ignore: diagnostics stay as they were
    }
  }
}

// Start suppressed: the very first paint happens before any project libs
// have reached the worker.
setSemanticValidationEnabled(false);

// Roots whose libs have fully reached the worker. `loadedRoot` is set when
// a load *starts*; this is set when it *finishes* — the Editor overlay and
// the diagnostics gate both key off this. At most the loaded root: the
// worker holds one project's libs, so switching projects empties it.
const readyRoots = new Set<string>();

/** True when the project's definitions are fully loaded (or no project). */
export function isIntelliSenseReady(root: string | null): boolean {
  if (!root) return true;
  return readyRoots.has(root);
}

// Monaco stores extra libs in a null-prototype map (Object.create(null)) and
// forwards that same object to the TS worker via postMessage
// (workerManager _updateExtraLibs / createData). Null-prototype objects are a
// known DataCloneError suspect on some WebKit builds (Tauri's WebView):
// the failure surfaces as an unhandled rejection from webWorker.js with no
// method name. Hand the worker plain-prototype copies instead; internal
// storage is untouched, only what getExtraLibs() returns (and hence what is
// posted) changes. Verified in the log line below.
for (const [, defaults] of [
  ['ts', ts],
  ['js', js],
] as const) {
  const origGet = defaults.getExtraLibs.bind(defaults);
  defaults.getExtraLibs = () => ({ ...origGet() });
}

let loadedRoot: string | null = null;
let loading: Promise<void> | null = null;

/**
 * URIs contributed by the last typings fetch (`load_nodejs_project_typings`,
 * re-anchored via libUriFor). Tracked so a typings-only refresh can swap
 * exactly those entries and leave project sources (and live models) alone.
 */
let typingsUris = new Set<string>();

/** Manifests whose change means node_modules may have changed. */
const TYPINGS_MANIFESTS = new Set([
  'package.json',
  'package-lock.json',
  'pnpm-lock.yaml',
  'yarn.lock',
  'bun.lock',
  'bun.lockb',
]);

/** True for a root-level manifest write (any operation). */
export function isTypingsManifest(path: string): boolean {
  const slash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  return TYPINGS_MANIFESTS.has(path.slice(slash + 1));
}

// Typings-only refresh state (see notifyTypingsMaybeStale below).
const TYPINGS_QUIET_MS = 2_500;
let typingsTimer: ReturnType<typeof setTimeout> | undefined;
let typingsInFlight = false;
let typingsRequeued = false;

/*
 * Everything handed to the TypeScript worker crosses a `postMessage`
 * structured-clone boundary (`setExtraLibs` → `updateExtraLibs`,
 * `setCompilerOptions` → worker createData). A single non-cloneable value
 * anywhere in the payload — `undefined`/`null` content from a failed read,
 * a non-string path, a tsconfig `paths` entry that isn't a string array —
 * makes the worker-side `postMessage` throw `DataCloneError: The object
 * can not be cloned`, which surfaces as an unhandled rejection from
 * `webWorker.js` with no hint at which payload was bad. So every payload
 * is coerced to plain `{ string: string }` data here, and every send is
 * wrapped so a failure logs what failed instead of rejecting unhandled.
 */
interface ExtraLibInput {
  filePath: unknown;
  content: unknown;
}

/** Plain `{ filePath: string, content: string }` entries, last one wins. */
export function sanitizeExtraLibs(libs: ExtraLibInput[]): { filePath: string; content: string }[] {
  const byPath = new Map<string, string>();
  for (const lib of libs) {
    if (typeof lib?.filePath !== 'string' || lib.filePath.length === 0) continue;
    const content = typeof lib.content === 'string' ? lib.content : (lib.content ?? '');
    byPath.set(lib.filePath, String(content));
  }
  return [...byPath].map(([filePath, content]) => ({ filePath, content }));
}

/** One sanitized send to both TS and JS defaults. */
function safeSetExtraLibs(libs: ExtraLibInput[]): void {
  let all: { filePath: string; content: string }[];
  try {
    all = sanitizeExtraLibs(libs);
  } catch {
    return;
  }
  for (const [, defaults] of [
    ['ts', ts],
    ['js', js],
  ] as const) {
    try {
      defaults.setExtraLibs(all);
    } catch {
      // ignore: a failed send leaves the previous libs in place
    }
  }
}

/** Loads the project into Monaco, once per project root. */
export function ensureProjectIntelliSense(root: string | null): Promise<void> {
  if (!root) return Promise.resolve();
  // Ready only while it's the loaded project: after A → B → A the worker
  // holds B's libs and compiler options, so A loads again.
  if (root === loadedRoot && readyRoots.has(root)) return Promise.resolve();
  if (root === loadedRoot && loading) return loading;
  // Another project replaces what the worker has: no root is ready until
  // its own load finishes.
  readyRoots.clear();
  // Changes queued for the previous project don't apply to this one.
  queued.clear();
  firstQueuedAt = null;
  clearTimeout(flushTimer);
  clearTimeout(reloadTimer);
  clearTimeout(typingsTimer);
  typingsInFlight = false;
  typingsRequeued = false;
  stale = false;
  loadedRoot = root;
  // A new project has no libs yet: suppress semantic errors until its load
  // finishes, and clear any stale markers from the previous project.
  setSemanticValidationEnabled(false);
  loading = load(root);
  return loading;
}

async function load(root: string) {
  try {
    const [typings, sources, options] = await Promise.all([
      invoke<MonacoLibEntry[]>('load_nodejs_project_typings', { projectRoot: root }).catch(() => {
        return [] as MonacoLibEntry[];
      }),
      invoke<{ files: { path: string; content: string }[]; truncated: boolean }>(
        'load_project_sources',
        { root }
      ).catch(() => {
        return { files: [], truncated: false };
      }),
      projectCompilerOptions(root),
    ]);
    if (root !== loadedRoot) return; // another project was opened meanwhile

    ts.setCompilerOptions(sanitizeCompilerOptions(options));
    js.setCompilerOptions(sanitizeCompilerOptions(options));
    // One call each: adding thousands of files one by one would resync the
    // worker every time. Typings (node_modules .d.ts) are never opened as
    // models, so they all go in; project sources skip URIs that already have
    // a live model — the worker serves those from the model itself.
    // package.json files must stay in the lib set: the TS worker resolves
    // bare imports (solid-js → types/index.d.ts via the "types" field) by
    // reading package.json through readFile/fileExists, which only see
    // extraLibs + open models. Dropping them caused TS2307 for every bare
    // import (verified via __rovikIntellisense worker.semantic).
    const have = liveModelUris();
    const typingLibs = sanitizeExtraLibs(
      typings.map((t) => ({ filePath: libUriFor(root, t.name), content: t.content }))
    );
    const libs = [
      ...typingLibs,
      ...sources.files
        .filter((f) => !have.has(modelUriFor(f.path).toString()))
        .map((f) => ({ filePath: modelUriFor(f.path).toString(), content: f.content })),
    ];
    safeSetExtraLibs(libs);
    typingsUris = new Set(typingLibs.map((l) => l.filePath));
  } finally {
    // This root still current and never marked ready (first load, success
    // or failure — the fetches above all have fallbacks, but even a throw
    // must not leave validation off and the overlay up forever): mark it
    // ready, clear stale markers from the half-loaded program so no false
    // TS2307 survives, and re-enable semantic diagnostics. Background
    // refreshes of an already-ready root skip this: clearing markers there
    // would erase real errors on every burst reload.
    if (root === loadedRoot && !readyRoots.has(root)) {
      readyRoots.add(root);
      for (const m of monaco.editor.getModels()) {
        try {
          monaco.editor.setModelMarkers(m, 'typescript', []);
          monaco.editor.setModelMarkers(m, 'javascript', []);
        } catch {
          // ignore
        }
      }
      setSemanticValidationEnabled(true);
    }
  }
}

/*
 * A dependency install writes root manifests (package.json, lockfiles) and
 * then settles node_modules — but node_modules itself is watcher-ignored,
 * so no fs event marks the typings stale. The manifest write is the signal:
 * debounce past it (install writes manifests several times, before
 * node_modules settles), then re-fetch typings ONLY and merge them into the
 * existing extra-libs, leaving project sources untouched.
 *
 * Deliberately NOT a full load(): no overlay, no diagnostic toggle, no
 * marker wipe — the old libs stay valid throughout and the worker
 * revalidates incrementally, so at most the new import shows a (true)
 * TS2307 until the refresh lands. A backend failure keeps the old libs
 * (never wipes to empty). A project switch drops the pending refresh via
 * the root guard. A pending source flush/reload covers typings anyway via
 * its own full load(), so the refresh yields to it.
 */

/** A manifest changed on disk: schedule a typings-only refresh (no-op otherwise). */
export function notifyTypingsMaybeStale(path: string): void {
  const root = loadedRoot;
  if (!root || !isTypingsManifest(path) || !path.startsWith(root)) return;
  // The initial load hasn't finished: it reads node_modules itself, so it
  // already picks up whatever the install wrote.
  if (!readyRoots.has(root)) return;
  clearTimeout(typingsTimer);
  typingsTimer = setTimeout(() => void runTypingsRefresh(root), TYPINGS_QUIET_MS);
}

async function runTypingsRefresh(root: string): Promise<void> {
  if (typingsInFlight) {
    typingsRequeued = true;
    return;
  }
  // A source flush/reload is pending or running: its full load() re-fetches
  // typings too, so a separate refresh would only race it (both do a
  // read-modify-write over the same extra-libs map).
  if (stale || queued.size > 0) return;
  typingsInFlight = true;
  try {
    let typings: MonacoLibEntry[];
    try {
      typings = await invoke<MonacoLibEntry[]>('load_nodejs_project_typings', {
        projectRoot: root,
      });
    } catch {
      return; // keep the old libs
    }
    if (root !== loadedRoot) return;
    if (stale || queued.size > 0) return; // source activity started meanwhile
    const fresh = sanitizeExtraLibs(
      typings.map((t) => ({ filePath: libUriFor(root, t.name), content: t.content }))
    );
    // Typings are never opened as models, but guard the passthrough shape
    // (libUriFor returning a non-node_modules URI): the worker serves live
    // models itself, so never add an extra-lib copy of one.
    const have = liveModelUris();
    const addable = fresh.filter((l) => !have.has(l.filePath));
    const next = new Map(Object.entries(ts.getExtraLibs()).map(([uri, lib]) => [uri, lib.content]));
    for (const uri of typingsUris) next.delete(uri);
    for (const { filePath, content } of addable) next.set(filePath, content);
    typingsUris = new Set(addable.map((l) => l.filePath));
    safeSetExtraLibs([...next].map(([filePath, content]) => ({ filePath, content })));
  } finally {
    typingsInFlight = false;
    if (typingsRequeued) {
      typingsRequeued = false;
      if (root === loadedRoot) {
        clearTimeout(typingsTimer);
        typingsTimer = setTimeout(() => void runTypingsRefresh(root), TYPINGS_QUIET_MS);
      }
    }
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
  const next = new Map(Object.entries(libs).map(([uri, lib]) => [uri, lib.content]));
  for (const [uri, text] of changed) next.set(uri, text);
  // Never keep an extra-lib copy of a live model (see dropProjectSourceLib).
  // Checked even when nothing changed: an open file's entry is dropped even
  // if the disk copy is identical.
  let dropped = 0;
  for (const uri of liveModelUris()) {
    if (next.delete(uri)) dropped++;
  }
  if (changed.length === 0 && dropped === 0) return;
  const all = [...next].map(([filePath, content]) => ({ filePath, content }));
  safeSetExtraLibs(all);
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

/** URIs of live (non-disposed) editor models. */
function liveModelUris(): Set<string> {
  const out = new Set<string>();
  for (const m of monaco.editor.getModels()) {
    if (!m.isDisposed()) out.add(m.uri.toString());
  }
  return out;
}

/**
 * An opened file is served from its model from now on (models are cached,
 * never disposed): drop its extra-lib copy so the worker's program lists
 * the file once. No-op when it has no extra-lib entry, so re-mounts of an
 * already-open tab cost nothing.
 */
export function dropProjectSourceLib(filePath: string): void {
  const uri = modelUriFor(filePath).toString();
  const libs = ts.getExtraLibs();
  if (!(uri in libs)) return;
  const all = sanitizeExtraLibs(
    Object.entries(libs)
      .filter(([key]) => key !== uri)
      .map(([filePath, lib]) => ({ filePath, content: lib.content }))
  );
  safeSetExtraLibs(all);
}

/**
 * Project sources whose models are being disposed (idle-model clean-up,
 * see lib/models.ts): hand their text back to the worker as extra libs, as
 * `load` would have, so files that import them still resolve. A model is
 * its file's only copy in the worker (see dropProjectSourceLib). One
 * update for all of them: each update resyncs the worker.
 */
export function restoreProjectSourceLibs(models: monaco.editor.ITextModel[]): void {
  const root = loadedRoot;
  if (!root) return;
  const rootPath = modelUriFor(root).path.replace(/\/+$/, '');
  const libs = ts.getExtraLibs();
  const added = models
    .filter((m) => !m.isDisposed() && m.uri.scheme === 'file')
    .filter((m) => m.uri.path.startsWith(`${rootPath}/`) && isProjectSource(m.uri.path))
    .filter((m) => !(m.uri.toString() in libs))
    .map((m) => ({ filePath: m.uri.toString(), content: m.getValue() }));
  if (added.length === 0) return;
  safeSetExtraLibs(
    sanitizeExtraLibs([
      ...Object.entries(libs).map(([filePath, lib]) => ({ filePath, content: lib.content })),
      ...added,
    ])
  );
}

/* ---------------------------------------------------------------------------
 * Compiler options from tsconfig.json / jsconfig.json
 * ------------------------------------------------------------------------- */

const JSX: Record<string, monaco.typescript.JsxEmit> = {
  preserve: monaco.typescript.JsxEmit.Preserve,
  react: monaco.typescript.JsxEmit.React,
  'react-native': monaco.typescript.JsxEmit.ReactNative,
  'react-jsx': monaco.typescript.JsxEmit.ReactJSX,
  'react-jsxdev': monaco.typescript.JsxEmit.ReactJSXDev,
};

/**
 * Plain-JSON copy of compiler options for the worker handoff: drops any
 * non-string `baseUrl` / `jsxImportSource` and non-string-array `paths`
 * entries so the structured-clone `postMessage` can't throw DataCloneError.
 */
function sanitizeCompilerOptions(
  options: monaco.typescript.CompilerOptions
): monaco.typescript.CompilerOptions {
  const clean: monaco.typescript.CompilerOptions = { ...options };
  if (clean.baseUrl !== undefined && typeof clean.baseUrl !== 'string') delete clean.baseUrl;
  if (clean.paths !== undefined) {
    if (clean.paths && typeof clean.paths === 'object') {
      const paths: Record<string, string[]> = {};
      for (const [k, v] of Object.entries(clean.paths)) {
        if (Array.isArray(v)) {
          const strs = v.filter((e): e is string => typeof e === 'string');
          if (strs.length > 0) paths[k] = strs;
        }
      }
      clean.paths = paths;
    } else {
      delete clean.paths;
    }
  }
  if (clean.jsxImportSource !== undefined && typeof clean.jsxImportSource !== 'string')
    delete clean.jsxImportSource;
  return clean;
}

/**
 * The options the project's configs set that matter for navigation —
 * tsconfig.json with its `extends` and `references` followed, so a solution
 * config pointing at tsconfig.app.json / tsconfig.node.json works (see
 * lib/tsconfig.ts).
 */
async function projectCompilerOptions(root: string): Promise<monaco.typescript.CompilerOptions> {
  const options = { ...BASE_OPTIONS };
  const resolved = await resolveTsOptions(root, readFileContent, async (dir) =>
    (await readDirEntries(root, dir)).filter((e) => !e.is_dir).map((e) => e.name)
  ).catch(() => ({}) as ResolvedTsOptions);
  if (resolved.baseUrl) options.baseUrl = modelUriFor(resolved.baseUrl).toString();
  if (resolved.paths && Object.keys(resolved.paths).length > 0) {
    // Substitutions are absolute, so mappings from configs in different
    // folders (tsconfig.app.json in the root, packages/x/tsconfig.json...)
    // all hold under the one baseUrl the worker gets.
    const paths: Record<string, string[]> = {};
    for (const [key, subs] of Object.entries(resolved.paths)) paths[key] = subs.map(pathUriFor);
    options.paths = paths;
  }
  if (resolved.jsx && JSX[resolved.jsx] !== undefined) options.jsx = JSX[resolved.jsx];
  if (resolved.jsxImportSource) options.jsxImportSource = resolved.jsxImportSource;
  return options;
}

/**
 * A `paths` substitution as a file URI the worker matches against module
 * URIs. The folder part goes through modelUriFor (encoded like models); the
 * part from the `*` on stays raw, as libUriFor explains.
 */
function pathUriFor(path: string): string {
  const star = path.indexOf('*');
  if (star < 0) return modelUriFor(path).toString();
  const slash = path.lastIndexOf('/', star);
  return modelUriFor(path.slice(0, Math.max(slash, 0))).toString() + path.slice(Math.max(slash, 0));
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
    // Typings used to load under a virtual /node_modules root (the real files
    // are in the project's node_modules); current ones are keyed by absolute
    // path, so fsPath is already right. Both shapes handled.
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

/* ---------------------------------------------------------------------------
 * Diagnostics aid: run `window.__rovikIntellisense()` in DevTools when a
 * tab has no IntelliSense. It shows whether the project's libs reached the
 * worker (extraLibs), what models exist, and which carry TS markers —
 * i.e. whether the worker is validating them at all.
 * ------------------------------------------------------------------------- */

interface IntellisenseDiag {
  loadedRoot: string | null;
  extraLibs: number;
  extraLibSample: string[];
  /** Prototype of what getExtraLibs() returns (what is posted to the worker). */
  extraLibsProto: string;
  /** structuredClone() of a 3-entry sample: proves cloneability without cloning 30MB. */
  cloneProbe: string;
  models: { uri: string; language: string; attached: boolean; disposed: boolean }[];
  markers: { uri: string; owner: string; count: number; codes: string[] }[];
  /** Direct answers from the TS worker for the attached (visible) model. */
  worker?: {
    uri: string;
    syntactic: string[];
    semantic: string[];
    error?: string;
  };
}

async function intellisenseDiag(): Promise<IntellisenseDiag> {
  const libs = ts.getExtraLibs();
  const keys = Object.keys(libs);
  const models = monaco.editor.getModels().map((m) => ({
    uri: m.uri.toString(),
    language: m.getLanguageId(),
    attached: m.isAttachedToEditor(),
    disposed: m.isDisposed(),
  }));
  const markers = models.map((m) => {
    const all = monaco.editor.getModelMarkers({ resource: monaco.Uri.parse(m.uri) });
    return {
      uri: m.uri,
      owner: all[0]?.owner ?? '(none)',
      count: all.length,
      codes: [
        ...new Set(all.map((x) => String(typeof x.code === 'object' ? x.code?.value : x.code))),
      ].slice(0, 10),
    };
  });
  const diag: IntellisenseDiag = {
    loadedRoot,
    extraLibs: keys.length,
    extraLibSample: keys.slice(0, 5),
    extraLibsProto:
      Object.getPrototypeOf(libs) === Object.prototype
        ? 'plain'
        : Object.getPrototypeOf(libs) === null
          ? 'null'
          : 'other',
    cloneProbe: (() => {
      try {
        const sample: Record<string, unknown> = {};
        for (const k of keys.slice(0, 3)) sample[k] = libs[k];
        structuredClone(sample);
        return 'cloneable';
      } catch (err) {
        return `NOT-CLONEABLE: ${err instanceof Error ? `${err.name}: ${err.message}` : String(err)}`;
      }
    })(),
    models,
    markers,
  };
  // Ask the worker itself about the visible file: this is what completions /
  // hover / go-to-definition see, bypassing markers and providers.
  const attached = monaco.editor.getModels().find((m) => m.isAttachedToEditor());
  const lang = attached?.getLanguageId();
  if (attached && (lang === 'typescript' || lang === 'javascript')) {
    const uri = attached.uri.toString();
    try {
      const getter =
        lang === 'typescript'
          ? await monaco.typescript.getTypeScriptWorker()
          : await monaco.typescript.getJavaScriptWorker();
      const proxy = await getter(attached.uri);
      const [syn, sem] = await Promise.all([
        proxy.getSyntacticDiagnostics(uri),
        proxy.getSemanticDiagnostics(uri),
      ]);
      diag.worker = {
        uri,
        syntactic: syn.map((d) => String(d.code)).slice(0, 10),
        semantic: sem
          .map((d) => `${d.code}: ${flattenDiagnosticMessageText(d.messageText)}`)
          .slice(0, 10),
      };
    } catch (err) {
      diag.worker = { uri, syntactic: [], semantic: [], error: String(err) };
    }
  }
  return diag;
}

function flattenDiagnosticMessageText(msg: unknown): string {
  if (typeof msg === 'string') return msg;
  if (msg && typeof msg === 'object' && 'messageText' in msg)
    return flattenDiagnosticMessageText((msg as { messageText: unknown }).messageText);
  return '';
}

declare global {
  interface Window {
    __rovikIntellisense?: () => Promise<IntellisenseDiag>;
  }
}
window.__rovikIntellisense = intellisenseDiag;
