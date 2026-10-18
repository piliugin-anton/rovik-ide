// sharedState.ts tests (node:test): maps the windows share. Each "window"
// here is a map with a browser storage of its own, as Rovik's windows have;
// the backend is one, as the data folder is.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { sharedMap, type Entries, type SharedBackend } from './sharedState';

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** The data folder's maps, as shared_state.rs keeps them. */
function folder() {
  const maps = new Map<string, Entries>();
  let fail = 0;
  const backend: SharedBackend = {
    async read(name) {
      if (fail > 0 && fail--) throw new Error('unreadable');
      const saved = maps.get(name);
      return saved ? { ...saved } : null;
    },
    async start(name, entries) {
      if (fail > 0 && fail--) throw new Error('unwritable');
      if (!maps.has(name)) maps.set(name, { ...entries });
      return { ...maps.get(name)! };
    },
    async merge(name, patch) {
      if (fail > 0 && fail--) throw new Error('unwritable');
      const saved = { ...(maps.get(name) ?? {}) };
      for (const [k, v] of Object.entries(patch)) {
        if (v === null) delete saved[k];
        else saved[k] = v;
      }
      maps.set(name, saved);
      return { ...saved };
    },
  };
  return { maps, backend, failNext: (n: number) => (fail = n) };
}

/** A window's browser storage. */
function storage(initial: Record<string, string> = {}) {
  const items = new Map(Object.entries(initial));
  return {
    items,
    getItem: (k: string) => items.get(k) ?? null,
    setItem: (k: string, v: string) => void items.set(k, v),
    removeItem: (k: string) => void items.delete(k),
  };
}

test("a window's save keeps what another saved meanwhile", async () => {
  const { maps, backend } = folder();
  // Both started with the same settings in their storage.
  const before = JSON.stringify({ fontSize: 14, wordWrap: 'off' });
  const a = sharedMap('editor-settings', { key: 'k', storage: storage({ k: before }), backend });
  const b = sharedMap('editor-settings', { key: 'k', storage: storage({ k: before }), backend });
  await a.set({ fontSize: 20 });
  await b.set({ wordWrap: 'on' });
  assert.deepEqual(maps.get('editor-settings'), { fontSize: 20, wordWrap: 'on' });
  assert.deepEqual(b.all(), { fontSize: 20, wordWrap: 'on' }, "b's save brought a's back");
  // a hears b's when it reads again (it comes to the front).
  const heard: Entries[] = [];
  a.subscribe((all) => heard.push(all));
  await a.refresh();
  assert.deepEqual(a.all(), { fontSize: 20, wordWrap: 'on' });
  assert.deepEqual(heard, [{ fontSize: 20, wordWrap: 'on' }]);
  await a.refresh();
  assert.equal(heard.length, 1, 'nothing new: nothing heard');
});

test('the file starts as the copy when none was saved: an update from before', async () => {
  const { maps, backend } = folder();
  const local = storage({ k: JSON.stringify({ '/p/a': { files: [] } }) });
  const a = sharedMap('project-sessions', { key: 'k', storage: local, backend });
  await a.refresh();
  assert.deepEqual(maps.get('project-sessions'), { '/p/a': { files: [] } });
  // The next window starts from the file, whatever its own storage has.
  const b = sharedMap('project-sessions', { key: 'k', storage: storage(), backend });
  await b.refresh();
  assert.deepEqual(b.all(), { '/p/a': { files: [] } });
});

test('a removed entry is removed from the file, not just here', async () => {
  const { maps, backend } = folder();
  const a = sharedMap('chat-unread', { key: 'k', storage: storage(), backend });
  const b = sharedMap('chat-unread', { key: 'k', storage: storage(), backend });
  await a.set({ c1: true, c2: true });
  await b.refresh();
  await b.set({ c1: null });
  await a.set({ c3: true });
  assert.deepEqual(maps.get('chat-unread'), { c2: true, c3: true }, 'c1 read in b stays read');
  assert.deepEqual(a.all(), { c2: true, c3: true });
});

test("what's on its way stays over a read that comes back before it", async () => {
  const { maps, backend } = folder();
  maps.set('m', { y: 2 });
  let open: () => void = () => {};
  const gate = new Promise<void>((resolve) => (open = resolve));
  const slow: SharedBackend = {
    ...backend,
    start: async (name, entries) => {
      await gate;
      return backend.start(name, entries);
    },
  };
  const a = sharedMap('m', { key: 'k', storage: storage(), backend: slow });
  await settle();
  const saved = a.set({ x: 1 });
  assert.deepEqual(a.all(), { x: 1 }, 'here at once');
  // The first read comes back: the file's, without x yet.
  open();
  await settle();
  assert.deepEqual(a.all(), { x: 1, y: 2 }, 'the read from before it doesn’t take it back');
  await saved;
  assert.deepEqual(maps.get('m'), { x: 1, y: 2 });
});

test('two windows starting at once: neither copy undoes the other’s first change', async () => {
  const { maps, backend } = folder();
  const before = JSON.stringify({ fontSize: 14 });
  const a = sharedMap('m', { key: 'k', storage: storage({ k: before }), backend });
  const b = sharedMap('m', { key: 'k', storage: storage({ k: before }), backend });
  await a.set({ fontSize: 20 });
  await b.refresh();
  assert.deepEqual(maps.get('m'), { fontSize: 20 });
  assert.deepEqual(b.all(), { fontSize: 20 });
});

test('a save that failed goes with the next', async () => {
  const { maps, backend, failNext } = folder();
  const a = sharedMap('m', { key: 'k', storage: storage(), backend });
  await a.refresh();
  failNext(1);
  await a.set({ x: 1 });
  assert.equal(maps.get('m')?.x, undefined);
  assert.deepEqual(a.all(), { x: 1 }, 'kept here meanwhile');
  await a.set({ y: 2 });
  assert.deepEqual(maps.get('m'), { x: 1, y: 2 });
});

test('the copy is kept in the browser storage, to start from', async () => {
  const { backend } = folder();
  const local = storage();
  const a = sharedMap('m', { key: 'k', storage: local, backend });
  await a.set({ x: 1 });
  assert.equal(local.items.get('k'), JSON.stringify({ x: 1 }));
  await a.set({ x: null });
  assert.equal(local.items.has('k'), false, 'nothing kept: nothing stored');
});

test('without the backend (not in the app) the copy is all there is', async () => {
  const local = storage({ k: '{not json' });
  const a = sharedMap('m', { key: 'k', storage: local, backend: null });
  assert.deepEqual(a.all(), {}, 'bad data is no data');
  await a.set({ x: 1 });
  await a.refresh();
  assert.deepEqual(a.all(), { x: 1 });
});
