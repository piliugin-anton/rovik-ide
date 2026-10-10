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
//   - some projects have no tsconfig.json at all, only tsconfig.*.json.
//
// Monaco has one program for the whole project, so the configs are merged
// into one set of options: the root's own first, then its references (app
// configs before tooling ones like tsconfig.node.json, which only cover
// vite.config.ts and the like), first value wins. `paths` are merged across
// all of them, each mapping made absolute against the config that declared
// it, so mappings from configs in different folders all stay correct.
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
  include?: unknown;
  files?: unknown;
  references: string[];
}

const ROOT_CONFIGS = ['tsconfig.json', 'jsconfig.json'];
/** Configs and references nest a few levels at most; this stops cycles too. */
const MAX_DEPTH = 16;

export async function resolveTsOptions(
  root: string,
  readFile: ReadFile,
  listDir?: ListDir
): Promise<ResolvedTsOptions> {
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
  if (!found && listDir) {
    // No tsconfig.json: take every tsconfig.*.json in the root.
    const names = await listDir(rootDir).catch(() => [] as string[]);
    const extra = names.filter((n) => /^(tsconfig|jsconfig)\..+\.json$/i.test(n)).sort();
    const loaded: LoadedConfig[] = [];
    for (const name of extra) {
      const before = configs.length;
      await visit(join(rootDir, name), 0);
      loaded.push(...configs.splice(before));
    }
    loaded.sort((a, b) => Number(isToolingConfig(a)) - Number(isToolingConfig(b)));
    configs.push(...loaded);
  }

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
  return merged;
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
      if (base.include !== undefined) result.include = base.include;
      if (base.files !== undefined) result.files = base.files;
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

  if (json.include !== undefined) result.include = json.include;
  if (json.files !== undefined) result.files = json.files;
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
