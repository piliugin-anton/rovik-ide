// Runs the frontend tests (src/**/*.test.ts, *.test.tsx) with node:test;
// `npm test` typechecks them first (tsconfig.test.json).
//
// Each test file is bundled with esbuild first: the sources use
// extensionless imports, which node can't resolve on its own, and the
// browser build of solid-js is used (the `browser` condition), so stores
// behave as they do in the app rather than as the server build does.
// Component tests (*.test.tsx) get Solid's JSX compiled as the app's build
// does (babel-preset-solid), and a DOM from happy-dom (src/test/dom.ts).
import { build, transform } from 'esbuild';
import { transformAsync } from '@babel/core';
import solid from 'babel-preset-solid';
import { readFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';

function testFiles(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return testFiles(path);
    return /\.test\.tsx?$/.test(name) ? [path] : [];
  });
}

/** .tsx: types stripped by esbuild, then the JSX compiled by Solid's preset. */
const solidJsx = {
  name: 'solid-jsx',
  setup(b) {
    b.onLoad({ filter: /\.tsx$/ }, async ({ path }) => {
      const source = await readFile(path, 'utf8');
      const stripped = await transform(source, {
        loader: 'tsx',
        jsx: 'preserve',
        sourcefile: path,
      });
      const out = await transformAsync(stripped.code, {
        filename: path,
        presets: [[solid, { generate: 'dom' }]],
        babelrc: false,
        configFile: false,
      });
      return { contents: out.code, loader: 'js' };
    });
  },
};

/**
 * What can't run in Node, for component tests: Monaco (and its workers)
 * is a stub any property of which is a no-op; stylesheets and fonts are
 * empty. The app's own code is bundled as it is.
 */
const browserOnly = {
  name: 'browser-only',
  setup(b) {
    b.onResolve({ filter: /^monaco-editor(\/|$)|\?worker$/ }, (args) => {
      // Monaco's TypeScript worker is TypeScript's own services, which run
      // in Node as they do: the project worker's test runs on the real one.
      if (args.path === 'monaco-editor/language/typescript/ts.worker.js') return undefined;
      return { path: args.path, namespace: 'stub' };
    });
    b.onLoad({ filter: /.*/, namespace: 'stub' }, () => ({
      // A property of the stub is the stub, however deep: `typescript`,
      // `languages` and the rest are read as they are when a module loads
      // (defaults.ts reads its defaults then), and any call is a no-op.
      // Node's ESM interop copies a CommonJS module's own keys, so the
      // namespaces a module reads at load are in `exports` and `ownKeys`.
      //
      // Two are real, because what's built on them is plain JavaScript:
      // a URI (uris.ts keys files by its text) and the editor's list of
      // models (an LSP client opens what is open, and finds nothing).
      contents: `
const disposable = { dispose: () => {} };
const makeUri = (text) => ({
  scheme: 'file',
  path: text.replace(/^file:\\/\\//, ''),
  fsPath: text.replace(/^file:\\/\\//, ''),
  toString: () => text,
  with: (change) => makeUri(change && change.path ? 'file://' + change.path : text),
});
const Uri = {
  parse: (text) => makeUri(text),
  file: (path) => makeUri('file://' + String(path).replace(/\\\\/g, '/')),
  joinPath: (base, ...parts) =>
    makeUri(String(base).replace(/\\/+$/, '') + '/' + parts.join('/')),
};
const editor = new Proxy(
  {},
  {
    get: (target, key) =>
      key === 'getModels' ? () => [] : key === 'onDidCreateModel' ? () => disposable : stub,
  }
);
const exposed = { Uri, editor };
const names = [
  ...Object.keys(exposed),
  'typescript',
  'languages',
  'Range',
  'Position',
  'Selection',
  'KeyCode',
  'KeyMod',
  'MarkerSeverity',
  'MarkerTag',
];
const stub = new Proxy(function () {}, {
  get: (target, key) => (key === '__esModule' ? false : key in exposed ? exposed[key] : stub),
  apply: () => stub,
  construct: () => stub,
  getOwnPropertyDescriptor: (target, key) =>
    Object.getOwnPropertyDescriptor(target, key) ?? {
      value: key in exposed ? exposed[key] : stub,
      writable: true,
      enumerable: true,
      configurable: true,
    },
  ownKeys: (target) => [...Reflect.ownKeys(target), ...names],
});
module.exports = stub;
`,
      loader: 'js',
    }));
  },
};

const files = testFiles('src');
const out = mkdtempSync(join(tmpdir(), 'rovik-tests-'));
try {
  const bundles = [];
  for (const file of files) {
    const outfile = join(
      out,
      relative('src', file)
        .replace(/[\\/]/g, '__')
        .replace(/\.tsx?$/, '.mjs')
    );
    await build({
      entryPoints: [file],
      outfile,
      bundle: true,
      platform: 'node',
      format: 'esm',
      conditions: ['browser'],
      logLevel: 'error',
      plugins: [solidJsx, browserOnly],
      // Vite's `import.meta.glob` (lib/monacoThemes.ts: the community
      // themes' JSON, loaded lazily) isn't Node's: here it finds no file,
      // which the loader takes as "not there" and falls back on vs-dark.
      banner: { js: 'import.meta.glob = () => ({});' },
      loader: {
        '.css': 'empty',
        '.ttf': 'empty',
        '.woff': 'empty',
        '.woff2': 'empty',
        '.svg': 'empty',
      },
    });
    bundles.push(outfile);
  }
  const run = spawnSync(process.execPath, ['--test', ...bundles], { stdio: 'inherit' });
  process.exitCode = run.status ?? 1;
} finally {
  rmSync(out, { recursive: true, force: true });
}
