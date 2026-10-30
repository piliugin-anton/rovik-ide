import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { ActiveConfig } from './activeConfig';
import { workerLibs } from './libs';
import { PROJECT_LIB } from './paths';

describe('active TypeScript config', () => {
  it('keeps the originating config while browsing dependency definitions', () => {
    const config = new ActiveConfig('/p');
    config.configure({
      options: {},
      typesDefaultAll: true,
      projects: ['app', 'other'].map((name) => ({
        path: `/p/${name}/tsconfig.json`,
        include: [`/p/${name}/src/**/*`],
        files: [],
        exclude: [],
        types: [name],
      })),
    });
    // Capture which file supplies options without Monaco's URI/defaults runtime.
    const optionFiles: (string | null)[] = [];
    config.optionsFor = (path) => {
      optionFiles.push(path);
      return {};
    };
    config.setActiveFile('/p/app/src/main.ts');
    for (const file of [
      '/p/node_modules/@types/react/index.d.ts',
      '/p/app/node_modules/.pnpm/pkg@1/node_modules/pkg/index.d.ts',
    ]) {
      config.setActiveFile(file);
      config.apply();
      const project = JSON.parse(workerLibs.contentOf(PROJECT_LIB)!);
      assert.equal(project.path, '/p/app/tsconfig.json');
      assert.deepEqual(project.files, [file], 'the opened dependency is included explicitly');
      assert.equal(config.covers('/p/app/src/main.ts'), true);
      assert.equal(config.covers('/p/other/src/main.ts'), false);
    }
    config.setActiveFile('/p/other/src/main.ts');
    config.apply();
    assert.deepEqual(JSON.parse(workerLibs.contentOf(PROJECT_LIB)!).files, []);
    assert.deepEqual(optionFiles, [
      '/p/app/src/main.ts',
      '/p/app/src/main.ts',
      '/p/other/src/main.ts',
    ]);
    assert.equal(config.covers('/p/other/src/main.ts'), true);
    assert.equal(config.covers('/p/app/src/main.ts'), false);
  });
});
