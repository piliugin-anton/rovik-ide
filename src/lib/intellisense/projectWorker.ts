// projectWorker.ts
//
// The TypeScript worker (Monaco's, spawned by lib/monacoSetup.ts through
// worker.ts), with three changes.
//
// Which files make up the program. Monaco's worker lists every extra lib
// as a root file, so each typing file under node_modules was parsed and
// bound whether or not anything imported it. For a project with a few
// thousand dependencies that was gigabytes of syntax trees, built again
// on every change, until the webview process was killed for its memory.
// Here the program is the open models and the project's own sources; a
// dependency file is only read when an import, a `/// <reference>` or a
// `types` entry resolves to it, as node_modules is for tsc. The base class
// already reads libs on demand (readFile / fileExists). A file that is
// both a model (an open tab) and a lib is listed once: the model, which
// the base class reads first.
//
// Links. node_modules is full of links to folders (pnpm links every
// dependency into its store, workspaces link members into the root), and
// a linked package is loaded once, under its real path (typings/
// node_modules.rs). A path through a link is read as its target, and
// `realpath` tells TypeScript the real one, so it keeps one source file
// for a package however many paths reach it, as tsc does.
//
// Keys. A dependency lib is keyed with the part after `node_modules` raw
// (uris.ts); a dependency file opened as a tab is a model whose URI has
// that part percent-encoded (`%40mui`). Every name is taken in the raw
// form: as the model's root name, in what Monaco asks about the model,
// and in what TypeScript resolves. One name, so one file: an opened
// @types/react would otherwise be a second copy of it in the program,
// declaring the global JSX namespace again.
//
// Which files. The program is the active config's (PROJECT_LIB, from
// activeConfig.ts): of the sources and models, those the config takes in,
// as tsserver's program for that file. With no config, every source. A
// model outside the program gets empty answers, not errors.
//
// Old programs. Each edit builds a new program; the one it replaces is
// emptied (replacedPrograms.ts). Under JavaScriptCore every program stayed
// reachable from the next, with its type checker, and typing in a big
// project grew the worker until the web view was killed.
//
// Imports that found nothing. TypeScript keeps what a file's imports
// resolved to until that file changes; tsserver also watches where a
// failed lookup looked, which a language service host can't. So a file
// arriving later (a new source outside the active config, a package's
// typings once it's installed) left its importers saying it doesn't exist
// until they were edited. When the libs gain or lose a file, the next
// program resolves every import again (hasInvalidatedResolutions).
import {
  libFileMap,
  ts,
  TypeScriptWorker,
  type CreateData,
  type ExtraLib,
  type MirrorModel,
  type WorkerContext,
} from 'monaco-editor/language/typescript/ts.worker.js';
import { projectCovers, type TsProject } from '../tsconfig';
import { isProjectSource, LINKS_LIB, PROJECT_LIB } from './paths';
import { releasingReplacedPrograms } from './replacedPrograms';

/** Links to links: pnpm's store is one hop, a linked member's own links two. */
const MAX_LINK_HOPS = 8;

export class ProjectTypeScriptWorker extends TypeScriptWorker {
  private linksCache: { version: number; links: ReadonlyMap<string, string> } | undefined;
  private projectCache:
    { version: number; project: TsProject | null; covered: Map<string, boolean> } | undefined;
  /** Bumped each time a file comes to the libs or goes. */
  private filesVersion = 0;
  /** The files the current program's imports were resolved against. */
  private resolvedWith = -1;

  constructor(ctx: WorkerContext, createData: CreateData) {
    super(ctx, createData);
    // Every call of Monaco's and ours goes through it. A new program has
    // resolved every import it had to against the libs as they are.
    this._languageService = releasingReplacedPrograms(this._languageService, () => {
      this.resolvedWith = this.filesVersion;
    });
  }

  override async updateExtraLibs(extraLibs: Record<string, ExtraLib>): Promise<void> {
    if (!sameNames(this.getExtraLibs(), extraLibs)) this.filesVersion++;
    return super.updateExtraLibs(extraLibs);
  }

  /**
   * Whether a file's imports must be resolved again, though the file
   * didn't change: once the libs gained or lost a file since the program
   * was built. TypeScript asks this of every file of the program (unbound,
   * hence an arrow) while it decides whether the program is current.
   */
  readonly hasInvalidatedResolutions = (): boolean => this.resolvedWith !== this.filesVersion;

  override getScriptFileNames(): string[] {
    const models = this._ctx
      .getMirrorModels()
      .map((model) => model.uri)
      .filter((uri) => !isDefaultLib(uri))
      .map((uri) => libKeyOf(uri.toString()))
      .filter((key) => this.inProject(key));
    const open = new Set(models);
    // Libs that aren't sources (typings, package.json, the links) are only
    // read when something resolves to them.
    const sources = Object.keys(this.getExtraLibs()).filter(
      (uri) => !open.has(uri) && isProjectSource(uri) && this.inProject(uri)
    );
    return models.concat(sources);
  }

  /** Whether the active config takes the file in (every file without one). */
  private inProject(key: string): boolean {
    const lib = this.getExtraLibs()[PROJECT_LIB];
    if (!lib) return true;
    if (this.projectCache?.version !== lib.version) {
      let project: TsProject | null = null;
      try {
        project = JSON.parse(lib.content) as TsProject | null;
      } catch {
        // a bad payload means every file, not a dead worker
      }
      this.projectCache = { version: lib.version, project, covered: new Map() };
    }
    const { project, covered } = this.projectCache;
    if (!project) return true;
    let answer = covered.get(key);
    if (answer === undefined) {
      answer = projectCovers(project, pathOfUri(key));
      covered.set(key, answer);
    }
    return answer;
  }

  /** Whether TypeScript's program holds the file (a root, or reached from one). */
  inProgram(key: string): boolean {
    return this.getLanguageService().getProgram()?.getSourceFile(key) !== undefined;
  }

  /**
   * A model's version, told apart from its lib copy's: when the model goes
   * (idle clean-up, lib/models.ts) the lib takes over, and a coinciding
   * number would make TypeScript keep the model's text.
   */
  override getScriptVersion(fileName: string): string {
    const version = super.getScriptVersion(this.realpath(fileName));
    return this._getModel(fileName) ? `model:${version}` : version;
  }

  /** What readFile, fileExists and getScriptSnapshot read: through the links. */
  protected override _getScriptText(fileName: string): string | undefined {
    return super._getScriptText(this.realpath(fileName));
  }

  /** The model for a name in either form: as Monaco keys it, or as the libs do. */
  protected override _getModel(fileName: string): MirrorModel | null {
    const direct = super._getModel(fileName);
    if (direct) return direct;
    const key = libKeyOf(fileName);
    return this._ctx.getMirrorModels().find((m) => libKeyOf(m.uri.toString()) === key) ?? null;
  }

  /** The path a link resolves to; TypeScript keys a resolved module by it. */
  realpath(path: string): string {
    const links = this.links();
    let current = libKeyOf(path);
    if (links.size === 0) return current;
    for (let hop = 0; hop < MAX_LINK_HOPS; hop++) {
      const next = throughLinks(current, links);
      if (next === current) break;
      current = next;
    }
    return current;
  }

  /**
   * For the diagnostics aid (devtools.ts): what the program holds, which
   * packages it holds more than once (the usual cause of "X is not
   * assignable to X" between two copies of @types/react), and where each
   * import of `fileName` resolves to.
   */
  async programInfo(fileName: string): Promise<ProgramInfo> {
    const files = this.getLanguageService().getProgram()?.getSourceFiles() ?? [];
    const byPackage = new Map<string, Set<string>>();
    for (const { fileName: name } of files) {
      const pkg = packageOf(name);
      if (!pkg) continue;
      const dirs = byPackage.get(pkg.name) ?? new Set();
      dirs.add(pkg.dir);
      byPackage.set(pkg.name, dirs);
    }
    const duplicates: Record<string, string[]> = {};
    for (const [name, dirs] of byPackage) if (dirs.size > 1) duplicates[name] = [...dirs].sort();
    const imports: Record<string, string | null> = {};
    const text = this._getScriptText(fileName);
    if (text !== undefined) {
      const api = ts.typescript;
      for (const { fileName: spec } of api.preProcessFile(text, true, true).importedFiles) {
        const resolved = api.resolveModuleName(spec, fileName, this.getCompilationSettings(), this);
        imports[spec] = resolved.resolvedModule?.resolvedFileName ?? null;
      }
    }
    return { files: files.length, duplicates, imports };
  }

  private links(): ReadonlyMap<string, string> {
    const lib = this.getExtraLibs()[LINKS_LIB];
    if (!lib) return new Map();
    if (this.linksCache?.version !== lib.version) {
      let parsed: Record<string, string> = {};
      try {
        parsed = JSON.parse(lib.content) as Record<string, string>;
      } catch {
        // a bad payload means no links, not a dead worker
      }
      this.linksCache = { version: lib.version, links: new Map(Object.entries(parsed)) };
    }
    return this.linksCache.links;
  }
}

/**
 * Monaco's language features, by name, with what to answer for a file
 * outside the program (the adapters expect these shapes, and a rejection
 * from the worker is an unhandled one). Each takes the file's name first.
 */
const FEATURES: Record<string, () => unknown> = {
  getScriptText: () => undefined,
  getSyntacticDiagnostics: () => [],
  getSemanticDiagnostics: () => [],
  getSuggestionDiagnostics: () => [],
  getCompilerOptionsDiagnostics: () => [],
  getCompletionsAtPosition: () => undefined,
  getCompletionEntryDetails: () => undefined,
  getSignatureHelpItems: () => undefined,
  getQuickInfoAtPosition: () => undefined,
  getDocumentHighlights: () => [],
  getDefinitionAtPosition: () => [],
  getReferencesAtPosition: () => [],
  getNavigationTree: () => undefined,
  getFormattingEditsForDocument: () => [],
  getFormattingEditsForRange: () => [],
  getFormattingEditsAfterKeystroke: () => [],
  findRenameLocations: () => [],
  getRenameInfo: () => ({ canRename: false, localizedErrorMessage: 'Not in the program.' }),
  getEmitOutput: () => ({ outputFiles: [], emitSkipped: true }),
  getCodeFixesAtPosition: () => [],
  provideInlayHints: () => [],
};

type Feature = (this: ProjectTypeScriptWorker, fileName: string, ...rest: unknown[]) => unknown;

// Every feature takes the file's name in the raw form, and answers
// nothing for a file outside the program. On the prototype, where Monaco
// finds the methods it exposes to the main thread.
for (const [name, empty] of Object.entries(FEATURES)) {
  const base = (TypeScriptWorker.prototype as unknown as Record<string, Feature | undefined>)[name];
  if (typeof base !== 'function') continue;
  const wrapped: Feature = async function (fileName, ...rest) {
    const key = libKeyOf(fileName);
    if (!isDefaultLibName(key) && !this.inProgram(key)) return empty();
    return base.call(this, key, ...rest);
  };
  (ProjectTypeScriptWorker.prototype as unknown as Record<string, Feature>)[name] = wrapped;
}

export interface ProgramInfo {
  /** Files in the program, Monaco's libs included. */
  files: number;
  /** Packages present under more than one folder, by name. */
  duplicates: Record<string, string[]>;
  /** Each import specifier of the file, and the file it resolves to (null: unresolved). */
  imports: Record<string, string | null>;
}

const PACKAGE_DIR = /^(.*\/node_modules\/((?:@[^/]+\/)?[^/]+))\//;

/** The package a dependency file belongs to: its name, and its folder (the last `node_modules/<name>` of the path). */
function packageOf(fileName: string): { name: string; dir: string } | null {
  const match = PACKAGE_DIR.exec(fileName);
  return match ? { name: match[2], dir: match[1] } : null;
}

/** A path as the libs are keyed: raw after the first `node_modules`. */
function libKeyOf(path: string): string {
  const at = path.indexOf('/node_modules/');
  if (at < 0 || !path.includes('%', at)) return path;
  try {
    return path.slice(0, at) + decodeURIComponent(path.slice(at));
  } catch {
    return path;
  }
}

/** Whether two lib sets hold the same files (whatever their contents). */
function sameNames(a: Record<string, unknown>, b: Record<string, unknown>): boolean {
  const names = Object.keys(b);
  if (names.length !== Object.keys(a).length) return false;
  return names.every((name) => name in a);
}

/** `path` with its longest leading link (a folder) replaced by that link's target. */
function throughLinks(path: string, links: ReadonlyMap<string, string>): string {
  let end = path.length;
  while (end > 0) {
    const target = links.get(path.slice(0, end));
    if (target !== undefined) return target + path.slice(end);
    end = path.lastIndexOf('/', end - 1);
  }
  return path;
}

/** Monaco's own lib.*.d.ts, which it may open as models (peek definition). */
function isDefaultLib(uri: MirrorModel['uri']): boolean {
  return uri.path.startsWith('/lib.') && uri.path.slice(1) in libFileMap;
}

/** The same, by name, as Monaco's worker checks it. */
function isDefaultLibName(fileName: string): boolean {
  return fileName.startsWith('file:///') && fileName.slice('file:///'.length) in libFileMap;
}

/** A file URI as an absolute path, as the configs' patterns are (lib/tsconfig.ts). */
function pathOfUri(uri: string): string {
  if (!uri.startsWith('file://')) return uri;
  let path: string;
  try {
    path = decodeURIComponent(uri.slice('file://'.length));
  } catch {
    path = uri.slice('file://'.length);
  }
  // Windows: `/c:/x` is `c:/x`.
  return /^\/[A-Za-z]:\//.test(path) ? path.slice(1) : path;
}
