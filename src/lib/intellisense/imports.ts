// imports.ts
//
// What a file names of other packages, read from its text without
// parsing, as the backend reads it (rovik_core::typings::imports): the
// bare specifiers of its imports, and the packages that may hold each
// one's declarations. Pure, so the worker could share it; nothing here may
// import monaco-editor.

const IMPORT =
  /\bfrom\s*['"]([^'"\n]+)['"]|\bimport\s*\(\s*['"]([^'"\n]+)['"]|\brequire\s*\(\s*['"]([^'"\n]+)['"]|\bimport\s+['"]([^'"\n]+)['"]|<reference\s+types\s*=\s*['"]([^'"\n]+)['"]/g;

/** The bare specifiers (packages, not paths) a file imports, each once, in order. */
export function bareSpecifiers(text: string): string[] {
  const out: string[] = [];
  for (const match of text.matchAll(IMPORT)) {
    const spec = match[1] ?? match[2] ?? match[3] ?? match[4] ?? match[5];
    if (spec !== undefined && isBare(spec) && !out.includes(spec)) out.push(spec);
  }
  return out;
}

/** A specifier that names a package: not a path, a URL, a virtual module or a `#` alias. */
export function isBare(spec: string): boolean {
  return !(
    spec.length === 0 ||
    /^[./#\\\0]/.test(spec) ||
    spec.includes('://') ||
    spec.startsWith('virtual:') ||
    spec.includes('*') ||
    /^[A-Za-z]:[/\\]/.test(spec)
  );
}

/** `@scope/name` for `@scope/name/x`, `name` for `name/x`; null for an alias like `@/x`. */
export function packageNameOf(spec: string): string | null {
  const parts = spec.split('/');
  if (parts[0] === '') return null;
  if (parts[0].startsWith('@')) {
    if (parts[0].length === 1 || !parts[1]) return null;
    return `${parts[0]}/${parts[1]}`;
  }
  return parts[0];
}

/**
 * The packages that may hold a specifier's declarations: the package
 * itself, then its `@types` (`@types/scope__name` for a scoped one). A Node
 * built-in (`node:fs`, and `fs` itself) is typed by `@types/node`. Empty
 * for a specifier no package can be found for.
 */
export function candidatePackages(spec: string): string[] {
  if (!isBare(spec)) return [];
  if (spec.startsWith('node:')) return spec.length > 'node:'.length ? ['@types/node'] : [];
  const name = packageNameOf(spec);
  if (name === null) return [];
  if (NODE_BUILTINS.has(name)) return [name, '@types/node'];
  if (name.startsWith('@types/')) return [name];
  const types = name.startsWith('@')
    ? `@types/${name.slice(1).replace('/', '__')}`
    : `@types/${name}`;
  return [name, types];
}

const NODE_BUILTINS = new Set([
  'assert',
  'async_hooks',
  'buffer',
  'child_process',
  'cluster',
  'console',
  'constants',
  'crypto',
  'dgram',
  'diagnostics_channel',
  'dns',
  'domain',
  'events',
  'fs',
  'http',
  'http2',
  'https',
  'inspector',
  'module',
  'net',
  'os',
  'path',
  'perf_hooks',
  'process',
  'punycode',
  'querystring',
  'readline',
  'repl',
  'stream',
  'string_decoder',
  'sys',
  'timers',
  'tls',
  'trace_events',
  'tty',
  'url',
  'util',
  'v8',
  'vm',
  'wasi',
  'worker_threads',
  'zlib',
]);
