// defaults.ts
//
// Monaco's TypeScript and JavaScript language defaults, configured as one:
// both languages share the project's libs, options and validation state.
import * as monaco from 'monaco-editor';

const LANGUAGE_DEFAULTS = [
  monaco.typescript.typescriptDefaults,
  monaco.typescript.javascriptDefaults,
] as const;

export type LanguageDefaults = (typeof LANGUAGE_DEFAULTS)[number];

export function forEachDefaults(action: (defaults: LanguageDefaults) => void): void {
  for (const defaults of LANGUAGE_DEFAULTS) {
    try {
      action(defaults);
    } catch {
      // ignore: the defaults stay as they were
    }
  }
}

/** The defaults whose libs are read back (both hold the same set). */
export const primaryDefaults: LanguageDefaults = LANGUAGE_DEFAULTS[0];

export const BASE_OPTIONS: monaco.typescript.CompilerOptions = {
  target: monaco.typescript.ScriptTarget.ESNext,
  module: monaco.typescript.ModuleKind.ESNext,
  moduleResolution: monaco.typescript.ModuleResolutionKind.NodeJs,
  jsx: monaco.typescript.JsxEmit.Preserve,
  allowJs: true,
  checkJs: false,
  allowNonTsExtensions: true,
  esModuleInterop: true,
  allowSyntheticDefaultImports: true,
};

let appliedOptions = '';

/**
 * Gives the worker these compiler options, unless it has them already: a
 * change restarts the worker, which then reads every file of its program
 * again. Returns whether anything changed.
 */
export function applyCompilerOptions(options: monaco.typescript.CompilerOptions): boolean {
  const next = JSON.stringify(options);
  if (next === appliedOptions) return false;
  appliedOptions = next;
  forEachDefaults((defaults) => defaults.setCompilerOptions(options));
  return true;
}

/**
 * While a project's libs are still loading, the worker resolves every bare
 * import (and cross-file symbol) to nothing and reports TS2307 on each
 * import line: false positives that vanish once the libs arrive. Semantic
 * diagnostics stay off until the first load finishes.
 */
export function setSemanticValidationEnabled(enabled: boolean): void {
  forEachDefaults((defaults) =>
    defaults.setDiagnosticsOptions({
      ...defaults.getDiagnosticsOptions(),
      noSemanticValidation: !enabled,
    })
  );
}

/** Drops the markers a half-loaded program left on every model. */
export function clearTypeScriptMarkers(): void {
  for (const model of monaco.editor.getModels()) {
    for (const owner of ['typescript', 'javascript']) {
      try {
        monaco.editor.setModelMarkers(model, owner, []);
      } catch {
        // ignore
      }
    }
  }
}

let initialized = false;

/** One-time setup of both languages' defaults. */
export function initLanguageDefaults(): void {
  if (initialized) return;
  initialized = true;
  applyCompilerOptions(BASE_OPTIONS);
  // Multi-file project: every open TS/JS model belongs to the worker's
  // program (cross-file completions, go-to-definition), not just the model
  // that triggered worker creation. Without this the worker syncs models
  // lazily per request, which the multi-file guides (e.g. SO #57146485)
  // call out as the thing that breaks IDE-style tab setups.
  forEachDefaults((defaults) => defaults.setEagerModelSync(true));
  // The very first paint happens before any project libs reach the worker.
  setSemanticValidationEnabled(false);
  // Monaco stores extra libs in a null-prototype map (Object.create(null))
  // and forwards that same object to the worker via postMessage
  // (workerManager _updateExtraLibs / createData). Null-prototype objects
  // are a known DataCloneError suspect on some WebKit builds (Tauri's
  // WebView): the failure surfaces as an unhandled rejection from
  // webWorker.js with no method name. Hand the worker plain-prototype
  // copies instead; internal storage is untouched, only what getExtraLibs()
  // returns (and hence what is posted) changes.
  forEachDefaults((defaults) => {
    const original = defaults.getExtraLibs.bind(defaults);
    defaults.getExtraLibs = () => ({ ...original() });
  });
}
