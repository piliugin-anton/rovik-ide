// Runs the frontend unit tests (src/**/*.test.ts) with node:test.
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
    b.onResolve({ filter: /^monaco-editor(\/|$)|\?worker$/ }, (args) => ({
      path: args.path,
      namespace: 'stub',
    }));
    b.onLoad({ filter: /.*/, namespace: 'stub' }, () => ({
      contents:
        'const stub = new Proxy(function () {}, { get: (_, k) => (k === "__esModule" ? false : stub), apply: () => stub, construct: () => stub });\nmodule.exports = stub;',
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
