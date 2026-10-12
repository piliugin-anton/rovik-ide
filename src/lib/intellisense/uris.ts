// uris.ts
//
// How files are keyed in the TypeScript worker. Open tabs are Monaco
// models, keyed by `modelUriFor`; every other file reaches the worker as
// an extra lib under the same kind of key, so a resolved import matches.
import * as monaco from 'monaco-editor';

/** The model URI for a file path; libraries are keyed the same way. */
export function modelUriFor(filePath: string): monaco.Uri {
  let normalized = filePath.replace(/\\/g, '/');
  while (normalized.startsWith('/')) normalized = normalized.substring(1);
  return monaco.Uri.parse(`file:///${normalized}`);
}

/**
 * The key of a dependency file. TypeScript's Node resolution walks
 * `node_modules` folders up from the importing model, building each probe
 * from the model's URI string plus the literal import text. So the folder
 * part is encoded exactly like models' URIs (spaces and the like), while
 * the package-relative part stays RAW: `Uri.toString()` would encode `@`
 * as `%40`, and the worker's exact-string lookup then never matches a
 * scoped package (`@types/*`, `@tauri-apps/*`): TS2307 on every import.
 * A file outside `node_modules` is keyed as a model would be.
 */
export function dependencyUriFor(root: string, filePath: string): string {
  const nodeModules = `${nodeModulesDirOf(root)}/`;
  const file = forwardSlashes(filePath);
  if (!file.startsWith(nodeModules)) return modelUriFor(file).toString();
  return `${nodeModulesUriFor(root)}/${file.slice(nodeModules.length)}`;
}

/** The project's node_modules folder as dependency keys start (no trailing slash). */
export function nodeModulesUriFor(root: string): string {
  return modelUriFor(nodeModulesDirOf(root)).toString();
}

function nodeModulesDirOf(root: string): string {
  return `${forwardSlashes(root).replace(/\/+$/, '')}/node_modules`;
}

/**
 * A `paths` substitution as a file URI the worker matches against module
 * URIs: the folder part encoded like models, the part from the `*` on raw,
 * as dependencyUriFor explains.
 */
export function pathPatternUriFor(path: string): string {
  const star = path.indexOf('*');
  if (star < 0) return modelUriFor(path).toString();
  const slash = Math.max(path.lastIndexOf('/', star), 0);
  return modelUriFor(path.slice(0, slash)).toString() + path.slice(slash);
}

function forwardSlashes(path: string): string {
  return path.replace(/\\/g, '/');
}
