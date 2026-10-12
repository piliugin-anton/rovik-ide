// projectWorker.ts
//
// The TypeScript worker (Monaco's, spawned by lib/monacoSetup.ts through
// worker.ts), with one change: which files make up the program.
//
// Monaco's worker lists every extra lib as a root file of the program, so
// each typing file under node_modules was parsed and bound whether or not
// anything imported it. For a project with a few thousand dependencies
// that was gigabytes of syntax trees, built again on every change, until
// the webview process was killed for its memory. Here the program is the
// open models and the project's own sources; a dependency file is only
// read when an import, a `/// <reference>` or a `types` entry resolves to
// it, as node_modules is for tsc. The base class already reads libs on
// demand (readFile / fileExists), so nothing else changes.
//
// A file that is both a model (an open tab) and a lib is listed once: the
// model, which the base class reads first.
import {
  libFileMap,
  TypeScriptWorker,
  type MirrorModel,
} from 'monaco-editor/language/typescript/ts.worker.js';
import { isDependencyPath } from './paths';

export class ProjectTypeScriptWorker extends TypeScriptWorker {
  override getScriptFileNames(): string[] {
    const models = this._ctx
      .getMirrorModels()
      .map((model) => model.uri)
      .filter((uri) => !isDefaultLib(uri))
      .map((uri) => uri.toString());
    const open = new Set(models);
    const sources = Object.keys(this.getExtraLibs()).filter(
      (uri) => !open.has(uri) && !isDependencyPath(uri)
    );
    return models.concat(sources);
  }

  /**
   * A model's version, told apart from its lib copy's: when the model goes
   * (idle clean-up, lib/models.ts) the lib takes over, and a coinciding
   * number would make TypeScript keep the model's text.
   */
  override getScriptVersion(fileName: string): string {
    const version = super.getScriptVersion(fileName);
    return this._getModel(fileName) ? `model:${version}` : version;
  }
}

/** Monaco's own lib.*.d.ts, which it may open as models (peek definition). */
function isDefaultLib(uri: MirrorModel['uri']): boolean {
  return uri.path.startsWith('/lib.') && uri.path.slice(1) in libFileMap;
}
