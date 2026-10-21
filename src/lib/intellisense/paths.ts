// paths.ts
//
// Path rules shared by the main thread and the TypeScript worker
// (worker.ts). The worker can't import monaco-editor, so nothing here may.

/** JavaScript/TypeScript source files. */
export const SOURCE_FILE = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/i;

/** Folders the backend never looks into (typings/mod.rs). */
const SKIPPED_DIR =
  /[\\/](node_modules|\.git|dist|build|out|target|\.next|\.svelte-kit|coverage)[\\/]/;

/**
 * A file of the project's sources: one the worker's program is made of.
 * With the project's `root`, only the folders below it count, as for the
 * backend's walk: a project that is itself in a folder named `build` or
 * `out` has sources all the same, which its changes never reached.
 */
export function isProjectSource(path: string, root?: string | null): boolean {
  return SOURCE_FILE.test(path) && !SKIPPED_DIR.test(belowRoot(path, root));
}

/** The part of `path` below `root` (from its separator on), or all of it. */
function belowRoot(path: string, root: string | null | undefined): string {
  if (!root) return path;
  const file = path.replace(/\\/g, '/');
  const base = root.replace(/\\/g, '/').replace(/\/+$/, '');
  return file.startsWith(`${base}/`) ? file.slice(base.length) : path;
}

/**
 * A dependency's file (under a `node_modules` folder): the worker resolves
 * it when something imports it, but it is never a file of the program.
 */
export function isDependencyPath(pathOrUri: string): boolean {
  return /[\\/]node_modules[\\/]/.test(pathOrUri);
}

/**
 * The lib that carries the dependency folders' links (project.ts to
 * projectWorker.ts): link URI → target URI, as JSON. Never a file of the
 * program, and swapped together with the typings.
 */
export const LINKS_LIB = 'rovik://links';

/**
 * The lib that tells the worker which config's files make up its program
 * (activeConfig.ts to projectWorker.ts): the active file's config as a
 * `TsProject` (lib/tsconfig.ts), as JSON, or `null` for every source.
 */
export const PROJECT_LIB = 'rovik://project';

/** A lib that comes and goes with the dependencies' typings. */
export function isTypingsLib(uri: string): boolean {
  return uri === LINKS_LIB || isDependencyPath(uri);
}

/** A lib loaded from the project (a source, a typing, the links): not PROJECT_LIB. */
export function isLoadedLib(uri: string): boolean {
  return uri !== PROJECT_LIB;
}

/** A lib that comes and goes with the project's sources. */
export function isSourceLib(uri: string): boolean {
  return isLoadedLib(uri) && !isTypingsLib(uri);
}

/** Manifests whose change means node_modules may have changed. */
const TYPINGS_MANIFESTS = new Set([
  'package.json',
  'package-lock.json',
  'pnpm-lock.yaml',
  'yarn.lock',
  'bun.lock',
  'bun.lockb',
]);

/** True for a manifest write (any operation). */
export function isTypingsManifest(path: string): boolean {
  return TYPINGS_MANIFESTS.has(baseName(path));
}

/** tsconfig.json, jsconfig.json and their tsconfig.*.json variants. */
export function isTsConfig(path: string): boolean {
  return /^(tsconfig|jsconfig)(\.[^\\/]*)?\.json$/i.test(baseName(path));
}

function baseName(path: string): string {
  return path.slice(Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\')) + 1);
}
