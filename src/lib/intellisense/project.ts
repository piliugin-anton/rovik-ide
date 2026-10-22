// project.ts
//
// One project's definitions in the TypeScript worker: loaded once, then
// kept current from the file watcher (App.tsx) through the facade
// (index.ts), and grown as the code imports more. A new project replaces
// the old one: the worker holds one project's libs and options.
//
// The dependencies come as the closure of what the code imports
// (rovik_core::typings): the packages the sources, configs and manifests
// name, and what those packages' declarations import in turn. An import
// typed later, of an installed package nothing imported before, is looked
// up from the edited model or the changed file once typing pauses, and
// its closure is added (dependencies.ts keeps what is loaded, and what
// isn't installed, so nothing is asked for twice).
import * as monaco from 'monaco-editor';
import {
  loadDependencyTypings,
  loadProjectDefinitions,
  loadProjectSources,
  loadProjectTypings,
  readFileContent,
  readFileUnlessGone,
} from '../commands';
import type { ImportSite, LoadedFiles, LoadedTypings } from '../../types';
import { notify } from '../notifications';
import { readProjectConfig } from './compilerOptions';
import { clearTypeScriptMarkers, setSemanticValidationEnabled } from './defaults';
import { Dependencies } from './dependencies';
import { bareSpecifiers } from './imports';
import { workerLibs, type LibEntry } from './libs';
import {
  isLoadedLib,
  isProjectSource,
  isSourceLib,
  isTsConfig,
  isTypingsLib,
  isTypingsManifest,
  LINKS_LIB,
  SOURCE_FILE,
} from './paths';
import { QuietRunner } from './quietRunner';
import { refreshSemanticTokens } from './semanticTokens';
import { SourceChangeQueue, type SourceChangeSink } from './sourceChanges';
import { ActiveConfig } from './activeConfig';
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
/** An import typed or saved: its package is looked up once typing pauses. */
const IMPORTS_QUIET_MS = 300;

const NOTHING: LoadedFiles = { files: [], links: [], truncated: false };
const NO_TYPINGS: LoadedTypings = { ...NOTHING, packages: [], missing: [] };

export class IntelliSenseProject {
  /** Resolves when the first load is done (success or not). */
  readonly loaded: Promise<void>;
  /** The definitions have fully reached the worker. */
  ready = false;
  private disposed = false;
  private readonly types: ActiveConfig;
  private readonly sources: SourceChangeQueue;
  private readonly typingsRefresh: QuietRunner;
  private readonly configReload: QuietRunner;
  private readonly importsLookup: QuietRunner;
  /** tsconfig/jsconfig files found in the project (members' included). */
  private configPaths = new Set<string>();
  /** The dependencies loaded so far, and the imports found uninstalled. */
  private readonly deps = new Dependencies();
  /** Every dependency file's URI: the active config's @types come from them. */
  private readonly typingUris = new Set<string>();
  /** Imports whose package isn't loaded, by importing file, until looked up. */
  private readonly wanted = new Map<string, Set<string>>();
  /** The sources the worker has, path by lib URI: a folder that goes takes its own. */
  private readonly sourcePaths = new Map<string, string>();
  /**
   * Changes that came while the first load ran: its walk may have been
   * before them, and the set it gives the worker replaces every source.
   */
  private readonly early = new Map<string, string | null | undefined>();
  /** Open models edited since their imports were last looked at. */
  private readonly editedModels = new Set<monaco.editor.ITextModel>();
  private readonly modelListeners: monaco.IDisposable[] = [];

  constructor(
    readonly root: string,
    activeFile: string | null
  ) {
    this.types = new ActiveConfig(root);
    this.types.setActiveFile(activeFile);
    const sink: SourceChangeSink = {
      apply: (changes) => this.applySourceChanges(changes),
      reload: () => this.reloadSources(),
    };
    this.sources = new SourceChangeQueue(sink);
    this.typingsRefresh = new QuietRunner(() => this.refreshTypings(), TYPINGS_QUIET_MS);
    this.configReload = new QuietRunner(() => this.reloadConfig(), CONFIG_QUIET_MS);
    this.importsLookup = new QuietRunner(() => this.lookUpImports(), IMPORTS_QUIET_MS);
    this.watchModels();
    this.loaded = this.load();
  }

  /** Stops every pending update; the worker's libs are left for the next project. */
  dispose(): void {
    this.disposed = true;
    this.sources.dispose();
    this.typingsRefresh.dispose();
    this.configReload.dispose();
    this.importsLookup.dispose();
    for (const listener of this.modelListeners) listener.dispose();
    this.modelListeners.length = 0;
    this.editedModels.clear();
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
    if (!this.owns(path) || !isProjectSource(path, this.root)) return;
    this.sources.queue(path, content);
  }

  /**
   * Something that isn't a source changed: if it was a folder that went
   * (removed, or renamed away: a rename reports the folder alone), the
   * sources the worker has in it are read again, and go with it.
   */
  pathChanged(path: string): void {
    if (!this.owns(path)) return;
    const inside = `${modelUriFor(path).toString().replace(/\/+$/, '')}/`;
    for (const [uri, file] of this.sourcePaths) {
      if (uri.startsWith(inside)) this.sources.queue(file);
    }
  }

  /**
   * Too much changed for the watcher to say what (a branch switch, an
   * unpacked archive): the sources, their configs and the dependencies'
   * typings are read again once it settles.
   */
  everythingChanged(): void {
    if (!this.ready) return; // the first load is reading it all already
    this.sources.reloadAll();
    this.typingsRefresh.trigger();
  }

  /** Any file was written: a manifest or a tsconfig means more than one source. */
  fileWritten(path: string): void {
    // The initial load reads node_modules and the configs itself.
    if (!this.owns(path) || !this.ready) return;
    if (isTypingsManifest(path)) this.typingsRefresh.trigger();
    if (isTsConfig(path)) {
      this.configPaths.add(path);
      this.configReload.trigger();
    }
  }

  private owns(path: string): boolean {
    return forwardSlashes(path).startsWith(forwardSlashes(this.root));
  }

  private async load(): Promise<void> {
    // A new project has no libs yet: no semantic errors until it is loaded.
    setSemanticValidationEnabled(false);
    try {
      const { sources, typings } = await loadProjectDefinitions(this.root).catch(() => ({
        sources: NOTHING,
        typings: NO_TYPINGS,
      }));
      if (this.disposed) return;
      // The configs are among the sources: no second trip to disk for them.
      this.noteConfigs(sources);
      const loaded = new Map(sources.files.map((f) => [f.path, f.content]));
      const config = await readProjectConfig(
        this.root,
        [...this.configPaths],
        async (path) => loaded.get(path) ?? readFileContent(path)
      );
      if (this.disposed) return;
      this.warnIfTruncated('typings', typings);
      this.warnIfTruncated('sources', sources);
      this.deps.reset(typings);
      const typingLibs = this.typingLibs(typings);
      // Options first: a change restarts the worker, which then starts
      // with the complete lib set instead of receiving it twice.
      this.types.configure(config);
      this.applyInstalled();
      workerLibs.replace([...typingLibs, ...this.sourceLibs(sources)], isLoadedLib);
      this.noteSources(sources);
    } finally {
      // Even a throw must not leave validation off and the overlay up
      // forever: mark it ready, clear the markers of the half-loaded
      // program so no false TS2307 survives, and validate again.
      if (!this.disposed) {
        this.ready = true;
        clearTypeScriptMarkers();
        setSemanticValidationEnabled(true);
        // Files open while the libs were loading were classified against an
        // empty program (globals, imported symbols): classify them again.
        refreshSemanticTokens();
        // Imports typed while the project loaded.
        if (this.editedModels.size > 0) this.importsLookup.trigger();
        // Files changed meanwhile, on top of what it loaded.
        if (this.early.size > 0) {
          const early = new Map(this.early);
          this.early.clear();
          void this.applySourceChanges(early);
        }
      }
    }
  }

  private async applySourceChanges(
    changes: ReadonlyMap<string, string | null | undefined>
  ): Promise<void> {
    if (!this.ready) {
      for (const [path, content] of changes) this.early.set(path, content);
      return;
    }
    const read = await Promise.all(
      [...changes].map(async ([path, content]) => {
        // Gone: its lib goes. There but unreadable (binary, too large): it stays.
        const text = content !== undefined ? content : await readFileUnlessGone(path);
        return [path, text] as const;
      })
    );
    if (this.disposed) return;
    for (const [path, text] of read) {
      const uri = modelUriFor(path).toString();
      if (text === null) {
        workerLibs.delete(uri);
        this.sourcePaths.delete(uri);
      } else if (text !== undefined) {
        workerLibs.set(uri, text);
        this.sourcePaths.set(uri, path);
        this.noteImports(path, text);
      }
    }
    if (this.wanted.size > 0) this.importsLookup.trigger();
  }

  private async reloadSources(): Promise<void> {
    const sources = await loadProjectSources(this.root).catch(() => null);
    if (this.disposed || !sources) return;
    this.warnIfTruncated('sources', sources);
    this.noteConfigs(sources);
    workerLibs.replace(this.sourceLibs(sources), isSourceLib);
    this.noteSources(sources);
    for (const file of sources.files) this.noteImports(file.path, file.content);
    if (this.wanted.size > 0) this.importsLookup.trigger();
    // Configs may have come, gone or changed with the rest.
    this.configReload.trigger();
  }

  private async refreshTypings(): Promise<void> {
    const typings = await loadProjectTypings(this.root).catch(() => null);
    if (this.disposed || !typings) return; // keep the old libs
    this.warnIfTruncated('typings', typings);
    this.deps.reset(typings);
    workerLibs.replace(this.typingLibs(typings), isTypingsLib);
    this.applyInstalled();
    // An import typed into a tab, not saved yet, isn't among the sources
    // the closure was read from: looked up again from the open models.
    for (const model of monaco.editor.getModels()) {
      if (this.isSourceModel(model)) this.editedModels.add(model);
    }
    this.importsLookup.trigger();
  }

  private async reloadConfig(): Promise<void> {
    const config = await readProjectConfig(this.root, [...this.configPaths]);
    if (this.disposed) return;
    this.types.configure(config);
    this.types.apply();
  }

  /** Notes the imports of a changed file or model whose package isn't loaded, for the next look-up. */
  private noteImports(path: string, text: string): void {
    const specifiers = this.deps.wanted(bareSpecifiers(text));
    if (specifiers.length === 0) return;
    const set = this.wanted.get(path) ?? new Set<string>();
    for (const spec of specifiers) set.add(spec);
    this.wanted.set(path, set);
  }

  /** Loads the packages the noted imports reach, and adds them to the worker's libs. */
  private async lookUpImports(): Promise<void> {
    if (!this.ready) return; // the first load's closure covers the sources
    for (const model of this.editedModels) {
      if (!model.isDisposed()) this.noteImports(pathOfModel(model.uri), model.getValue());
    }
    this.editedModels.clear();
    if (this.wanted.size === 0) return;
    const sites: ImportSite[] = [...this.wanted].map(([from, specifiers]) => ({
      from,
      specifiers: [...specifiers],
    }));
    this.wanted.clear();
    const typings = await loadDependencyTypings(this.root, sites, this.deps.loadedDirs).catch(
      () => null
    );
    if (this.disposed || !typings) return;
    this.warnIfTruncated('typings', typings);
    const linksChanged = this.deps.absorb(typings);
    for (const lib of this.typingFileLibs(typings)) workerLibs.set(lib.filePath, lib.content);
    if (linksChanged) workerLibs.set(LINKS_LIB, this.linksLib());
    this.applyInstalled();
  }

  /** Open TypeScript/JavaScript models: their imports are looked at as they're typed. */
  private watchModels(): void {
    const watch = (model: monaco.editor.ITextModel) => {
      if (!this.isSourceModel(model)) return;
      const edited = model.onDidChangeContent(() => {
        this.editedModels.add(model);
        this.importsLookup.trigger();
      });
      const gone = model.onWillDispose(() => {
        edited.dispose();
        this.editedModels.delete(model);
      });
      this.modelListeners.push(edited, gone);
    };
    try {
      for (const model of monaco.editor.getModels()) watch(model);
      this.modelListeners.push(monaco.editor.onDidCreateModel(watch));
    } catch {
      // no editor (tests): imports come from the files alone
    }
  }

  private isSourceModel(model: monaco.editor.ITextModel): boolean {
    const path = pathOfModel(model.uri);
    return SOURCE_FILE.test(path) && this.owns(path);
  }

  /** The dependencies' files and links, as the worker's @types scope. */
  private applyInstalled(): void {
    this.types.setInstalled(this.typingUris, [...this.deps.allLinks.keys()].map(dependencyUriFor));
    this.types.apply();
  }

  /** The typings, and the links the worker resolves paths through. */
  private typingLibs(typings: LoadedTypings): { filePath: string; content: string }[] {
    this.typingUris.clear();
    return [...this.typingFileLibs(typings), { filePath: LINKS_LIB, content: this.linksLib() }];
  }

  private typingFileLibs(typings: LoadedTypings): { filePath: string; content: string }[] {
    return typings.files.map((f) => {
      const uri = dependencyUriFor(f.path);
      this.typingUris.add(uri);
      return { filePath: uri, content: f.content };
    });
  }

  private linksLib(): string {
    const links: Record<string, string> = {};
    for (const [path, target] of this.deps.allLinks)
      links[dependencyUriFor(path)] = dependencyUriFor(target);
    return JSON.stringify(links);
  }

  private noteConfigs(sources: LoadedFiles): void {
    this.configPaths = new Set(sources.files.map((f) => f.path).filter(isTsConfig));
  }

  /** The sources just loaded are all the worker has. */
  private noteSources(sources: LoadedFiles): void {
    this.sourcePaths.clear();
    for (const f of sources.files) this.sourcePaths.set(modelUriFor(f.path).toString(), f.path);
  }

  /** The sources, with the package.json files the worker reads for node16 resolution. */
  private sourceLibs(sources: LoadedFiles): LibEntry[] {
    return sources.files.map((f) => ({
      filePath: modelUriFor(f.path).toString(),
      content: f.content,
    }));
  }

  private warnIfTruncated(what: 'typings' | 'sources', loaded: LoadedFiles): void {
    if (!loaded.truncated) return;
    console.warn(`IntelliSense: not all ${what} of ${this.root} were loaded (limit reached)`);
    notify({
      id: `intellisense-truncated-${what}`,
      kind: 'warning',
      title: 'IntelliSense: not every definition was loaded',
      message:
        what === 'sources'
          ? 'The project has more JavaScript/TypeScript files than the editor loads (5,000 files or 40 MB). Imports of the files left out show as unresolved.'
          : 'The dependencies this project reaches are over what the editor loads (40,000 files or 128 MB). Some packages are left out.',
      timeout: 20_000,
    });
  }
}

function forwardSlashes(path: string): string {
  return path.replace(/\\/g, '/');
}

/** A model's file path, as the backend and the watcher name it (uris.ts). */
function pathOfModel(uri: monaco.Uri): string {
  const path = uri.path;
  // Windows: `/c:/x` is `c:/x`.
  return /^\/[A-Za-z]:\//.test(path) ? path.slice(1) : path;
}
