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
import { LINKS_LIB, PROJECT_LIB } from './paths';
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

// What the app gives the worker (defaults.ts BASE_OPTIONS, activeConfig.ts).
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
    // One name for the file: the raw one, as a root and in the program.
    assert.deepEqual(w.getScriptFileNames(), [
      `${NM}/@mui/material/FormHelperText/FormHelperText.d.ts`,
    ]);
    assert.ok(!programFiles(w).some((f) => f.includes('%40')));
  });

  it("the program is the active config's files; others get empty answers", async () => {
    const PROJECT_DIR = '/home/u/my proj';
    const appConfig = {
      path: `${PROJECT_DIR}/apps/app/tsconfig.json`,
      include: [`${PROJECT_DIR}/apps/app/**/*`],
      files: [],
      exclude: [],
    };
    const appFile = `${ROOT}/apps/app/src/a.ts`;
    const otherFile = `${ROOT}/apps/other/src/b.ts`;
    const libs: Record<string, ExtraLib> = {
      [`${ROOT}/apps/app/src/util.ts`]: { content: 'export const u = 1;', version: 1 },
      [`${ROOT}/apps/other/src/util.ts`]: { content: 'export const v = 1;', version: 1 },
      [PROJECT_LIB]: { content: JSON.stringify(appConfig), version: 1 },
    };
    const bad = 'export const n: string = 1;';
    const w = worker([model(appFile, bad), model(otherFile, bad)], { ...OPTIONS, types: [] }, libs);
    assert.deepEqual(w.getScriptFileNames().sort(), [appFile, `${ROOT}/apps/app/src/util.ts`]);
    assert.deepEqual(codes(await w.getSemanticDiagnostics(appFile)), [2322]);
    assert.deepEqual(codes(await w.getSemanticDiagnostics(otherFile)), [], 'outside: no answer');
    assert.equal(await w.getQuickInfoAtPosition(otherFile, 14), undefined);
    assert.deepEqual(await w.getRenameInfo(otherFile, 14, {}), {
      canRename: false,
      localizedErrorMessage: 'Not in the program.',
    });
    // Without a config, every source is in.
    const all = worker(
      [model(otherFile, bad)],
      { ...OPTIONS, types: [] },
      { ...libs, [PROJECT_LIB]: { content: 'null', version: 2 } }
    );
    assert.equal(all.getScriptFileNames().length, 3);
  });

  it('programInfo: files, packages held twice, and where imports resolve', async () => {
    const libs: Record<string, ExtraLib> = {
      ...LIBS,
      [`${NM}/@scope/pkg/node_modules/@types/plain/index.d.ts`]: {
        content: 'declare const y: number; export default y;',
        version: 1,
      },
    };
    const text = "import { x } from '@scope/pkg'; import y from 'plain'; import z from 'nowhere';";
    const w = worker([model(APP, text)], { ...OPTIONS, types: [] }, libs);
    await w.getSemanticDiagnostics(APP);
    const info = await w.programInfo(APP);
    assert.ok(info.files > 3);
    assert.deepEqual(info.duplicates, {});
    assert.deepEqual(info.imports, {
      '@scope/pkg': `${NM}/@scope/pkg/dist/index.d.ts`,
      plain: `${NM}/@types/plain/index.d.ts`,
      nowhere: null,
    });
    // The same package reached under two folders is reported.
    const pkgText = "import y from 'plain'; export declare const x: typeof y;";
    const two = worker(
      [model(APP, text), model(`${NM}/@scope/pkg/dist/index.d.ts`, pkgText)],
      { ...OPTIONS, types: [] },
      libs
    );
    await two.getSemanticDiagnostics(APP);
    assert.deepEqual((await two.programInfo(APP)).duplicates, {
      '@types/plain': [`${NM}/@scope/pkg/node_modules/@types/plain`, `${NM}/@types/plain`],
    });
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

  it('an import that found nothing finds a file that comes later, its importer unchanged', async () => {
    // A source outside the active config (not a root of the program), and
    // a package's typings once it's installed: both reach the worker as
    // libs the program doesn't list, so only a lookup made again finds them.
    const SHARED = `${ROOT}/shared/x.ts`;
    const config = {
      path: '/home/u/my proj/tsconfig.json',
      include: ['/home/u/my proj/src/**/*'],
      files: [],
      exclude: [],
    };
    const text =
      "import { x } from '../shared/x'; import { z } from 'zod'; export const n = x + z;";
    const project = { [PROJECT_LIB]: { content: JSON.stringify(config), version: 1 } };
    const w = worker([model(APP, text)], { ...OPTIONS, types: [] }, project);
    assert.deepEqual(codes(await w.getSemanticDiagnostics(APP)), [2307, 2307]);
    await w.updateExtraLibs({
      ...project,
      [SHARED]: { content: 'export const x = 1;', version: 1 },
    });
    assert.deepEqual(codes(await w.getSemanticDiagnostics(APP)), [2307], 'the new source is found');
    await w.updateExtraLibs({
      ...project,
      [SHARED]: { content: 'export const x = 1;', version: 1 },
      [`${NM}/zod/package.json`]: { content: '{"name":"zod","types":"index.d.ts"}', version: 1 },
      [`${NM}/zod/index.d.ts`]: { content: 'export declare const z: number;', version: 1 },
    });
    assert.deepEqual(codes(await w.getSemanticDiagnostics(APP)), [], "the package's typings too");
    // And a file that goes is missed again.
    await w.updateExtraLibs({ ...project });
    assert.deepEqual(codes(await w.getSemanticDiagnostics(APP)), [2307, 2307]);
  });

  it('files coming or going rebuild the program once; other lib changes keep it', async () => {
    const w = worker([model(APP, APP_TEXT)], { ...OPTIONS, types: ['node', 'vite/client'] });
    await w.getSemanticDiagnostics(APP);
    const program = () => w.getLanguageService().getProgram();
    const first = program();
    // A file nothing reaches changes: the program stays as it is.
    await w.updateExtraLibs({
      ...LIBS,
      [`${NM}/big/index.d.ts`]: { content: 'export declare const big: string;', version: 2 },
    });
    await w.getSemanticDiagnostics(APP);
    assert.equal(program(), first, 'same files, nothing to resolve again');
    // A file comes: one new program, then it's kept.
    await w.updateExtraLibs({
      ...LIBS,
      [`${NM}/new/index.d.ts`]: { content: 'export declare const fresh: number;', version: 1 },
    });
    await w.getSemanticDiagnostics(APP);
    const second = program();
    assert.notEqual(second, first);
    await w.getSemanticDiagnostics(APP);
    await w.getQuickInfoAtPosition(APP, 10);
    assert.equal(program(), second, 'not rebuilt on every call');
  });

  it('a program an edit replaced is emptied; the answers follow the edits', async () => {
    // An open tab being typed in: each edit is a new version of its model.
    let text = 'export const n: number = 1;';
    let version = 1;
    const typing: MirrorModel = {
      uri: { path: new URL(APP).pathname, toString: () => APP } as MirrorModel['uri'],
      get version() {
        return version;
      },
      getValue: () => text,
    };
    const w = worker([typing], { ...OPTIONS, types: [] });
    assert.deepEqual(codes(await w.getSemanticDiagnostics(APP)), []);
    const first = w.getLanguageService().getProgram();
    assert.ok(first);
    for (let edit = 1; edit <= 4; edit++) {
      text = edit % 2 ? 'export const n: string = 1;' : 'export const n: number = 1;';
      version++;
      assert.deepEqual(codes(await w.getSemanticDiagnostics(APP)), edit % 2 ? [2322] : []);
    }
    // Nothing of the first program is left to keep its checker alive.
    assert.deepEqual(Object.keys(first), []);
    const current = w.getLanguageService().getProgram();
    assert.notEqual(current, first);
    assert.ok(current?.getSourceFile(APP));
    // Answers that don't change the program leave it as it is.
    await w.getQuickInfoAtPosition(APP, 13);
    assert.equal(w.getLanguageService().getProgram(), current);
    assert.ok(current?.getSourceFile(APP));
  });
});
