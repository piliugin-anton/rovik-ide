import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { isDependencyPath, isProjectSource, isTsConfig, isTypingsManifest } from './paths';

describe('intellisense paths', () => {
  it('isProjectSource: JS/TS outside dependencies and build output', () => {
    assert.equal(isProjectSource('/p/src/a.ts'), true);
    assert.equal(isProjectSource('C:\\p\\src\\a.jsx'), true);
    assert.equal(isProjectSource('/p/src/a.md'), false);
    assert.equal(isProjectSource('/p/node_modules/x/index.d.ts'), false);
    assert.equal(isProjectSource('/p/dist/bundle.js'), false);
  });

  it('isDependencyPath: anything under a node_modules folder, path or URI', () => {
    assert.equal(isDependencyPath('file:///p/node_modules/@types/node/index.d.ts'), true);
    assert.equal(isDependencyPath('C:\\p\\node_modules\\x\\package.json'), true);
    assert.equal(isDependencyPath('file:///p/src/node_modules.ts'), false);
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
