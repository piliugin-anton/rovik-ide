// activeConfig.ts
//
// The config owning the file shown in the editor decides what the worker's
// one program is, as it would be tsserver's for that file:
//
//   - its files: the sources the config's `include`/`files`/`exclude` take
//     in (the config travels to the worker as PROJECT_LIB). A workspace
//     member's program doesn't hold another member's files, whose own
//     @types/react would otherwise declare the global JSX namespace a
//     second time ("Element is not assignable to ReactNode");
//   - its global @types: the packages its `types` names, or every package
//     in node_modules/@types when unset (before TypeScript 6; none from 6
//     on), as compilerOptions.types. TypeScript resolves the names itself
//     from the folder of compilerOptions.configFilePath, so `process` is
//     typed in vite.config.ts and not in browser code. What the code
//     imports resolves on its own whatever `types` says, since the worker
//     can read any dependency file, as tsc can read node_modules.
//
// Switching between files of one config costs nothing; a file of another
// config applies its options, which restarts the worker only when they
// differ. Other open files are outside the program meanwhile; covers()
// tells the diagnostics reporter which markers to trust (lib/diagnostics.ts).
import type * as monaco from 'monaco-editor';
import { projectFor, typesFor, type TsProject } from '../tsconfig';
import { workerLibs } from './libs';
import { PROJECT_LIB, SOURCE_FILE } from './paths';
import { sanitizeCompilerOptions, type ProjectConfig } from './compilerOptions';
import { applyCompilerOptions, BASE_OPTIONS } from './defaults';
import { installedTypesFor, typesPackagesByFolder } from './typeRoots';
import { folderUriFor, modelUriFor } from './uris';

export class ActiveConfig {
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

  /** Gives the worker the active file's config: its options and its files. */
  apply(): void {
    const owner = this.activeFile === null ? undefined : this.ownerOf(this.activeFile);
    // Options first: a change restarts the worker, which then starts with
    // the lib set as it is after the second call.
    applyCompilerOptions(this.optionsFor(this.activeFile));
    workerLibs.set(PROJECT_LIB, JSON.stringify(owner ?? null));
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
   * Whether the worker's markers for `path` are current: its config is the
   * active file's, so it is in the program (false while another config's
   * file is active).
   */
  covers(path: string): boolean {
    // Other files (Markdown, Rust...) aren't validated by the worker.
    if (!SOURCE_FILE.test(path)) return true;
    const active = this.activeFile === null ? undefined : this.ownerOf(this.activeFile);
    return this.ownerOf(path) === active;
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
