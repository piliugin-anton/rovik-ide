// compilerOptions.ts
//
// Compiler options from the project's tsconfig.json / jsconfig.json, with
// `extends` and `references` followed (lib/tsconfig.ts), and the configs
// themselves as projects: which one owns a file decides its global @types
// (activeConfig.ts).
import * as monaco from 'monaco-editor';
import { readDirEntries, readFileContent } from '../commands';
import {
  moduleSettings,
  resolveTsProjects,
  type ReadFile,
  type ResolvedTsOptions,
  type TsProject,
} from '../tsconfig';
import { BASE_OPTIONS } from './defaults';
import { modelUriFor, pathPatternUriFor } from './uris';

type CompilerOptions = monaco.typescript.CompilerOptions;

export interface ProjectConfig {
  /** The options that matter for navigation, merged across the configs. */
  options: CompilerOptions;
  /** Every config found, in priority order. */
  projects: TsProject[];
  /** Whether an unset `types` means every @types package (before TS 6). */
  typesDefaultAll: boolean;
}

const JSX: Record<string, monaco.typescript.JsxEmit> = {
  preserve: monaco.typescript.JsxEmit.Preserve,
  react: monaco.typescript.JsxEmit.React,
  'react-native': monaco.typescript.JsxEmit.ReactNative,
  'react-jsx': monaco.typescript.JsxEmit.ReactJSX,
  'react-jsxdev': monaco.typescript.JsxEmit.ReactJSXDev,
};

/**
 * The project's configs: the root's, and `configPaths` found elsewhere in
 * it (workspace members'). `readFile` may serve already-loaded text;
 * anything else (an `extends` target in node_modules) comes from disk.
 */
export async function readProjectConfig(
  root: string,
  configPaths: readonly string[],
  readFile: ReadFile = readFileContent
): Promise<ProjectConfig> {
  const options: CompilerOptions = { ...BASE_OPTIONS };
  const listDir = async (dir: string) =>
    (await readDirEntries(root, dir)).filter((e) => !e.is_dir).map((e) => e.name);
  const [{ options: resolved, projects }, typesDefaultAll] = await Promise.all([
    resolveTsProjects(root, readFile, listDir, configPaths).catch(() => ({
      options: {} as ResolvedTsOptions,
      projects: [] as TsProject[],
    })),
    typesDefaultToAll(root),
  ]);
  // The project's way of resolving modules (package.json exports...).
  Object.assign(options, moduleSettings(resolved));
  if (resolved.baseUrl) options.baseUrl = modelUriFor(resolved.baseUrl).toString();
  if (resolved.paths && Object.keys(resolved.paths).length > 0) {
    // Substitutions are absolute, so mappings from configs in different
    // folders (tsconfig.app.json in the root, packages/x/tsconfig.json...)
    // all hold under the one baseUrl the worker gets.
    const paths: Record<string, string[]> = {};
    for (const [key, subs] of Object.entries(resolved.paths))
      paths[key] = subs.map(pathPatternUriFor);
    options.paths = paths;
  }
  if (resolved.jsx && JSX[resolved.jsx] !== undefined) options.jsx = JSX[resolved.jsx];
  if (resolved.jsxImportSource) options.jsxImportSource = resolved.jsxImportSource;
  return { options, projects, typesDefaultAll };
}

/**
 * Whether an unset `types` means every @types package: true before
 * TypeScript 6, which made it mean none. Follows the project's installed
 * compiler; without one, the editor's own (5.x).
 */
async function typesDefaultToAll(root: string): Promise<boolean> {
  try {
    const pkg = JSON.parse(
      await readFileContent(`${root.replace(/[\\/]+$/, '')}/node_modules/typescript/package.json`)
    ) as { version?: unknown };
    return !(parseInt(String(pkg.version), 10) >= 6);
  } catch {
    return true;
  }
}

/**
 * A plain-JSON copy for the worker handoff: a non-string `baseUrl`,
 * `jsxImportSource` or `configFilePath`, a `paths` entry that isn't a
 * string array, or a non-string `types` entry would make the structured
 * clone throw DataCloneError.
 */
export function sanitizeCompilerOptions(options: CompilerOptions): CompilerOptions {
  const clean: CompilerOptions = { ...options };
  for (const key of ['baseUrl', 'jsxImportSource', 'configFilePath'] as const) {
    if (clean[key] !== undefined && typeof clean[key] !== 'string') delete clean[key];
  }
  if (clean.paths !== undefined) {
    if (clean.paths && typeof clean.paths === 'object') {
      const paths: Record<string, string[]> = {};
      for (const [k, v] of Object.entries(clean.paths)) {
        const strs = Array.isArray(v) ? v.filter((e): e is string => typeof e === 'string') : [];
        if (strs.length > 0) paths[k] = strs;
      }
      clean.paths = paths;
    } else {
      delete clean.paths;
    }
  }
  if (clean.types !== undefined) {
    clean.types = Array.isArray(clean.types)
      ? clean.types.filter((e): e is string => typeof e === 'string')
      : [];
  }
  return clean;
}
