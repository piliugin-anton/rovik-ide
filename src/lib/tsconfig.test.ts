// @ts-nocheck
// tsconfig.ts resolution tests (node:test), against an in-memory file tree.
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  jsonc,
  moduleSettings,
  normalize,
  projectFor,
  resolveTsOptions,
  resolveTsProjects,
  typesFor,
} from './tsconfig';

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

const fsOfFiles = (files) => {
  const { readFile, listDir } = fsOf(files);
  return { readFile, listDir };
};

const resolve = (files, root = '/p') => {
  const { readFile, listDir } = fsOf(files);
  return resolveTsOptions(root, readFile, listDir);
};

describe('resolveTsOptions', () => {
  it('reads shared base configs and missing candidates once per load, then refreshes them', async () => {
    const files = {
      '/p/tsconfig.json': '{"files":[],"references":[{"path":"./app"},{"path":"./tools"}]}',
      '/p/app/tsconfig.json': '{"extends":"../base"}',
      '/p/tools/tsconfig.json': '{"extends":"../base"}',
      '/p/base.json': '{"include":["${configDir}/src"],"compilerOptions":{"jsx":"preserve"}}',
    };
    const reads = new Map<string, number>();
    const { readFile } = fsOf(files);
    const read = async (path: string) => {
      reads.set(path, (reads.get(path) ?? 0) + 1);
      return readFile(path);
    };
    const result = await resolveTsProjects('/p', read);
    assert.equal(reads.get('/p/base'), 1, 'failed probes are shared too');
    assert.equal(reads.get('/p/base.json'), 1);
    assert.deepEqual(
      result.projects.slice(1).map((p) => p.include),
      [['/p/app/src'], ['/p/tools/src']]
    );
    files['/p/base.json'] = '{"compilerOptions":{"jsx":"react-jsx"}}';
    assert.equal((await resolveTsProjects('/p', read)).options.jsx, 'react-jsx');
  });

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
      module: undefined,
      moduleResolution: undefined,
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

describe('projects', () => {
  const files = {
    '/p/tsconfig.json': `{
      "compilerOptions": { "types": ["vite/client"] },
      "include": ["src"],
      "exclude": ["src/**/*.test.ts"]
    }`,
    '/p/tsconfig.test.json': `{
      "extends": "./tsconfig.json",
      "compilerOptions": { "types": ["node", "vite/client"] },
      "include": ["src/**/*.test.ts"],
      "exclude": []
    }`,
    '/p/tsconfig.node.json': `{
      "compilerOptions": { "types": ["node"], "paths": { "x": ["y"] } },
      "files": ["vite.config.ts"]
    }`,
  };

  it('picks the config that owns a file, with its types', async () => {
    const { options } = await resolveTsProjects('/p', ...Object.values(fsOfFiles(files)));
    assert.equal(options.paths, undefined, 'unreferenced configs leave the options alone');
    const { projects } = await resolveTsProjects('/p', ...Object.values(fsOfFiles(files)));
    const owner = (f) => projectFor(projects, f)?.path;
    // tsconfig.json first; the unreferenced ones claim what it doesn't cover.
    assert.equal(owner('/p/src/App.tsx'), '/p/tsconfig.json');
    assert.equal(owner('/p/src/lib/a.test.ts'), '/p/tsconfig.test.json');
    assert.equal(owner('/p/vite.config.ts'), '/p/tsconfig.node.json');
    assert.equal(owner('/p/scripts/x.ts'), undefined);
    assert.deepEqual(projectFor(projects, '/p/src/App.tsx').types, ['vite/client']);
  });

  it('follows references and inherits patterns through extends', async () => {
    const withRefs = {
      ...files,
      '/p/tsconfig.json': `{ "files": [], "references": [
        { "path": "./tsconfig.app.json" }, { "path": "./tsconfig.test.json" }, { "path": "./tsconfig.node.json" }] }`,
      '/p/tsconfig.app.json': files['/p/tsconfig.json'],
      '/p/tsconfig.test.json': files['/p/tsconfig.test.json'].replace(
        './tsconfig.json',
        './tsconfig.app.json'
      ),
    };
    const { projects } = await resolveTsProjects('/p', ...Object.values(fsOfFiles(withRefs)));
    const owner = (f) => projectFor(projects, f);
    assert.equal(owner('/p/src/App.tsx').path, '/p/tsconfig.app.json');
    assert.deepEqual(owner('/p/src/lib/a.test.ts').types, ['node', 'vite/client']);
    assert.equal(owner('/p/vite.config.ts').path, '/p/tsconfig.node.json');
    assert.equal(owner('/p/other/x.ts'), undefined);
  });

  it('defaults to everything except node_modules', async () => {
    const { projects } = await resolveTsProjects(
      '/p',
      ...Object.values(fsOfFiles({ '/p/tsconfig.json': '{}' }))
    );
    assert.equal(projectFor(projects, '/p/a/b.ts')?.path, '/p/tsconfig.json');
    assert.equal(projectFor(projects, '/p/node_modules/x/i.ts'), undefined);
  });

  it("a workspace member's config owns its files and adds its paths", async () => {
    const files = {
      '/p/tsconfig.json': `{ "compilerOptions": { "module": "nodenext", "paths": { "root/*": ["./src/*"] } } }`,
      '/p/@scope/lib/tsconfig.json': `{
        "extends": "../../tsconfig.json",
        "compilerOptions": { "types": ["node"], "paths": { "@scope/api/*": ["../api/src/*"] } },
        "exclude": ["dist"]
      }`,
      '/p/@scope/api/tsconfig.json': `{ "compilerOptions": { "moduleResolution": "Bundler" } }`,
    };
    const { options, projects } = await resolveTsProjects(
      '/p',
      ...Object.values(fsOfFiles(files)),
      ['/p/@scope/lib/tsconfig.json', '/p/@scope/api/tsconfig.json']
    );
    assert.deepEqual(options.paths, {
      'root/*': ['/p/src/*'],
      '@scope/api/*': ['/p/@scope/api/src/*'],
    });
    assert.equal(options.module, 'nodenext');
    assert.equal(options.moduleResolution, 'bundler', 'a member sets what the root leaves unset');
    assert.equal(projectFor(projects, '/p/src/a.ts')?.path, '/p/tsconfig.json');
    assert.equal(
      projectFor(projects, '/p/@scope/lib/src/a.ts')?.path,
      '/p/@scope/lib/tsconfig.json'
    );
    assert.deepEqual(projectFor(projects, '/p/@scope/lib/src/a.ts')?.types, ['node']);
    assert.equal(projectFor(projects, '/p/@scope/lib/dist/a.d.ts')?.path, '/p/tsconfig.json');
  });

  it('moduleSettings: node16/nodenext need their module, bundler and node keep it', () => {
    assert.deepEqual(moduleSettings({}), {});
    assert.deepEqual(moduleSettings({ module: 'esnext' }), {});
    assert.deepEqual(moduleSettings({ moduleResolution: 'node' }), { moduleResolution: 2 });
    assert.deepEqual(moduleSettings({ moduleResolution: 'bundler' }), { moduleResolution: 100 });
    assert.deepEqual(moduleSettings({ module: 'nodenext' }), { module: 199, moduleResolution: 99 });
    assert.deepEqual(moduleSettings({ module: 'esnext', moduleResolution: 'node16' }), {
      module: 100,
      moduleResolution: 3,
    });
    assert.deepEqual(moduleSettings({ moduleResolution: 'weird' }), {});
  });

  it("typesFor: the config's types, or every installed package before TS 6", () => {
    const installed = ['node', 'react'];
    assert.deepEqual(typesFor(undefined, true, installed), ['node', 'react']);
    assert.deepEqual(typesFor(undefined, false, installed), []);
    assert.deepEqual(typesFor(['vite/client', 'node'], false, installed), ['vite/client', 'node']);
    assert.deepEqual(typesFor([], true, installed), []);
    assert.deepEqual(typesFor(['*'], false, installed), ['node', 'react']);
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
