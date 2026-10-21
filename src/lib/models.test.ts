import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { followDisk, syncTabModels, type FileModel, type FileModels } from './models';

/** Models of files as a test has them: text, whether an editor shows them, edits made. */
function fakeModels(files: Record<string, { text: string; shown?: boolean }>) {
  const live = new Map<string, FileModel & { edits: string[]; reads: () => number }>();
  for (const [path, { text, shown = false }] of Object.entries(files)) {
    let value = text;
    let disposed = false;
    let valueReads = 0;
    const edits: string[] = [];
    live.set(path, {
      key: `file://${path}`,
      path,
      getValue: () => {
        valueReads++;
        return value;
      },
      isDisposed: () => disposed,
      isAttachedToEditor: () => shown,
      replace: (next) => {
        edits.push(next);
        value = next;
      },
      dispose: () => {
        disposed = true;
      },
      edits,
      reads: () => valueReads,
    });
  }
  const models: FileModels = {
    keyOf: (path) => `file://${path}`,
    forPath: (path) => live.get(path),
    all: () => [...live.values()],
  };
  return { models, model: (path: string) => live.get(path)! };
}

describe('syncTabModels', () => {
  it("a tab in the background gets its tab's text; the shown one is its editor's", () => {
    const { models, model } = fakeModels({
      '/p/behind.ts': { text: 'export const a = 1;' },
      '/p/shown.ts': { text: 'typed', shown: true },
    });
    syncTabModels(
      [
        { path: '/p/behind.ts', content: 'export const a = 1;\nexport const b = 2;' },
        { path: '/p/shown.ts', content: 'on disk' },
        { path: '/p/no-model-yet.ts', content: 'x' },
      ],
      models
    );
    assert.deepEqual(model('/p/behind.ts').edits, ['export const a = 1;\nexport const b = 2;']);
    assert.deepEqual(model('/p/shown.ts').edits, []);
  });

  it("a tab whose text didn't change isn't read again", () => {
    const { models, model } = fakeModels({ '/p/same.ts': { text: 'one' } });
    const tabs = [{ path: '/p/same.ts', content: 'two' }];
    syncTabModels(tabs, models);
    const reads = model('/p/same.ts').reads();
    syncTabModels(tabs, models);
    assert.equal(model('/p/same.ts').reads(), reads);
    assert.deepEqual(model('/p/same.ts').edits, ['two']);
  });
});

describe('followDisk', () => {
  it("models no tab holds take the disk's text, and go with their files", async () => {
    const { models, model } = fakeModels({
      '/p/src/utils/a.ts': { text: 'old a' },
      '/p/src/utils/gone.ts': { text: 'gone' },
      '/p/src/utils/binary.ts': { text: 'kept' },
      '/p/src/utils/open.ts': { text: 'a tab has it' },
      '/p/src/other.ts': { text: 'elsewhere' },
    });
    const disk: Record<string, string | null | undefined> = {
      '/p/src/utils/a.ts': 'new a',
      '/p/src/utils/gone.ts': null,
      '/p/src/utils/binary.ts': undefined,
      '/p/src/utils/open.ts': 'on disk',
      '/p/src/other.ts': 'changed too',
    };
    const read: string[] = [];
    await followDisk('/p/src/utils', {
      models,
      read: async (path) => {
        read.push(path);
        return disk[path];
      },
      openTabs: () => new Set(['file:///p/src/utils/open.ts']),
    });
    assert.deepEqual(model('/p/src/utils/a.ts').edits, ['new a']);
    assert.equal(model('/p/src/utils/gone.ts').isDisposed(), true);
    assert.deepEqual(model('/p/src/utils/binary.ts').edits, [], 'unreadable: left as it is');
    assert.deepEqual(model('/p/src/utils/open.ts').edits, [], 'its tab keeps it');
    assert.deepEqual(model('/p/src/other.ts').edits, [], 'outside the folder');
    assert.deepEqual(read.sort(), [
      '/p/src/utils/a.ts',
      '/p/src/utils/binary.ts',
      '/p/src/utils/gone.ts',
    ]);
  });

  it("files are read as the watcher spells them, not as their model's URI does", async () => {
    // Keys as modelUriFor makes them: forward slashes, the drive letter
    // lower-cased and its colon encoded, a space as %20.
    const keyOf = (p: string) =>
      'file:///' +
      p
        .replace(/\\/g, '/')
        .replace(/^([A-Za-z]):/, (_, d: string) => `${d.toLowerCase()}%3A`)
        .replace(/ /g, '%20');
    const { model } = fakeModels({ x: { text: 'old' } });
    const base = model('x');
    const inner: FileModel = {
      ...base,
      key: keyOf('C:\\my proj\\src\\deep\\a.ts'),
      path: '\\wrong',
    };
    const models: FileModels = { keyOf, forPath: () => undefined, all: () => [inner] };
    const read: string[] = [];
    await followDisk('C:\\my proj\\src', {
      models,
      read: async (path) => {
        read.push(path);
        return 'new';
      },
      openTabs: () => new Set(),
    });
    assert.deepEqual(read, ['C:\\my proj\\src\\deep\\a.ts']);
    assert.deepEqual(model('x').edits, ['new']);
  });

  it('a file opened in a tab while it was read is left to the tab', async () => {
    const { models, model } = fakeModels({ '/p/a.ts': { text: 'old' } });
    let open = new Set<string>();
    await followDisk('/p/a.ts', {
      models,
      read: async () => {
        open = new Set(['file:///p/a.ts']);
        return 'new';
      },
      openTabs: () => open,
    });
    assert.deepEqual(model('/p/a.ts').edits, []);
  });
});
