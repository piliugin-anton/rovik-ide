import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { bareSpecifiers, candidatePackages, packageNameOf } from './imports';
import { Dependencies } from './dependencies';
import type { LoadedTypings } from '../../types';

describe('imports', () => {
  it('finds every kind of import and keeps the bare ones, each once', () => {
    const text = `
      import React, { useState } from "react";
      import type { A } from '@scope/pkg/sub';
      import './styles.css';
      import 'reflect-metadata';
      export * from "re-exported";
      export { x } from
        'next-line';
      const l = require('lodash/fp');
      const d = await import("dynamic");
      /// <reference types="vite/client" />
      import x = require("legacy");
      import abs from '/abs/path';
      import alias from '#alias';
      import url from 'https://esm.sh/x';
      import v from 'virtual:pwa';
      import again from "react";
    `;
    assert.deepEqual(bareSpecifiers(text), [
      'react',
      '@scope/pkg/sub',
      'reflect-metadata',
      're-exported',
      'next-line',
      'lodash/fp',
      'dynamic',
      'vite/client',
      'legacy',
    ]);
  });

  it('names the packages that may hold a specifier', () => {
    assert.equal(packageNameOf('lodash/fp'), 'lodash');
    assert.equal(packageNameOf('@scope/pkg/sub'), '@scope/pkg');
    assert.equal(packageNameOf('@/components'), null);
    assert.deepEqual(candidatePackages('react'), ['react', '@types/react']);
    assert.deepEqual(candidatePackages('@babel/core/x'), ['@babel/core', '@types/babel__core']);
    assert.deepEqual(candidatePackages('@types/node'), ['@types/node']);
    assert.deepEqual(candidatePackages('node:fs/promises'), ['@types/node']);
    assert.deepEqual(candidatePackages('fs'), ['fs', '@types/node']);
    assert.deepEqual(candidatePackages('./local'), []);
    assert.deepEqual(candidatePackages('@/x'), []);
  });
});

describe('Dependencies', () => {
  const loaded = (
    packages: [string, string][],
    missing: string[] = [],
    links: [string, string][] = []
  ): LoadedTypings => ({
    files: [],
    links: links.map(([path, target]) => ({ path, target })),
    packages: packages.map(([name, dir]) => ({ name, dir })),
    missing,
    truncated: false,
  });

  it('wants the imports whose package is neither loaded nor known to be missing', () => {
    const deps = new Dependencies(
      loaded(
        [
          ['react', '/p/node_modules/react'],
          ['@types/plain', '/p/node_modules/@types/plain'],
        ],
        ['ghost']
      )
    );
    assert.deepEqual(
      deps.wanted([
        'react',
        'react/jsx-runtime',
        'plain',
        'ghost',
        './x',
        '@/alias',
        'node:fs',
        'lodash',
        'lodash',
      ]),
      ['node:fs', 'lodash']
    );
    assert.deepEqual(deps.loadedDirs, ['/p/node_modules/react', '/p/node_modules/@types/plain']);
    deps.forgetMissing();
    assert.deepEqual(deps.wanted(['ghost']), ['ghost']);
  });

  it('absorbs later loads and starts over on a reset', () => {
    const deps = new Dependencies(
      loaded([['a', '/p/node_modules/a']], [], [['/p/node_modules/l', '/p/store/l']])
    );
    assert.equal(deps.absorb(loaded([['b', '/p/node_modules/b']], ['nope'])), false);
    assert.equal(deps.absorb(loaded([], [], [['/p/node_modules/m', '/p/store/m']])), true);
    assert.deepEqual(deps.wanted(['a', 'b', 'nope', 'c']), ['c']);
    assert.deepEqual(
      [...deps.allLinks],
      [
        ['/p/node_modules/l', '/p/store/l'],
        ['/p/node_modules/m', '/p/store/m'],
      ]
    );
    deps.reset(loaded([['c', '/p/node_modules/c']]));
    assert.deepEqual(deps.wanted(['a', 'b', 'nope', 'c']), ['a', 'b', 'nope']);
    assert.equal(deps.allLinks.size, 0);
    assert.deepEqual(deps.loadedDirs, ['/p/node_modules/c']);
  });
});
