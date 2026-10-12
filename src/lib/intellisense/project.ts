// project.ts
//
// One project's definitions in the TypeScript worker: loaded once, then
// kept current from the file watcher (App.tsx) through the facade
// (index.ts). A new project replaces the old one: the worker holds one
// project's libs and options.
import { loadProjectSources, loadProjectTypings, readFileContent } from '../commands';
import type { LoadedFiles } from '../../types';
import { readProjectConfig } from './compilerOptions';
import { clearTypeScriptMarkers, setSemanticValidationEnabled } from './defaults';
import { workerLibs, type LibEntry } from './libs';
import { isDependencyPath, isProjectSource, isTsConfig, isTypingsManifest } from './paths';
import { QuietRunner } from './quietRunner';
import { SourceChangeQueue, type SourceChangeSink } from './sourceChanges';
import { TypesScope } from './typesScope';
import { dependencyUriFor, modelUriFor } from './uris';

/*
 * A dependency install writes root manifests (package.json, lockfiles) and
 * then settles node_modules; node_modules itself is watcher-ignored, so the
 * manifest write is the signal. Debounced past it (an install writes
 * manifests several times, before node_modules settles), then the typings
 * are fetched again and swapped file by file, the sources untouched: the
 * old libs stay valid throughout, so at most a new import shows a (true)
 * TS2307 until the refresh lands. A failed fetch keeps the old libs.
 */
const TYPINGS_QUIET_MS = 2_500;
/** A tsconfig write: the configs are read again once writes settle. */
const CONFIG_QUIET_MS = 500;

const NOTHING: LoadedFiles = { files: [], truncated: false };

export class IntelliSenseProject {
  /** Resolves when the first load is done (success or not). */
  readonly loaded: Promise<void>;
  /** The definitions have fully reached the worker. */
  ready = false;
  private disposed = false;
  private readonly types: TypesScope;
  private readonly sources: SourceChangeQueue;
  private readonly typingsRefresh: QuietRunner;
  private readonly configReload: QuietRunner;

  constructor(
    readonly root: string,
    activeFile: string | null
  ) {
    this.types = new TypesScope(root);
    this.types.setActiveFile(activeFile);
    const sink: SourceChangeSink = {
      apply: (changes) => this.applySourceChanges(changes),
      reload: () => this.reloadSources(),
    };
    this.sources = new SourceChangeQueue(sink);
    this.typingsRefresh = new QuietRunner(() => this.refreshTypings(), TYPINGS_QUIET_MS);
    this.configReload = new QuietRunner(() => this.reloadConfig(), CONFIG_QUIET_MS);
    this.loaded = this.load();
  }

  /** Stops every pending update; the worker's libs are left for the next project. */
  dispose(): void {
    this.disposed = true;
    this.sources.dispose();
    this.typingsRefresh.dispose();
    this.configReload.dispose();
  }

  /** The file shown in the editor: its config's @types become the worker's. */
  setActiveFile(path: string | null): void {
    this.types.setActiveFile(path);
    if (this.ready) this.types.apply();
  }

  typesScopeCovers(path: string): boolean {
    return this.types.covers(path);
  }

  /** A source changed on disk: `content` if known, null when deleted, undefined to read it. */
  sourceChanged(path: string, content?: string | null): void {
    if (!this.owns(path) || !isProjectSource(path)) return;
    this.sources.queue(path, content);
  }

  /** Any file was written: a manifest or a tsconfig means more than one source. */
  fileWritten(path: string): void {
    // The initial load reads node_modules and the configs itself.
    if (!this.owns(path) || !this.ready) return;
    if (isTypingsManifest(path)) this.typingsRefresh.trigger();
    if (isTsConfig(path)) this.configReload.trigger();
  }

  private owns(path: string): boolean {
    return path.startsWith(this.root);
  }

  private async load(): Promise<void> {
    // A new project has no libs yet: no semantic errors until it is loaded.
    setSemanticValidationEnabled(false);
    try {
      const [typings, sources, config] = await Promise.all([
        loadProjectTypings(this.root).catch(() => NOTHING),
        loadProjectSources(this.root).catch(() => NOTHING),
        readProjectConfig(this.root),
      ]);
      if (this.disposed) return;
      this.warnIfTruncated('typings', typings);
      this.warnIfTruncated('sources', sources);
      const typingLibs = this.typingLibs(typings);
      // Options first: a change restarts the worker, which then starts
      // with the complete lib set instead of receiving it twice.
      this.types.configure(config);
      this.types.setInstalled(typingLibs.map((lib) => lib.filePath));
      this.types.apply();
      workerLibs.replace([...typingLibs, ...this.sourceLibs(sources)]);
    } finally {
      // Even a throw must not leave validation off and the overlay up
      // forever: mark it ready, clear the markers of the half-loaded
      // program so no false TS2307 survives, and validate again.
      if (!this.disposed) {
        this.ready = true;
        clearTypeScriptMarkers();
        setSemanticValidationEnabled(true);
      }
    }
  }

  private async applySourceChanges(
    changes: ReadonlyMap<string, string | null | undefined>
  ): Promise<void> {
    const read = await Promise.all(
      [...changes].map(async ([path, content]) => {
        // A read that fails leaves the lib as it is: a deletion comes as
        // its own event.
        const text =
          content !== undefined ? content : await readFileContent(path).catch(() => undefined);
        return [modelUriFor(path).toString(), text] as const;
      })
    );
    if (this.disposed) return;
    for (const [uri, text] of read) {
      if (text === null) workerLibs.delete(uri);
      else if (text !== undefined) workerLibs.set(uri, text);
    }
  }

  private async reloadSources(): Promise<void> {
    const sources = await loadProjectSources(this.root).catch(() => null);
    if (this.disposed || !sources) return;
    this.warnIfTruncated('sources', sources);
    workerLibs.replace(this.sourceLibs(sources), (uri) => !isDependencyPath(uri));
  }

  private async refreshTypings(): Promise<void> {
    const typings = await loadProjectTypings(this.root).catch(() => null);
    if (this.disposed || !typings) return; // keep the old libs
    this.warnIfTruncated('typings', typings);
    const typingLibs = this.typingLibs(typings);
    workerLibs.replace(typingLibs, isDependencyPath);
    this.types.setInstalled(typingLibs.map((lib) => lib.filePath));
    this.types.apply();
  }

  private async reloadConfig(): Promise<void> {
    const config = await readProjectConfig(this.root);
    if (this.disposed) return;
    this.types.configure(config);
    this.types.apply();
  }

  private typingLibs(typings: LoadedFiles): { filePath: string; content: string }[] {
    return typings.files.map((f) => ({
      filePath: dependencyUriFor(f.path),
      content: f.content,
    }));
  }

  private sourceLibs(sources: LoadedFiles): LibEntry[] {
    return sources.files.map((f) => ({
      filePath: modelUriFor(f.path).toString(),
      content: f.content,
    }));
  }

  private warnIfTruncated(what: string, loaded: LoadedFiles): void {
    if (loaded.truncated) {
      console.warn(`IntelliSense: not all ${what} of ${this.root} were loaded (limit reached)`);
    }
  }
}
