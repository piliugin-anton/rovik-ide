// sharedState.ts
//
// What Rovik's windows share of what each keeps in its browser storage.
// That storage is a window's own: read once, as the window starts, and
// written whole, so a window saving its copy of a map undid what another
// had saved meanwhile: the tabs of a project whose window had closed, a
// setting changed in another window, a chat's unread mark. A shared map is
// kept by the backend instead (shared_state.rs), a file each: a save sends
// only the entries it changes, and a window reads the file again when
// another may have changed it: as it comes to the front, and before it
// needs an entry another may have saved (a project's tabs, as it opens).
// The browser storage keeps this window's copy, to start from; the file
// starts as that copy when none was saved yet (an update from before).
import { invoke } from '@tauri-apps/api/core';

export type Entries = Record<string, unknown>;

/** Where shared maps are kept: the backend's files. */
export interface SharedBackend {
  /** The map as saved; null while none was saved. */
  read(name: string): Promise<Entries | null>;
  /** The map as saved, or `entries` saved as it while none is (another
   *  window's, saved first, is kept whole). */
  start(name: string, entries: Entries): Promise<Entries>;
  /** Saves the entries onto the map as it is now (a null removes one). */
  merge(name: string, patch: Entries): Promise<Entries>;
}

type LocalStore = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

export interface SharedMapOptions {
  /** This window's copy, in its browser storage. */
  key: string;
  /** Default: localStorage. */
  storage?: LocalStore | null;
  /** Default: the backend's files, in the app (none elsewhere: the copy only). */
  backend?: SharedBackend | null;
}

export interface SharedMap {
  /** This window's copy. */
  all(): Entries;
  /** Sets entries (undefined or null removes one): here at once, then in
   *  the file, which the promise waits for (it can't fail). */
  set(patch: Entries): Promise<void>;
  /** Reads the file again: resolves once it's read, or couldn't be. */
  refresh(): Promise<void>;
  /** `fn` runs when another window's save changed the copy. */
  subscribe(fn: (all: Entries) => void): () => void;
}

const files: SharedBackend = {
  read: (name) => invoke<Entries | null>('shared_state_read', { name }),
  start: (name, entries) => invoke<Entries>('shared_state_start', { name, entries }),
  merge: (name, patch) => invoke<Entries>('shared_state_merge', { name, patch }),
};

const inApp = () => typeof globalThis === 'object' && '__TAURI_INTERNALS__' in globalThis;

const isEntries = (v: unknown): v is Entries => !!v && typeof v === 'object' && !Array.isArray(v);

function patched(base: Entries, patch: Entries): Entries {
  const out = { ...base };
  for (const [key, value] of Object.entries(patch)) {
    if (value === undefined || value === null) delete out[key];
    else out[key] = value;
  }
  return out;
}

/** The same JSON, whatever order its keys are in (the backend sorts them). */
function same(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (!a || !b || typeof a !== 'object' || typeof b !== 'object') return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const keys = Object.keys(a);
  if (keys.length !== Object.keys(b).length) return false;
  return keys.every(
    (k) => Object.prototype.hasOwnProperty.call(b, k) && same((a as Entries)[k], (b as Entries)[k])
  );
}

/** The maps this window follows: read again as it comes to the front. */
const followed = new Set<SharedMap>();
let following = false;
function followFocus() {
  if (following || typeof window === 'undefined' || !window.addEventListener) return;
  following = true;
  window.addEventListener('focus', () => {
    for (const map of followed) void map.refresh();
  });
}

/** The map `name`, shared by the windows (see the top of this file). */
export function sharedMap(name: string, options: SharedMapOptions): SharedMap {
  const storage =
    options.storage !== undefined
      ? options.storage
      : typeof localStorage === 'undefined'
        ? null
        : localStorage;
  const backend = options.backend !== undefined ? options.backend : inApp() ? files : null;
  const listeners = new Set<(all: Entries) => void>();
  /** Set here and not in the file yet, in order. */
  const pending: Entries[] = [];
  /** The map was read: until it is, it's started from the copy if none is saved. */
  let known = false;
  let queue: Promise<void> = Promise.resolve();

  const readCopy = (): Entries => {
    try {
      const parsed: unknown = JSON.parse(storage?.getItem(options.key) ?? '{}');
      return isEntries(parsed) ? parsed : {};
    } catch {
      return {};
    }
  };
  let copy = readCopy();

  function keepCopy() {
    try {
      if (Object.keys(copy).length === 0) storage?.removeItem(options.key);
      else storage?.setItem(options.key, JSON.stringify(copy));
    } catch {
      // Not kept here: the file has it.
    }
  }

  /** The map as saved, with what's still on its way there on top. */
  function saw(saved: Entries) {
    const next = pending.reduce(patched, saved);
    if (same(next, copy)) return;
    copy = next;
    keepCopy();
    for (const fn of listeners) fn(copy);
  }

  async function sync(reread: boolean) {
    if (!backend) return;
    if (!known || reread) {
      // None saved yet (an update from before, or it's gone): it starts as
      // this window's copy.
      const saved = (known ? await backend.read(name) : null) ?? (await backend.start(name, copy));
      known = true;
      saw(saved);
    }
    if (pending.length === 0) return;
    const sending = pending.splice(0);
    try {
      saw(await backend.merge(name, Object.assign({}, ...sending)));
    } catch (err) {
      pending.unshift(...sending);
      throw err;
    }
  }

  function enqueue(reread: boolean): Promise<void> {
    const run = queue
      .then(() => sync(reread))
      .catch((err) => console.warn(`Shared ${name} not saved or read:`, err));
    queue = run;
    return run;
  }

  const map: SharedMap = {
    all: () => copy,
    set(patch) {
      copy = patched(copy, patch);
      keepCopy();
      pending.push(Object.fromEntries(Object.entries(patch).map(([k, v]) => [k, v ?? null])));
      return enqueue(false);
    },
    refresh: () => enqueue(true),
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
  };
  if (backend) {
    // Read now: another window may have saved since this copy was.
    void enqueue(false);
    followed.add(map);
    followFocus();
  }
  return map;
}
