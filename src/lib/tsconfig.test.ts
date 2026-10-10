// @ts-nocheck
// tsconfig.ts resolution tests (node:test), against an in-memory file tree.
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { jsonc, normalize, resolveTsOptions } from './tsconfig';

function fsOf(files: Record<string, string>) {
  const readFile = async (path) => {
    if (!(path in files)) throw new Error(`ENOENT ${path}`);
    return files[path];
  };
  const listDir = async (dir) =>
    Object.keys(files)
      .filter((p) => p.startsWith(`${dir}/`) && !p.slice(dir.length + 1).includes('/'))
      .map((p) => p.slice(dir.length + 1));
  return { readFile, listDir };
}

const resolve = (files, root = '/p') => {
  const { readFile, listDir } = fsOf(files);
  return resolveTsOptions(root, readFile, listDir);
};

describe('resolveTsOptions', () => {
  it('follows a Vite-style solution config into its references', async () => {
    const options = await resolve({
      '/p/tsconfig.json': `{
        "files": [],
        "references": [{ "path": "./tsconfig.node.json" }, { "path": "./tsconfig.app.json" }]
      }`,
      '/p/tsconfig.app.json': `{
        "compilerOptions": {
          "jsx": "preserve", "jsxImportSource": "solid-js", // comment
          "paths": { "@/*": ["./src/*"] },
        },
        "include": ["src"]
      }`,
      '/p/tsconfig.node.json': `{
        "compilerOptions": { "jsx": "react-jsx", "types": ["node"] },
        "include": ["vite.config.ts"]
      }`,
    });
    assert.deepEqual(options, {
      baseUrl: undefined,
      jsx: 'preserve',
      jsxImportSource: 'solid-js',
      paths: { '@/*': ['/p/src/*'] },
    });
  });

  it('applies extends: relative, array and package, child wins', async () => {
    const options = await resolve({
      '/p/tsconfig.json': `{
        "extends": ["./configs/base", "@tsconfig/strictest"],
        "compilerOptions": { "jsx": "react-jsx" }
      }`,
      '/p/configs/base.json': `{
        "compilerOptions": { "jsx": "preserve", "baseUrl": "..", "paths": { "~/*": ["lib/*"] } }
      }`,
      '/p/node_modules/@tsconfig/strictest/tsconfig.json': `{
        "compilerOptions": { "jsxImportSource": "preact" }
      }`,
    });
    assert.equal(options.jsx, 'react-jsx');
    assert.equal(options.jsxImportSource, 'preact');
    assert.equal(options.baseUrl, '/p');
    assert.deepEqual(options.paths, { '~/*': ['/p/lib/*'] });
  });

  it('resolves ${configDir} against the extending config', async () => {
    const options = await resolve({
      '/p/tsconfig.json': `{ "extends": "./shared/tsconfig.json" }`,
      '/p/shared/tsconfig.json': `{ "compilerOptions": { "paths": { "#/*": ["\${configDir}/src/*"] } } }`,
    });
    assert.deepEqual(options.paths, { '#/*': ['/p/src/*'] });
  });

  it('follows folder references and merges paths from each', async () => {
    const options = await resolve({
      '/p/tsconfig.json': `{ "files": [], "references": [{ "path": "packages/a" }, { "path": "packages/b" }] }`,
      '/p/packages/a/tsconfig.json': `{ "compilerOptions": { "paths": { "a/*": ["src/*"] } } }`,
      '/p/packages/b/tsconfig.json': `{ "compilerOptions": { "paths": { "b/*": ["./lib/*"], "a/*": ["x"] } } }`,
    });
    assert.deepEqual(options.paths, {
      'a/*': ['/p/packages/a/src/*'],
      'b/*': ['/p/packages/b/lib/*'],
    });
  });

  it('survives reference and extends cycles', async () => {
    const options = await resolve({
      '/p/tsconfig.json': `{ "extends": "./tsconfig.json", "references": [{ "path": "./tsconfig.json" }], "compilerOptions": { "jsx": "react" } }`,
    });
    assert.equal(options.jsx, 'react');
  });

  it('falls back to tsconfig.*.json when there is no tsconfig.json', async () => {
    const options = await resolve({
      '/p/tsconfig.node.json': `{ "compilerOptions": { "jsx": "react" }, "include": ["vite.config.ts"] }`,
      '/p/tsconfig.app.json': `{ "compilerOptions": { "jsx": "preserve" }, "include": ["src"] }`,
    });
    assert.equal(options.jsx, 'preserve');
  });

  it('uses jsconfig.json, and ignores broken or missing configs', async () => {
    assert.equal(
      (await resolve({ '/p/jsconfig.json': `{ "compilerOptions": { "jsx": "React" } }` })).jsx,
      'react'
    );
    assert.deepEqual(await resolve({ '/p/tsconfig.json': `{ not json` }), {});
    assert.deepEqual(await resolve({}), {});
  });

  it('handles Windows roots', async () => {
    const options = await resolve(
      {
        'C:/p/tsconfig.json': `{ "references": [{ "path": ".\\\\tsconfig.app.json" }] }`,
        'C:/p/tsconfig.app.json': `{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }`,
      },
      'C:\\p\\'
    );
    assert.deepEqual(options.paths, { '@/*': ['C:/p/src/*'] });
  });
});

describe('helpers', () => {
  it('normalize', () => {
    assert.equal(normalize('/a/./b/../c/'), '/a/c');
    assert.equal(normalize('C:\\a\\..\\b'), 'C:/b');
    assert.equal(normalize('/..'), '/');
  });

  it('jsonc keeps comment-like text inside strings', () => {
    assert.deepEqual(JSON.parse(jsonc('{ "a": "//x", /* c */ "b": [1,], }')), { a: '//x', b: [1] });
  });
});
