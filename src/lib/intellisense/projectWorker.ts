// projectWorker.ts
//
// The TypeScript worker (Monaco's, spawned by lib/monacoSetup.ts through
// worker.ts), with two changes.
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
// that part percent-encoded (`%40mui`), and TypeScript builds the paths
// of its relative imports from that. Those are decoded back to the raw
// form, so the opened file's imports resolve to the libs.
import {
  libFileMap,
  TypeScriptWorker,
  type MirrorModel,
} from 'monaco-editor/language/typescript/ts.worker.js';
import { isProjectSource, LINKS_LIB } from './paths';

/** Links to links: pnpm's store is one hop, a linked member's own links two. */
const MAX_LINK_HOPS = 8;

export class ProjectTypeScriptWorker extends TypeScriptWorker {
  private linksCache: { version: number; links: ReadonlyMap<string, string> } | undefined;

  override getScriptFileNames(): string[] {
    const models = this._ctx
      .getMirrorModels()
      .map((model) => model.uri)
      .filter((uri) => !isDefaultLib(uri))
      .map((uri) => uri.toString());
    const open = new Set(models);
    // Libs that aren't sources (typings, package.json, the links) are only
    // read when something resolves to them.
    const sources = Object.keys(this.getExtraLibs()).filter(
      (uri) => !open.has(uri) && isProjectSource(uri)
    );
    return models.concat(sources);
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
