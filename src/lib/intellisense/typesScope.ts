// typesScope.ts
//
// Global @types, as tsc does them. A program sees the @types packages its
// config's `types` names: every package in node_modules/@types when unset
// (before TypeScript 6; none from 6 on). What the code imports resolves on
// its own whatever `types` says, since the worker can read any dependency
// file (worker.ts), as tsc can read node_modules.
//
// Monaco's worker has one program, so it gets the `types` of the config
// owning the active file (compilerOptions.types). TypeScript resolves the
// names itself, starting from the folder of compilerOptions.configFilePath
// (`vite/client` found in node_modules, `node` in node_modules/@types), so
// `process` is typed in vite.config.ts and not in browser code. Switching
// between files of one config costs nothing; a file of another config
// applies its `types`, which restarts the worker only when the list differs.
//
// Markers of other open files are computed under the active file's types
// meanwhile; covers() tells the diagnostics reporter which of them to trust
// (lib/diagnostics.ts).
import type * as monaco from 'monaco-editor';
import { projectFor, typesFor, type TsProject } from '../tsconfig';
import { SOURCE_FILE } from './paths';
import { sanitizeCompilerOptions, type ProjectConfig } from './compilerOptions';
import { applyCompilerOptions, BASE_OPTIONS } from './defaults';
import { installedTypesFor, typesPackagesByFolder } from './typeRoots';
import { folderUriFor, modelUriFor } from './uris';

function sameSet(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((x) => b.includes(x));
}

export class TypesScope {
  private projects: TsProject[] = [];
  private typesDefaultAll = true;
  private options: monaco.typescript.CompilerOptions = BASE_OPTIONS;
  /** @types package names by node_modules folder URI (typeRoots.ts). */
  private typesByFolder: ReadonlyMap<string, readonly string[]> = new Map();
  private activeFile: string | null = null;
  /** Owning config of a path (undefined: none), until the configs change. */
  private readonly owners = new Map<string, TsProject | undefined>();

  constructor(private readonly root: string) {}

  configure(config: ProjectConfig): void {
    this.projects = config.projects;
    this.typesDefaultAll = config.typesDefaultAll;
    this.options = config.options;
    this.owners.clear();
  }

  /** The @types packages installed, from the dependencies' file and link URIs. */
  setInstalled(fileUris: Iterable<string>, linkUris: Iterable<string>): void {
    this.typesByFolder = typesPackagesByFolder(fileUris, linkUris);
  }

  /** The file shown in the editor: its config's @types become the worker's. */
  setActiveFile(path: string | null): void {
    this.activeFile = path;
  }

  /** Gives the worker the active file's options; cheap when unchanged. */
  apply(): boolean {
    return applyCompilerOptions(this.optionsFor(this.activeFile));
  }

  /** The complete options for a file's config. */
  optionsFor(path: string | null): monaco.typescript.CompilerOptions {
    const owner = path === null ? undefined : this.ownerOf(path);
    return sanitizeCompilerOptions({
      ...this.options,
      types: this.typesOf(owner),
      configFilePath: modelUriFor(this.configPathOf(owner)).toString(),
    });
  }

  /**
   * Whether the worker's markers for `path` are computed with the @types
   * its own config allows (false while another config's file is active and
   * the two allow different ones).
   */
  covers(path: string): boolean {
    // Other files (Markdown, Rust...) aren't validated by the worker.
    if (!SOURCE_FILE.test(path)) return true;
    const active = this.activeFile === null ? undefined : this.ownerOf(this.activeFile);
    return sameSet(this.typesOf(this.ownerOf(path)), this.typesOf(active));
  }

  private typesOf(owner: TsProject | undefined): string[] {
    const installed = installedTypesFor(folderUriFor(this.configPathOf(owner)), this.typesByFolder);
    return typesFor(owner?.types, this.typesDefaultAll, installed);
  }

  /** A file's config, or a nominal one in the root: TypeScript resolves `types` from its folder. */
  private configPathOf(owner: TsProject | undefined): string {
    return owner?.path ?? `${this.root.replace(/[\\/]+$/, '')}/tsconfig.json`;
  }

  private ownerOf(path: string): TsProject | undefined {
    if (!this.owners.has(path)) {
      this.owners.set(path, projectFor(this.projects, path.replace(/\\/g, '/')));
    }
    return this.owners.get(path);
  }
}
