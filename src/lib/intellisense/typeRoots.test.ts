import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { installedTypesFor, typesPackagesByFolder } from './typeRoots';

describe('type roots', () => {
  const byFolder = typesPackagesByFolder(
    [
      'file:///r/node_modules/@types/node/index.d.ts',
      'file:///r/node_modules/@types/node/package.json',
      'file:///r/node_modules/@types/react/index.d.ts',
      'file:///r/node_modules/react/index.d.ts',
      'file:///r/packages/app/node_modules/@types/jest/index.d.ts',
      'file:///r/node_modules/x/node_modules/@types/nested/index.d.ts',
      'file:///r/node_modules/@types/not-a-package.d.ts',
    ],
    ['file:///r/node_modules/@types/linked', 'file:///r/node_modules/plain']
  );

  it('groups @types packages by their node_modules folder', () => {
    assert.deepEqual(
      [...byFolder],
      [
        ['file:///r/node_modules', ['linked', 'node', 'react']],
        ['file:///r/packages/app/node_modules', ['jest']],
        ['file:///r/node_modules/x/node_modules', ['nested']],
      ]
    );
  });

  it('a config sees the packages of its folder and every folder above', () => {
    assert.deepEqual(installedTypesFor('file:///r', byFolder), ['linked', 'node', 'react']);
    assert.deepEqual(installedTypesFor('file:///r/packages/app', byFolder), [
      'jest',
      'linked',
      'node',
      'react',
    ]);
    assert.deepEqual(installedTypesFor('file:///r/packages/lib/', byFolder), [
      'linked',
      'node',
      'react',
    ]);
    assert.deepEqual(installedTypesFor('file:///elsewhere', byFolder), []);
  });
});
