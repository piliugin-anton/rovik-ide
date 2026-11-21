// Example protected node:test oracle. Adapt the import, props, and expectation to
// the saved plan. The harness runs this file from /oracle with /build/project as cwd.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import test from 'node:test';
import { pathToFileURL } from 'node:url';

const root = process.cwd();
const require = createRequire(join(root, 'package.json'));
const esbuild = require('esbuild');
const babel = require('@babel/core');
const solid = require.resolve('babel-preset-solid');
const { Window } = await import(pathToFileURL(require.resolve('happy-dom')).href);

const solidJsx = {
  name: 'protected-solid-dom',
  setup(build) {
    build.onLoad({ filter: /\.tsx$/ }, async ({ path }) => {
      const source = await readFile(path, 'utf8');
      const stripped = await esbuild.transform(source, { loader: 'tsx', jsx: 'preserve' });
      const output = await babel.transformAsync(stripped.code, {
        filename: path,
        configFile: false,
        babelrc: false,
        presets: [[solid, { generate: 'dom' }]],
      });
      return { contents: output.code, loader: 'js' };
    });
  },
};

test('the badge renders its supplied label', async () => {
  const window = new Window();
  globalThis.window = window;
  globalThis.document = window.document;
  try {
    const bundle = await esbuild.build({
      absWorkingDir: root,
      stdin: {
        resolveDir: root,
        contents: `import { createComponent } from 'solid-js';
          import { render } from 'solid-js/web';
          import Badge from './src/Badge.tsx';
          export const mount = root => render(() => createComponent(Badge, { label: 'Ready' }), root);`,
      },
      bundle: true,
      platform: 'node',
      format: 'esm',
      conditions: ['browser'],
      plugins: [solidJsx],
      write: false,
    });
    const bytes = Buffer.from(bundle.outputFiles[0].contents);
    const { mount } = await import('data:text/javascript;base64,' + bytes.toString('base64'));
    const element = document.createElement('div');
    const dispose = mount(element);
    try {
      assert.equal(element.textContent, 'Ready');
    } finally {
      dispose();
    }
  } finally {
    await window.happyDOM.abort();
  }
});
