// intellisense/
//
// What Monaco's TypeScript service knows about the project. It only sees
// files it's given, and it used to get just the open tabs and node_modules
// typings: anything defined in a file that wasn't open couldn't be found,
// so Ctrl+click on a class from another file did nothing. Now:
//
//   - the project's own JS/TS sources are loaded once per project
//     (src-tauri/src/typings/sources.rs) and kept current from the file
//     watcher, in batches (sourceChanges.ts);
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

export { modelUriFor } from './uris';
export { isProjectSource } from './paths';
import { SOURCE_FILE } from './paths';

initLanguageDefaults();

let current: IntelliSenseProject | null = null;
/** The file shown in the editor, kept across projects. */
let activeFile: string | null = null;

/** Loads the project into Monaco, once per project root. */
export function ensureProjectIntelliSense(root: string | null): Promise<void> {
  if (!root) return Promise.resolve();
  // The worker holds one project: after A → B → A, A loads again.
  if (current?.root !== root) {
    current?.dispose();
    current = new IntelliSenseProject(root, activeFile);
  }
  return current.loaded;
}

/** True when the project's definitions are fully loaded (or no project). */
export function isIntelliSenseReady(root: string | null): boolean {
  return !root || (current?.root === root && current.ready);
}

/**
 * A source file changed on disk: `content` if known, null when deleted,
 * undefined to read it. Applied with others once changes settle.
 */
export function updateProjectSource(path: string, content?: string | null): void {
  current?.sourceChanged(path, content);
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

registerDefinitionOpener(() => current?.root ?? null);
installIntelliSenseDevtools(() => ({
  root: current?.root ?? null,
  ready: current?.ready ?? false,
  compilerOptions: primaryDefaults.getCompilerOptions(),
}));
