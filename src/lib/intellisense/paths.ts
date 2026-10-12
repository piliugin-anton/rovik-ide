// paths.ts
//
// Path rules shared by the main thread and the TypeScript worker
// (worker.ts). The worker can't import monaco-editor, so nothing here may.

/** JavaScript/TypeScript source files. */
export const SOURCE_FILE = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/i;

/** Folders the backend never loads sources from (typings/sources.rs). */
const SKIPPED_DIR =
  /[\\/](node_modules|\.git|dist|build|out|target|\.next|\.svelte-kit|coverage)[\\/]/;

/** A file of the project's sources: one the worker's program is made of. */
export function isProjectSource(path: string): boolean {
  return SOURCE_FILE.test(path) && !SKIPPED_DIR.test(path);
}

/**
 * A dependency's file (under a `node_modules` folder): the worker resolves
 * it when something imports it, but it is never a file of the program.
 */
export function isDependencyPath(pathOrUri: string): boolean {
  return /[\\/]node_modules[\\/]/.test(pathOrUri);
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
