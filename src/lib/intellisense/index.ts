// intellisense/
//
// What Monaco's TypeScript service knows about the project. It only sees
// files it's given, and it used to get just the open tabs and node_modules
// typings: anything defined in a file that wasn't open couldn't be found,
// so Ctrl+click on a class from another file did nothing. Now:
//
//   - the project's own JS/TS sources are loaded once per project
//     (src-tauri/src/typings/sources.rs) and kept current from the file
//     watcher, in batches (sourceChanges.ts): a file that's gone (removed,
//     renamed, its folder moved away) is dropped, and when the watcher
//     loses count they are all loaded again;
//   - its dependencies' typings are loaded with them, as the closure of
//     what the code imports (rovik_core::typings): the packages the
//     sources, configs and manifests name, from every node_modules folder
//     in the project, workspace members' included, and what those
//     packages' declarations import in turn, not every package installed.
//     An import typed later is looked up and its package added
//     (project.ts). They are files the worker may read but not files of
//     its program (worker.ts): only what the code imports is parsed;
//   - compiler options follow the project's tsconfig/jsconfig (baseUrl,
//     paths, jsx), with extends and references followed (lib/tsconfig.ts),
//     and JavaScript is understood too (allowJs); the global @types follow
//     the config owning the active file, whose files are the program's,
//     as tsserver (activeConfig.ts);
//   - go-to-definition into another file opens it in a tab at that line
//     (opener.ts).
//
// Open tabs are Monaco models: for those, the worker reads the model (what
// the user is typing), not the copy loaded from disk, which it lists once
// (worker.ts). Every lib goes through libs.ts, file by file, so the worker
// keeps what it parsed for files that didn't change.
import { initLanguageDefaults, primaryDefaults } from './defaults';
import { installIntelliSenseDevtools } from './devtools';
import { registerDefinitionOpener } from './opener';
import { IntelliSenseProject } from './project';
import { registerSemanticTokens } from './semanticTokens';

export { modelUriFor } from './uris';
export { isProjectSource } from './paths';
export { refreshSemanticTokens } from './semanticTokens';
import { SOURCE_FILE } from './paths';

initLanguageDefaults();
// Semantic colouring for TS/JS (semanticTokens.ts): the legend and the
// document provider; which themes turn it on is the editor's call.
registerSemanticTokens();

let current: IntelliSenseProject | null = null;
/** The folders the worker has loaded, as one key: another list is another project. */
let currentRoots: string | null = null;
/** The file shown in the editor, kept across projects. */
let activeFile: string | null = null;

/** The folders as one key: the same folders in the same order are one project (C06). */
const keyOf = (roots: readonly string[]): string => roots.join('\n');

/**
 * Loads the project into Monaco, once per set of folders: a project's one, or
 * a workspace's several, whose sources and configs are read as one project.
 */
export function ensureProjectIntelliSense(roots: readonly string[]): Promise<void> {
  if (roots.length === 0) return Promise.resolve();
  // The worker holds one project: after A → B → A, A loads again.
  if (currentRoots !== keyOf(roots) || !current) {
    current?.dispose();
    current = new IntelliSenseProject(roots, activeFile);
    currentRoots = keyOf(roots);
  }
  return current.loaded;
}

/** True when the project's definitions are fully loaded (or no folder is open). */
export function isIntelliSenseReady(roots: readonly string[]): boolean {
  return roots.length === 0 || (currentRoots === keyOf(roots) && current?.ready === true);
}

/**
 * A source file changed on disk: `content` if known, null when deleted,
 * undefined to read it (a file that's gone is dropped). Applied with
 * others once changes settle.
 */
export function updateProjectSource(path: string, content?: string | null): void {
  current?.sourceChanged(path, content);
}

/**
 * Something that isn't a source changed: when it's a folder that went
 * (removed, renamed away), the sources in it are checked and dropped.
 */
export function updateProjectPath(path: string): void {
  current?.pathChanged(path);
}

/**
 * Too much changed for the watcher to tell (its "re-check everything"):
 * the project's sources and typings are read again once it settles.
 */
export function updateWholeProject(): void {
  current?.everythingChanged();
}

/**
 * Any file was written: a manifest (package.json, a lockfile) means
 * node_modules may have changed, a tsconfig can move files between configs
 * or change their `types`. Both are followed up once writes settle.
 */
export function notifyProjectFileWritten(path: string): void {
  current?.fileWritten(path);
}

/** The file shown in the editor: its config's @types become the worker's. */
export function setActiveTypesFile(path: string | null): void {
  // Other files (Markdown, Rust...) don't use the worker: keep the scope.
  if (path !== null && !SOURCE_FILE.test(path)) return;
  activeFile = path;
  current?.setActiveFile(path);
}

/**
 * Whether the worker's current markers for `path` are computed with the
 * @types its own config allows (lib/diagnostics.ts).
 */
export function typesScopeCovers(path: string): boolean {
  return current?.typesScopeCovers(path) ?? true;
}

registerDefinitionOpener(() => current !== null);
installIntelliSenseDevtools(() => ({
  roots: current?.roots ?? [],
  ready: current?.ready ?? false,
  compilerOptions: primaryDefaults.getCompilerOptions(),
}));
