// The project worker against Monaco's real TypeScript worker and services:
// what the program is made of, and how dependency typings reach it.
import './testSelf';
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import type { typescript } from 'monaco-editor';
import type {
  ExtraLib,
  MirrorModel,
  WorkerContext,
} from 'monaco-editor/language/typescript/ts.worker.js';
import { LINKS_LIB } from './paths';
import { ProjectTypeScriptWorker } from './projectWorker';

// Keys as the app builds them (uris.ts): the folder encoded like a model's
// URI (a space in the project path), the package part raw.
const ROOT = 'file:///home/u/my%20proj';
const NM = `${ROOT}/node_modules`;
const APP = `${ROOT}/src/app.ts`;
const UTIL = `${ROOT}/src/util.ts`;

const LIBS: Record<string, ExtraLib> = {};
const lib = (key: string, content: string) => (LIBS[key] = { content, version: 1 });
lib(UTIL, 'export const u = 1;');
lib(`${ROOT}/package.json`, '{"name":"my-proj"}');
lib(`${ROOT}/tsconfig.json`, '{}');
lib(`${NM}/@scope/pkg/package.json`, '{"name":"@scope/pkg","types":"dist/index.d.ts"}');
lib(`${NM}/@scope/pkg/dist/index.d.ts`, 'export declare const x: number;');
lib(`${NM}/plain/package.json`, '{"name":"plain","main":"index.js"}');
lib(`${NM}/@types/plain/package.json`, '{"name":"@types/plain","types":"index.d.ts"}');
lib(`${NM}/@types/plain/index.d.ts`, 'declare const y: string; export default y;');
lib(`${NM}/@types/node/package.json`, '{"name":"@types/node","types":"index.d.ts"}');
lib(`${NM}/@types/node/index.d.ts`, 'declare var process: { pid: number };');
lib(`${NM}/@types/unused/index.d.ts`, 'declare var UNUSED_GLOBAL: number;');
lib(`${NM}/vite/client.d.ts`, 'interface ImportMeta { readonly env: { MODE: string } }');
lib(`${NM}/big/package.json`, '{"name":"big","types":"index.d.ts"}');
lib(`${NM}/big/index.d.ts`, 'export declare const big: number;');

const APP_TEXT = [
  "import { x } from '@scope/pkg';",
  "import y from 'plain';",
  "import { u } from './util';",
  'console.log(process.pid, x, y, u, import.meta.env.MODE);',
].join('\n');

// What the app gives the worker (defaults.ts BASE_OPTIONS, typesScope.ts).
const OPTIONS = {
  target: 99,
  module: 99,
  moduleResolution: 2,
  jsx: 1,
  allowJs: true,
  allowNonTsExtensions: true,
  esModuleInterop: true,
  configFilePath: `${ROOT}/tsconfig.json`,
};

function model(uri: string, text: string, version = 1): MirrorModel {
  const parsed = new URL(uri);
  return {
    uri: { path: decodeURIComponent(parsed.pathname), toString: () => uri } as MirrorModel['uri'],
    version,
    getValue: () => text,
  };
}

function worker(models: MirrorModel[], options: typescript.CompilerOptions, libs = LIBS) {
  const ctx: WorkerContext = { getMirrorModels: () => models };
  return new ProjectTypeScriptWorker(ctx, { compilerOptions: options, extraLibs: libs });
}

const codes = (diagnostics: { code: number }[]) => diagnostics.map((d) => d.code);
const programFiles = (w: ProjectTypeScriptWorker) =>
  (
    w as unknown as {
      getLanguageService(): { getProgram(): { getSourceFiles(): { fileName: string }[] } };
    }
  )
    .getLanguageService()
    .getProgram()
    .getSourceFiles()
    .map((f) => f.fileName);

describe('ProjectTypeScriptWorker', () => {
  it('parses only the typings the project reaches; imports and types resolve', async () => {
    const w = worker([model(APP, APP_TEXT)], { ...OPTIONS, types: ['node', 'vite/client'] });
    assert.deepEqual(codes(await w.getSemanticDiagnostics(APP)), []);
    assert.deepEqual(codes(await w.getCompilerOptionsDiagnostics(APP)), []);
    assert.deepEqual(w.getScriptFileNames().sort(), [APP, UTIL]);
    const files = programFiles(w);
    for (const reached of [
      `${NM}/@scope/pkg/dist/index.d.ts`,
      `${NM}/@types/plain/index.d.ts`,
      `${NM}/@types/node/index.d.ts`,
      `${NM}/vite/client.d.ts`,
    ]) {
      assert.ok(files.includes(reached), `in program: ${reached}`);
    }
    for (const unreached of [`${NM}/big/index.d.ts`, `${NM}/@types/unused/index.d.ts`]) {
      assert.ok(!files.includes(unreached), `not parsed: ${unreached}`);
    }
    const defs = await w.getDefinitionAtPosition(APP, APP_TEXT.indexOf('x }'));
    assert.equal(defs?.[0]?.fileName, `${NM}/@scope/pkg/dist/index.d.ts`);
  });

  it('globals follow `types`, as tsc', async () => {
    const none = worker([model(APP, 'console.log(process.pid);')], { ...OPTIONS, types: [] });
    assert.deepEqual(codes(await none.getSemanticDiagnostics(APP)), [2591]);
    const listed = worker([model(APP, 'console.log(UNUSED_GLOBAL);')], {
      ...OPTIONS,
      types: ['unused'],
    });
    assert.deepEqual(codes(await listed.getSemanticDiagnostics(APP)), []);
  });

  it('an open file is one root, read from its model, with its own version space', () => {
    const w = worker([model(APP, APP_TEXT), model(UTIL, 'export const u = "model";', 4)], {
      ...OPTIONS,
      types: ['node', 'vite/client'],
    });
    assert.deepEqual(w.getScriptFileNames().sort(), [APP, UTIL]);
    assert.equal(w.getScriptVersion(UTIL), 'model:4');
    assert.equal(w.getScriptVersion(`${NM}/big/index.d.ts`), '1');
    assert.ok(w.readFile(UTIL)?.includes('"model"'));
  });

  it('a linked package is one source file, reached through any link', async () => {
    const STORE = `${NM}/.pnpm/foo@1/node_modules/foo`;
    const MEMBER = `${ROOT}/packages/app`;
    const libs: Record<string, ExtraLib> = {
      [`${STORE}/package.json`]: { content: '{"name":"foo","types":"index.d.ts"}', version: 1 },
      [`${STORE}/index.d.ts`]: { content: 'export declare const foo: number;', version: 1 },
      [LINKS_LIB]: {
        content: JSON.stringify({
          [`${NM}/foo`]: STORE,
          [`${MEMBER}/node_modules/foo`]: STORE,
        }),
        version: 1,
      },
    };
    const text = "import { foo } from 'foo'; export const n: string = foo;";
    const memberFile = `${MEMBER}/src/a.ts`;
    const w = worker([model(APP, text), model(memberFile, text)], { ...OPTIONS, types: [] }, libs);
    assert.deepEqual(codes(await w.getSemanticDiagnostics(APP)), [2322]);
    assert.deepEqual(codes(await w.getSemanticDiagnostics(memberFile)), [2322]);
    assert.deepEqual(w.getScriptFileNames().sort(), [APP, memberFile].sort());
    const files = programFiles(w).filter((f) => f.includes('foo'));
    assert.deepEqual(files, [`${STORE}/index.d.ts`]);
    assert.equal(w.realpath(`${MEMBER}/node_modules/foo/index.d.ts`), `${STORE}/index.d.ts`);
    assert.equal(w.realpath(`${ROOT}/src/app.ts`), `${ROOT}/src/app.ts`);
  });

  it('a workspace member in an @scope folder: through paths, or through exports', async () => {
    // The folder part of a key is encoded like a model's URI (`@` → %40),
    // the part after node_modules raw (uris.ts).
    const PKG = `${ROOT}/%40something/central-client-api`;
    const libs: Record<string, ExtraLib> = {
      [`${PKG}/package.json`]: {
        content:
          '{"name":"@something/central-client-api","exports":{"./data":{"types":"./src/data.ts"}}}',
        version: 1,
      },
      [`${PKG}/src/data.ts`]: { content: 'export const data = 1;', version: 1 },
      [LINKS_LIB]: {
        content: JSON.stringify({ [`${NM}/@something/central-client-api`]: PKG }),
        version: 1,
      },
    };
    const app = `${ROOT}/apps/x/src/a.ts`;
    const text =
      "import { data } from '@something/central-client-api/data'; export const n: string = data;";
    const paths = { '@something/central-client-api/*': [`${PKG}/src/*`] };
    const viaPaths = worker([model(app, text)], { ...OPTIONS, types: [], paths }, libs);
    assert.deepEqual(codes(await viaPaths.getSemanticDiagnostics(app)), [2322]);
    const viaExports = worker(
      [model(app, text)],
      { ...OPTIONS, types: [], moduleResolution: 100 as typescript.ModuleResolutionKind },
      libs
    );
    assert.deepEqual(codes(await viaExports.getSemanticDiagnostics(app)), [2322]);
    const node10 = worker([model(app, text)], { ...OPTIONS, types: [] }, libs);
    assert.deepEqual(
      codes(await node10.getSemanticDiagnostics(app)),
      [2307],
      'as tsc: no exports under node10'
    );
  });

  it('a dependency file opened as a tab resolves its imports to the libs', async () => {
    // Monaco encodes `@` in a model's URI; the libs keep it raw after node_modules.
    const opened = `${NM}/%40mui/material/FormHelperText/FormHelperText.d.ts`;
    const text = "import { Base } from '../OverridableComponent'; export declare const F: Base;";
    const libs: Record<string, ExtraLib> = {
      [`${NM}/@mui/material/OverridableComponent.d.ts`]: {
        content: 'export interface Base { kind: string }',
        version: 1,
      },
    };
    const w = worker([model(opened, text)], { ...OPTIONS, types: [] }, libs);
    assert.deepEqual(codes(await w.getSemanticDiagnostics(opened)), []);
    assert.equal(
      w.realpath(`${NM}/%40mui/material/OverridableComponent.d.ts`),
      `${NM}/@mui/material/OverridableComponent.d.ts`
    );
  });

  it('a changed lib is parsed again, the others are kept', async () => {
    const w = worker([model(APP, APP_TEXT)], { ...OPTIONS, types: ['node', 'vite/client'] });
    await w.getSemanticDiagnostics(APP);
    const program = () =>
      (
        w as unknown as {
          getLanguageService(): { getProgram(): { getSourceFile(n: string): object } };
        }
      )
        .getLanguageService()
        .getProgram();
    const pkg = program().getSourceFile(`${NM}/@scope/pkg/dist/index.d.ts`);
    const node = program().getSourceFile(`${NM}/@types/node/index.d.ts`);
    await w.updateExtraLibs({
      ...LIBS,
      [`${NM}/@types/node/index.d.ts`]: {
        content: 'declare var process: { pid: string };',
        version: 2,
      },
    });
    await w.getSemanticDiagnostics(APP);
    assert.equal(program().getSourceFile(`${NM}/@scope/pkg/dist/index.d.ts`), pkg);
    assert.notEqual(program().getSourceFile(`${NM}/@types/node/index.d.ts`), node);
  });
});
