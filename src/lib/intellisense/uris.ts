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
 * from the model's URI string plus the literal import text. So the part up
 * to the first `node_modules` is encoded exactly like models' URIs (spaces
 * and the like), while the rest stays RAW: `Uri.toString()` would encode
 * `@` as `%40`, and the worker's exact-string lookup then never matches a
 * scoped package (`@types/*`, `@tauri-apps/*`): TS2307 on every import.
 * A file outside `node_modules` is keyed as a model would be.
 */
export function dependencyUriFor(filePath: string): string {
  const file = forwardSlashes(filePath);
  const inside = file.indexOf('/node_modules/');
  if (inside < 0) return modelUriFor(file).toString();
  const folder = inside + '/node_modules'.length;
  return modelUriFor(file.slice(0, folder)).toString() + file.slice(folder);
}

/** The URI of a file's folder, encoded like a model's. */
export function folderUriFor(filePath: string): string {
  const file = forwardSlashes(filePath);
  return modelUriFor(file.slice(0, Math.max(file.lastIndexOf('/'), 0))).toString();
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
