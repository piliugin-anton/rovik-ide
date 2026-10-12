// The parts of Monaco's TypeScript worker module that worker.ts builds on.
// The package ships no declarations for it (only for the editor API).
declare module 'monaco-editor/language/typescript/ts.worker.js' {
  import type { Uri, typescript } from 'monaco-editor';

  export interface MirrorModel {
    uri: Uri;
    version: number;
    getValue(): string;
  }

  export interface WorkerContext {
    getMirrorModels(): MirrorModel[];
  }

  export interface ExtraLib {
    content: string;
    version: number;
  }

  export interface CreateData {
    compilerOptions: typescript.CompilerOptions;
    extraLibs: Record<string, ExtraLib>;
    customWorkerPath?: string;
    inlayHintsOptions?: typescript.InlayHintsOptions;
  }

  export class TypeScriptWorker {
    constructor(ctx: WorkerContext, createData: CreateData);
    protected _ctx: WorkerContext;
    protected _getModel(fileName: string): MirrorModel | null;
    protected _getScriptText(fileName: string): string | undefined;
    getExtraLibs(): Record<string, ExtraLib>;
    getScriptFileNames(): string[];
    getScriptVersion(fileName: string): string;
    readFile(path: string): string | undefined;
    updateExtraLibs(extraLibs: Record<string, ExtraLib>): Promise<void>;
    getSyntacticDiagnostics(fileName: string): Promise<typescript.Diagnostic[]>;
    getSemanticDiagnostics(fileName: string): Promise<typescript.Diagnostic[]>;
    getCompilerOptionsDiagnostics(fileName: string): Promise<typescript.Diagnostic[]>;
    getDefinitionAtPosition(
      fileName: string,
      position: number
    ): Promise<readonly { fileName: string }[] | undefined>;
  }

  export function initialize(
    factory: (ctx: WorkerContext, createData: CreateData) => TypeScriptWorker
  ): void;

  /** Monaco's bundled lib.*.d.ts files, by name. */
  export const libFileMap: Record<string, string>;
}
