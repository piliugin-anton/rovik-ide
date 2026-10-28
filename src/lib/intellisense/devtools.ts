// devtools.ts
//
// Diagnostics aid: run `window.__rovikIntellisense()` in DevTools when a
// tab has no IntelliSense. It shows whether the project's libs reached the
// worker (extraLibs), what models exist, which carry TS markers (i.e.
// whether the worker validates them at all), and what the worker itself
// answers for the visible file.
import * as monaco from 'monaco-editor';
import { workerLibs } from './libs';
import type { ProgramInfo } from './projectWorker';

export interface IntelliSenseState {
  /** The folders the worker has: a project's one, a workspace's several (C06). */
  roots: readonly string[];
  ready: boolean;
  /** The compiler options in effect (types, configFilePath...). */
  compilerOptions: monaco.typescript.CompilerOptions;
}

interface IntellisenseDiag extends IntelliSenseState {
  extraLibs: number;
  extraLibSample: string[];
  /** Prototype of what getExtraLibs() returns (what is posted to the worker). */
  extraLibsProto: string;
  /** structuredClone() of a 3-entry sample: proves cloneability without cloning everything. */
  cloneProbe: string;
  models: { uri: string; language: string; attached: boolean; disposed: boolean }[];
  markers: { uri: string; owner: string; count: number; codes: string[] }[];
  /** Direct answers from the TS worker for the attached (visible) model. */
  worker?: {
    uri: string;
    syntactic: string[];
    semantic: string[];
    /** What the program holds and how this file's imports resolve. */
    program?: ProgramInfo;
    error?: string;
  };
}

declare global {
  interface Window {
    __rovikIntellisense?: () => Promise<IntellisenseDiag>;
  }
}

export function installIntelliSenseDevtools(state: () => IntelliSenseState): void {
  window.__rovikIntellisense = () => intellisenseDiag(state());
}

async function intellisenseDiag(state: IntelliSenseState): Promise<IntellisenseDiag> {
  const libs = workerLibs.snapshot();
  const keys = Object.keys(libs);
  const models = monaco.editor.getModels().map((m) => ({
    uri: m.uri.toString(),
    language: m.getLanguageId(),
    attached: m.isAttachedToEditor(),
    disposed: m.isDisposed(),
  }));
  const markers = models.map((m) => {
    const all = monaco.editor.getModelMarkers({ resource: monaco.Uri.parse(m.uri) });
    return {
      uri: m.uri,
      owner: all[0]?.owner ?? '(none)',
      count: all.length,
      codes: [
        ...new Set(all.map((x) => String(typeof x.code === 'object' ? x.code?.value : x.code))),
      ].slice(0, 10),
    };
  });
  const diag: IntellisenseDiag = {
    ...state,
    extraLibs: keys.length,
    extraLibSample: keys.slice(0, 5),
    extraLibsProto: protoName(libs),
    cloneProbe: cloneProbe(libs, keys.slice(0, 3)),
    models,
    markers,
  };
  // Ask the worker itself about the visible file: this is what completions /
  // hover / go-to-definition see, bypassing markers and providers.
  const attached = monaco.editor.getModels().find((m) => m.isAttachedToEditor());
  const lang = attached?.getLanguageId();
  if (attached && (lang === 'typescript' || lang === 'javascript')) {
    const uri = attached.uri.toString();
    try {
      const getter =
        lang === 'typescript'
          ? await monaco.typescript.getTypeScriptWorker()
          : await monaco.typescript.getJavaScriptWorker();
      const proxy = await getter(attached.uri);
      // The project worker's own method (projectWorker.ts): not in Monaco's type.
      const project = proxy as unknown as { programInfo(fileName: string): Promise<ProgramInfo> };
      const [syn, sem, program] = await Promise.all([
        proxy.getSyntacticDiagnostics(uri),
        proxy.getSemanticDiagnostics(uri),
        project.programInfo(uri),
      ]);
      diag.worker = {
        uri,
        syntactic: syn.map((d) => String(d.code)).slice(0, 10),
        semantic: sem.map((d) => `${d.code}: ${flattenMessage(d.messageText)}`).slice(0, 10),
        program,
      };
    } catch (err) {
      diag.worker = { uri, syntactic: [], semantic: [], error: String(err) };
    }
  }
  return diag;
}

function protoName(value: object): string {
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype ? 'plain' : proto === null ? 'null' : 'other';
}

function cloneProbe(libs: Record<string, unknown>, keys: string[]): string {
  try {
    const sample: Record<string, unknown> = {};
    for (const k of keys) sample[k] = libs[k];
    structuredClone(sample);
    return 'cloneable';
  } catch (err) {
    return `NOT-CLONEABLE: ${err instanceof Error ? `${err.name}: ${err.message}` : String(err)}`;
  }
}

function flattenMessage(msg: unknown): string {
  if (typeof msg === 'string') return msg;
  if (msg && typeof msg === 'object' && 'messageText' in msg)
    return flattenMessage((msg as { messageText: unknown }).messageText);
  return '';
}
