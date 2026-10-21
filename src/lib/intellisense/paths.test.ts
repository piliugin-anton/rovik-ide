import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  isDependencyPath,
  isLoadedLib,
  isProjectSource,
  isSourceLib,
  isTsConfig,
  isTypingsLib,
  isTypingsManifest,
  LINKS_LIB,
  PROJECT_LIB,
} from './paths';

describe('intellisense paths', () => {
  it('isProjectSource: JS/TS outside dependencies and build output', () => {
    assert.equal(isProjectSource('/p/src/a.ts'), true);
    assert.equal(isProjectSource('C:\\p\\src\\a.jsx'), true);
    assert.equal(isProjectSource('/p/src/a.md'), false);
    assert.equal(isProjectSource('/p/node_modules/x/index.d.ts'), false);
    assert.equal(isProjectSource('/p/dist/bundle.js'), false);
  });

  it('isProjectSource with the root: only the folders below it count', () => {
    // A project kept in a folder named like build output.
    assert.equal(isProjectSource('/home/u/build/app/src/a.ts', '/home/u/build/app'), true);
    assert.equal(isProjectSource('C:\\out\\app\\src\\a.ts', 'C:\\out\\app\\'), true);
    assert.equal(isProjectSource('/home/u/build/app/dist/a.js', '/home/u/build/app'), false);
    assert.equal(
      isProjectSource('/home/u/build/app/node_modules/x/a.d.ts', '/home/u/build/app'),
      false
    );
    // Outside the root: all of the path, as without one.
    assert.equal(isProjectSource('/home/u/build/other/a.ts', '/home/u/app'), false);
  });

  it('isDependencyPath: anything under a node_modules folder, path or URI', () => {
    assert.equal(isDependencyPath('file:///p/node_modules/@types/node/index.d.ts'), true);
    assert.equal(isDependencyPath('C:\\p\\node_modules\\x\\package.json'), true);
    assert.equal(isDependencyPath('file:///p/src/node_modules.ts'), false);
  });

  it('which libs each load replaces: the control lib of the active config never', () => {
    const typing = 'file:///p/node_modules/x/index.d.ts';
    const source = 'file:///p/src/a.ts';
    assert.deepEqual([typing, source, LINKS_LIB, PROJECT_LIB].map(isTypingsLib), [
      true,
      false,
      true,
      false,
    ]);
    assert.deepEqual([typing, source, LINKS_LIB, PROJECT_LIB].map(isSourceLib), [
      false,
      true,
      false,
      false,
    ]);
    assert.deepEqual([typing, source, LINKS_LIB, PROJECT_LIB].map(isLoadedLib), [
      true,
      true,
      true,
      false,
    ]);
  });

  it('manifests and configs by file name', () => {
    assert.equal(isTypingsManifest('/p/package.json'), true);
    assert.equal(isTypingsManifest('/p/packages/x/pnpm-lock.yaml'), true);
    assert.equal(isTypingsManifest('/p/src/package.ts'), false);
    assert.equal(isTsConfig('/p/tsconfig.json'), true);
    assert.equal(isTsConfig('/p/tsconfig.app.json'), true);
    assert.equal(isTsConfig('C:\\p\\jsconfig.json'), true);
    assert.equal(isTsConfig('/p/tsconfig.json.bak'), false);
  });
});
