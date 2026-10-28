// activeConfig.ts tests (node:test): the compiler options the worker gets
// follow the folder the shown file is in, so a workspace's folders each keep
// their own jsx and lib (C06), and markers are trusted for that folder's
// files alone. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type * as monaco from 'monaco-editor';
import { ActiveConfig } from './activeConfig';
import type { ProjectConfig } from './compilerOptions';

/** jsx's values, as TypeScript's JsxEmit numbers them (compilerOptions.ts). */
const PRESERVE = 1 as monaco.typescript.JsxEmit;
const REACT_JSX = 4 as monaco.typescript.JsxEmit;

/** A folder's configs and options, as readProjectConfig gives them. */
const config = (options: monaco.typescript.CompilerOptions): ProjectConfig => ({
  options,
  projects: [],
  typesDefaultAll: true,
});

test("a TSX file in the second folder gets its folder's jsx and lib", () => {
  const types = new ActiveConfig(['/w/api', '/w/web']);
  types.configure('/w/api', config({ jsx: PRESERVE, lib: ['lib.es2022.d.ts'] }));
  types.configure('/w/web', config({ jsx: REACT_JSX, lib: ['lib.dom.d.ts'] }));

  const api = types.optionsFor('/w/api/src/app.tsx');
  const web = types.optionsFor('/w/web/src/app.tsx');
  assert.equal(web.jsx, REACT_JSX);
  assert.deepEqual(web.lib, ['lib.dom.d.ts']);
  assert.equal(api.jsx, PRESERVE);
  assert.deepEqual(api.lib, ['lib.es2022.d.ts']);

  // A file in no folder of the window (a tab of one that left): the first's.
  assert.equal(types.optionsFor('/elsewhere/a.tsx').jsx, PRESERVE);
});

test("the active file's folder decides which markers are trusted", () => {
  const types = new ActiveConfig(['/w/api', '/w/web']);
  types.configure('/w/api', config({ jsx: PRESERVE }));
  types.configure('/w/web', config({ jsx: REACT_JSX }));
  types.setActiveFile('/w/web/src/app.tsx');

  assert.equal(types.covers('/w/web/src/other.tsx'), true, "its own folder's program");
  assert.equal(types.covers('/w/api/src/app.tsx'), false, "another folder's file is out");
  // Not validated by the worker at all: nothing to distrust.
  assert.equal(types.covers('/w/api/README.md'), true);
});
