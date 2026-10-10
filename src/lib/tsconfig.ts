// tsconfig.ts
//
// Which compiler options the project's TypeScript config sets, resolved the
// way tsc resolves them. tsconfig.json alone isn't enough:
//
//   - a "solution" tsconfig.json (`"files": []` + `references`, what Vite's
//     templates generate) sets nothing itself — the options live in the
//     referenced tsconfig.app.json / tsconfig.node.json;
//   - `extends` (a path, a package like `@tsconfig/node20`, or an array of
//     them) supplies options the file doesn't repeat;
//   - some projects have no tsconfig.json at all, only tsconfig.*.json, or
//     have unreferenced ones next to it (tests, tooling: `tsc -p`).
//
// Monaco has one program for the whole project, so the configs are merged
// into one set of options: the root's own first, then its references (app
// configs before tooling ones like tsconfig.node.json, which only cover
// vite.config.ts and the like), first value wins. `paths` are merged across
// all of them, each mapping made absolute against the config that declared
// it, so mappings from configs in different folders all stay correct.
//
// Each config is also kept as a project (its file patterns and `types`),
// so the editor can tell which config owns a file and give that file only
// the global @types its config allows (see projectFor / visibleTypes).
//
// Pure (file access is injected) so it can be unit-tested without Tauri.

/** Reads a file; throws (or rejects) when it doesn't exist. */
export type ReadFile = (path: string) => Promise<string>;
/** File names directly in a folder; used only when no tsconfig.json exists. */
export type ListDir = (path: string) => Promise<string[]>;

/** Options that matter for navigation, with absolute (forward-slash) paths. */
export interface ResolvedTsOptions {
  baseUrl?: string;
  /** Substitutions are absolute paths. */
  paths?: Record<string, string[]>;
  jsx?: string;
  jsxImportSource?: string;
}

/** One config file with its `extends` chain applied. */
interface LoadedConfig {
  path: string;
  options: ResolvedTsOptions;
  /** Absolute, each against the config file that declared it. */
  include?: string[];
  files?: string[];
  exclude?: string[];
  /** compilerOptions.types; undefined when unset. */
  types?: string[];
  references: string[];
}

/** Which files a config covers, and the global @types it lets them see. */
export interface TsProject {
  path: string;
  /** Absolute patterns. */
  include: string[];
  files: string[];
  exclude: string[];
  /** compilerOptions.types; undefined when unset (the compiler's default). */
  types?: string[];
}

const ROOT_CONFIGS = ['tsconfig.json', 'jsconfig.json'];
/** Configs and references nest a few levels at most; this stops cycles too. */
const MAX_DEPTH = 16;

export async function resolveTsOptions(
  root: string,
  readFile: ReadFile,
  listDir?: ListDir
): Promise<ResolvedTsOptions> {
  return (await resolveTsProjects(root, readFile, listDir)).options;
}

/** The merged options, and every config found as a project, in priority order. */
export async function resolveTsProjects(
  root: string,
  readFile: ReadFile,
  listDir?: ListDir
): Promise<{ options: ResolvedTsOptions; projects: TsProject[] }> {
  const rootDir = normalize(root);
  const configs: LoadedConfig[] = [];
  const seen = new Set<string>();

  const visit = async (path: string, depth: number): Promise<boolean> => {
    if (seen.has(path) || depth > MAX_DEPTH) return seen.has(path);
    const config = await loadConfig(path, readFile);
    if (!config) return false;
    seen.add(path);
    configs.push(config);
    const refs: LoadedConfig[] = [];
    for (const ref of config.references) {
      const before = configs.length;
      for (const candidate of referenceCandidates(ref)) {
        if (await visit(candidate, depth + 1)) break;
      }
      refs.push(...configs.splice(before));
    }
    // App configs before tooling configs; stable otherwise.
    refs.sort((a, b) => Number(isToolingConfig(a)) - Number(isToolingConfig(b)));
    configs.push(...refs);
    return true;
  };

  let found = false;
  for (const name of ROOT_CONFIGS) {
    if (await visit(join(rootDir, name), 0)) {
      found = true;
      break;
    }
  }
  // The root's tsconfig.*.json that nothing references (tsconfig.test.json,
  // tsconfig.node.json run with `tsc -p`): with no tsconfig.json they are
  // the project's configs; next to one they only claim the files it
  // doesn't cover (projectFor) and leave the merged options alone.
  const siblings: LoadedConfig[] = [];
  if (listDir) {
    const names = await listDir(rootDir).catch(() => [] as string[]);
    const extra = names.filter((n) => /^(tsconfig|jsconfig)\..+\.json$/i.test(n)).sort();
    for (const name of extra) {
      const before = configs.length;
      await visit(join(rootDir, name), 0);
      siblings.push(...configs.splice(before));
    }
    siblings.sort((a, b) => Number(isToolingConfig(a)) - Number(isToolingConfig(b)));
  }
  if (!found) configs.push(...siblings.splice(0));

  const merged: ResolvedTsOptions = {};
  for (const { options } of configs) {
    merged.baseUrl ??= options.baseUrl;
    merged.jsx ??= options.jsx;
    merged.jsxImportSource ??= options.jsxImportSource;
    if (options.paths) {
      merged.paths ??= {};
      for (const [key, subs] of Object.entries(options.paths)) merged.paths[key] ??= subs;
    }
  }
  return { options: merged, projects: [...configs, ...siblings].map(toProject) };
}

function toProject(config: LoadedConfig): TsProject {
  const dir = dirname(config.path);
  // tsc: no include and no files → everything; files only → no patterns.
  const include =
    config.include ?? (config.files === undefined ? [join(dir, '**/*')] : ([] as string[]));
  const exclude =
    config.exclude ??
    ['node_modules', 'bower_components', 'jspm_packages'].map((d) => join(dir, d));
  return { path: config.path, include, files: config.files ?? [], exclude, types: config.types };
}

/**
 * The first project whose files include `filePath` (absolute), like
 * tsserver picking the configured project for an open file; undefined when
 * none does (tsserver then infers a project with the default types).
 */
export function projectFor(projects: TsProject[], filePath: string): TsProject | undefined {
  const file = normalize(filePath);
  return projects.find(
    (p) =>
      p.files.some((f) => samePath(f, file)) ||
      (p.include.some((pattern) => matchesPattern(pattern, file)) &&
        !p.exclude.some((pattern) => matchesPattern(pattern, file)))
  );
}

/**
 * The @types packages (folder names under node_modules/@types) a program
 * with these `types` sees as globals, or 'all'. Unset `types` means every
 * @types package before TypeScript 6 and none from 6 on. A visible package's
 * own `/// <reference types="..." />` pulls that one in too, as in tsc.
 */
export function visibleTypes(
  types: string[] | undefined,
  defaultsToAll: boolean,
  referencesOf: (typesPackage: string) => string[]
): Set<string> | 'all' {
  if (types === undefined && defaultsToAll) return 'all';
  if (types?.includes('*')) return 'all';
  const visible = new Set<string>();
  const queue = (types ?? []).map(typesPackageName);
  while (queue.length > 0) {
    const name = queue.pop()!;
    if (visible.has(name)) continue;
    visible.add(name);
    queue.push(...referencesOf(name).map(typesPackageName));
  }
  return visible;
}

/** `node` → `node`, `@scope/pkg` → `scope__pkg`, `jest/globals` → `jest`. */
export function typesPackageName(reference: string): string {
  const parts = reference.replace(/^@types\//, '').split('/');
  return parts[0].startsWith('@') && parts.length > 1
    ? `${parts[0].slice(1)}__${parts[1]}`
    : parts[0];
}

/** `/// <reference types="x" />` directives in a declaration file. */
export function typeReferences(text: string): string[] {
  return [...text.matchAll(/^\s*\/\/\/\s*<reference\s+types\s*=\s*["']([^"']+)["']/gm)].map(
    (m) => m[1]
  );
}

/**
 * tsconfig include/exclude semantics: `*` and `?` stay within a folder,
 * `**` spans folders, and a pattern whose last part has no wildcard also
 * matches everything under it (a folder).
 */
function matchesPattern(pattern: string, file: string): boolean {
  if (!/[*?]/.test(pattern)) {
    return samePath(pattern, file) || startsWithPath(file, `${pattern}/`);
  }
  let re = '';
  const parts = pattern.split('/');
  parts.forEach((part, i) => {
    const sep = i < parts.length - 1 ? '/' : '';
    if (part === '**') {
      re += i < parts.length - 1 ? '(?:[^/]+/)*' : '.*';
      return;
    }
    re +=
      part
        .replace(/[.+^${}()|[\]\\]/g, '\\$&')
        .replace(/\*/g, '[^/]*')
        .replace(/\?/g, '[^/]') + sep;
  });
  return new RegExp(`^${re}$`, caseInsensitive(file) ? 'i' : '').test(file);
}

/** Windows paths (drive letter) compare case-insensitively. */
function caseInsensitive(path: string): boolean {
  return /^[A-Za-z]:\//.test(path);
}

function samePath(a: string, b: string): boolean {
  return caseInsensitive(b) ? a.toLowerCase() === b.toLowerCase() : a === b;
}

function startsWithPath(path: string, prefix: string): boolean {
  return caseInsensitive(path)
    ? path.toLowerCase().startsWith(prefix.toLowerCase())
    : path.startsWith(prefix);
}

/**
 * A config with its `extends` chain applied: compilerOptions merge shallowly
 * (the extending file wins), include/files are inherited, references are not.
 * `baseUrl` and `paths` resolve against the file that declares them.
 */
async function loadConfig(
  path: string,
  readFile: ReadFile,
  leafDir = dirname(path),
  depth = 0
): Promise<LoadedConfig | null> {
  if (depth > MAX_DEPTH) return null;
  let json: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(jsonc(await readFile(path)));
    if (!isObject(parsed)) return null;
    json = parsed;
  } catch {
    return null;
  }
  const dir = dirname(path);

  // Bases first, in order, so later ones (and this file) override them.
  const result: LoadedConfig = { path, options: {}, references: [] };
  const bases =
    typeof json.extends === 'string'
      ? [json.extends]
      : Array.isArray(json.extends)
        ? json.extends.filter((e): e is string => typeof e === 'string')
        : [];
  for (const spec of bases) {
    for (const candidate of extendsCandidates(spec, dir)) {
      const base = await loadConfig(candidate, readFile, leafDir, depth + 1);
      if (!base) continue;
      Object.assign(result.options, definedOnly(base.options));
      if (base.types !== undefined) result.types = base.types;
      for (const key of ['include', 'files', 'exclude'] as const) {
        if (base[key] !== undefined) result[key] = base[key];
      }
      break;
    }
  }

  const co = isObject(json.compilerOptions) ? json.compilerOptions : {};
  const resolve = (value: string) =>
    value.startsWith('${configDir}')
      ? join(leafDir, value.slice('${configDir}'.length).replace(/^[\\/]+/, ''))
      : join(dir, value);
  // baseUrl from this file or a base; paths without a baseUrl are relative
  // to the file declaring them (TS 4.1+).
  if (typeof co.baseUrl === 'string') result.options.baseUrl = resolve(co.baseUrl);
  if (isObject(co.paths)) {
    const base = result.options.baseUrl ?? dir;
    const paths: Record<string, string[]> = {};
    for (const [key, value] of Object.entries(co.paths)) {
      if (!Array.isArray(value)) continue;
      const subs = value
        .filter((s): s is string => typeof s === 'string')
        .map((s) =>
          s.startsWith('${configDir}') ? resolve(s) : isAbsolute(s) ? normalize(s) : join(base, s)
        );
      if (subs.length > 0) paths[key] = subs;
    }
    result.options.paths = paths;
  }
  if (typeof co.jsx === 'string') result.options.jsx = co.jsx.toLowerCase();
  if (typeof co.jsxImportSource === 'string') result.options.jsxImportSource = co.jsxImportSource;

  if (Array.isArray(co.types)) {
    result.types = co.types.filter((t): t is string => typeof t === 'string');
  }
  for (const key of ['include', 'files', 'exclude'] as const) {
    const list = json[key];
    if (Array.isArray(list)) {
      result[key] = list.filter((e): e is string => typeof e === 'string').map((e) => resolve(e));
    }
  }
  if (Array.isArray(json.references)) {
    for (const ref of json.references) {
      if (isObject(ref) && typeof ref.path === 'string')
        result.references.push(join(dir, ref.path));
    }
  }
  return result;
}

/** A reference names a config file or a folder holding tsconfig.json. */
function referenceCandidates(path: string): string[] {
  return /\.json$/i.test(path) ? [path] : [join(path, 'tsconfig.json'), path];
}

/** Where tsc looks for an `extends` target. */
function extendsCandidates(spec: string, fromDir: string): string[] {
  const withJson = (p: string) => (/\.json$/i.test(p) ? [p] : [p, `${p}.json`]);
  if (spec.startsWith('.') || isAbsolute(spec)) return withJson(join(fromDir, spec));
  // A package: node_modules folders from this config's folder up.
  const out: string[] = [];
  for (let dir = fromDir; ;) {
    const pkg = join(dir, `node_modules/${spec}`);
    out.push(...withJson(pkg), join(pkg, 'tsconfig.json'));
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return out;
}

/**
 * tsconfig.node.json and friends: configs for the build tooling
 * (vite.config.ts...), not the app. Their options go last.
 */
function isToolingConfig(config: LoadedConfig): boolean {
  if (/[\\/](tsconfig|jsconfig)\.node\.json$/i.test(config.path)) return true;
  const entries = [config.include, config.files]
    .filter(Array.isArray)
    .flat()
    .filter((e): e is string => typeof e === 'string');
  return entries.length > 0 && entries.every((e) => /(^|\/)[^/]*\.config\.[cm]?[jt]s$/i.test(e));
}

/** tsconfig files allow comments and trailing commas; JSON.parse doesn't. */
export function jsonc(text: string): string {
  let out = '';
  let inString = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inString) {
      out += ch;
      if (ch === '\\') out += text[++i] ?? '';
      else if (ch === '"') inString = false;
    } else if (ch === '"') {
      inString = true;
      out += ch;
    } else if (ch === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i++;
      out += '\n';
    } else if (ch === '/' && text[i + 1] === '*') {
      i += 2;
      while (i < text.length && !(text[i] === '*' && text[i + 1] === '/')) i++;
      i++;
    } else {
      out += ch;
    }
  }
  return out.replace(/,(\s*[}\]])/g, '$1');
}

/* ---------------------------------------------------------------------------
 * Paths: forward slashes throughout, Windows drive letters kept.
 * ------------------------------------------------------------------------- */

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function definedOnly<T extends object>(o: T): Partial<T> {
  return Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as Partial<T>;
}

function isAbsolute(p: string): boolean {
  return /^([\\/]|[A-Za-z]:[\\/])/.test(p);
}

export function normalize(p: string): string {
  const slashed = p.replace(/\\/g, '/');
  const prefix = /^[A-Za-z]:\//.test(slashed)
    ? slashed.slice(0, 3)
    : slashed.startsWith('/')
      ? '/'
      : '';
  const parts: string[] = [];
  for (const part of slashed.slice(prefix.length).split('/')) {
    if (part === '' || part === '.') continue;
    if (part === '..' && parts.length > 0 && parts[parts.length - 1] !== '..') parts.pop();
    else if (part !== '..' || !prefix) parts.push(part);
  }
  return prefix + parts.join('/') || '.';
}

function join(dir: string, rel: string): string {
  return isAbsolute(rel) ? normalize(rel) : normalize(`${dir}/${rel}`);
}

function dirname(p: string): string {
  const n = normalize(p);
  const i = n.lastIndexOf('/');
  if (i < 0) return '.';
  if (i === 0) return '/';
  if (/^[A-Za-z]:$/.test(n.slice(0, i))) return n.slice(0, i + 1);
  return n.slice(0, i);
}
