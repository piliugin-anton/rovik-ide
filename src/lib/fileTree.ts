import { createMemo, createSignal, createEffect } from 'solid-js';
import { readDirEntries } from './commands';
import type { DirEntryInfo, TreeRow } from '../types';

/**
 * Lazy, cache-backed file tree.
 *
 * Directories are only read from disk when expanded, and only one level at a
 * time — we never walk the whole project up front. Combined with row
 * virtualization in <FileTree>, this is what keeps the UI responsive on
 * large projects (tens of thousands of files): work scales with what's
 * expanded and visible, not with total project size.
 */
export function createFileTreeController(rootPath: () => string | null) {
  // path -> children, or undefined if never fetched
  const [cache, setCache] = createSignal<Map<string, DirEntryInfo[]>>(new Map());
  const [expanded, setExpanded] = createSignal<Set<string>>(new Set());
  const [loading, setLoading] = createSignal<Set<string>>(new Set());
  const [lastRootPath, setLastRootPath] = createSignal<string | null>(null);
  const [pendingExpanded, setPendingExpandedState] = createSignal<{
    root: string;
    paths: string[];
  } | null>(null);

  const files = createMemo<TreeRow[]>(() => {
    const currentCache = cache();
    const out: TreeRow[] = [];

    for (const children of currentCache.values()) {
      for (const child of children) {
        if (!child.is_dir) {
          out.push({
            id: child.id,
            path: child.path,
            name: child.name,
            isDir: false,
            depth: 0,
            ignored: child.ignored,
          });
        }
      }
    }

    return out;
  });

  // Directories asked to reload while a read of them was already running.
  // Dropping those requests lost tree updates: a file created during the
  // read never showed up until something else touched that folder.
  const reloadQueued = new Set<string>();

  async function loadDir(path: string) {
    const root = rootPath();
    if (!root) return;
    if (loading().has(path)) {
      reloadQueued.add(path);
      return;
    }

    setLoading((prev) => new Set(prev).add(path));
    try {
      const entries = await readDirEntries(root, path);
      setCache((prev) => {
        const next = new Map(prev);
        next.set(path, entries);
        return next;
      });
      // Expanded subfolders that aren't in the listing any more were deleted
      // or renamed: forget them, so they aren't read (and fail) later.
      const present = new Set(entries.filter((e) => e.is_dir).map((e) => e.path));
      const gone = [...expanded()].filter((p) => parentOf(p) === path && !present.has(p));
      for (const p of gone) forget(p);
    } catch (err) {
      // Usually deleted since it was expanded (or since the expansion state
      // was saved). Forget it: the effect below retries every expanded
      // folder that isn't cached, so keeping it would retry forever.
      console.warn(`Can't read ${path}, collapsing it:`, err);
      forget(path);
    } finally {
      setLoading((prev) => {
        const next = new Set(prev);
        next.delete(path);
        return next;
      });
      if (reloadQueued.delete(path)) void loadDir(path);
    }
  }

  // Register a saved expansion set to apply the next time `root` becomes
  // the active root. Call this *before* setRootPath so it's in place when
  // the reset effect below runs.
  function presetExpanded(root: string, paths: string[]) {
    setPendingExpandedState({ root, paths });
  }

  // (Re)load the root whenever the project folder changes, and reset state.
  createEffect(() => {
    const root = rootPath();
    if (root !== lastRootPath()) {
      setCache(new Map<string, DirEntryInfo[]>());
      const pending = pendingExpanded();
      if (pending && pending.root === root) {
        setExpanded(new Set([root, ...pending.paths]));
        setPendingExpandedState(null);
      } else if (root) {
        setExpanded(new Set([root]));
      }
      setLastRootPath(root);
    }
    if (root) {
      if (!cache().has(root)) {
        void loadDir(root);
      }
      // Fetch any expanded directory that isn't cached yet — covers both
      // manual toggling and a restored expansion set, with no ordering
      // requirement between parent/child.
      for (const p of expanded()) {
        if (p !== root && !cache().has(p) && !loading().has(p)) {
          void loadDir(p);
        }
      }
    }
  });

  function toggle(entry: { path: string; isDir: boolean; ignored?: boolean }) {
    if (!entry.isDir) return;
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(entry.path)) {
        next.delete(entry.path);
      } else {
        next.add(entry.path);
        if (entry.ignored || !cache().has(entry.path)) {
          void loadDir(entry.path);
        }
      }
      return next;
    });
  }

  /** Whether `path` is `base` or inside it. */
  function isAtOrUnder(path: string, base: string) {
    if (path === base) return true;
    const trimmed = base.replace(/[\\/]+$/, '');
    return path.startsWith(`${trimmed}/`) || path.startsWith(`${trimmed}\\`);
  }

  /** Drops a folder and everything under it from the expanded set and the cache. */
  function forget(path: string) {
    if (path === rootPath()) return; // the root is never collapsed
    setExpanded((prev) => {
      const doomed = [...prev].filter((p) => isAtOrUnder(p, path));
      if (doomed.length === 0) return prev;
      const next = new Set(prev);
      for (const p of doomed) next.delete(p);
      return next;
    });
    setCache((prev) => {
      const doomed = [...prev.keys()].filter((p) => isAtOrUnder(p, path));
      if (doomed.length === 0) return prev;
      const next = new Map(prev);
      for (const p of doomed) next.delete(p);
      return next;
    });
    reloadQueued.delete(path);
  }

  // path.dirname-equivalent for both '/' and '\' separators, since Tauri
  // paths can be POSIX or Windows depending on platform.
  function parentOf(path: string): string | null {
    const trimmed = path.replace(/[\\/]+$/, '');
    const idx = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
    if (idx < 0) return null;
    return idx === 0 ? trimmed.slice(0, 1) : trimmed.slice(0, idx);
  }

  function refresh(changedPath?: string) {
    const root = rootPath();
    if (!root) return;

    if (!changedPath) {
      for (const dirPath of cache().keys()) void loadDir(dirPath);
      return;
    }

    // If changedPath is itself a directory we've already loaded (e.g. it was
    // modified in place, or removed and we need to pick that up), refresh it.
    if (cache().has(changedPath)) {
      void loadDir(changedPath);
    }

    // Always refresh whatever directory *contains* changedPath — that's the
    // listing that needs to gain or lose this entry on create/remove.
    const parent = parentOf(changedPath);
    if (parent && cache().has(parent)) {
      void loadDir(parent);
    }
  }

  /**
   * Reloads every loaded directory on the way from `path` up to the root.
   * For a path whose parent folders were just (re)created, e.g. by saving a
   * tab whose folder had been deleted: each level gains an entry.
   */
  function refreshAncestors(path: string) {
    const root = rootPath();
    if (!root) return;
    const rootTrimmed = root.replace(/[\\/]+$/, '');
    let dir = parentOf(path);
    while (dir && dir.length >= rootTrimmed.length) {
      if (cache().has(dir)) void loadDir(dir);
      if (dir === rootTrimmed) break;
      dir = parentOf(dir);
    }
  }

  const rows = createMemo<TreeRow[]>(() => {
    const root = rootPath();
    if (!root) return [];
    const out: TreeRow[] = [];
    const currentCache = cache();
    const currentExpanded = expanded();
    const currentLoading = loading();

    function walk(path: string, depth: number) {
      const children = currentCache.get(path);
      if (!children) return;
      for (const child of children) {
        out.push({
          id: child.id,
          path: child.path,
          name: child.name,
          isDir: child.is_dir,
          depth,
          ignored: child.ignored,
        });
        if (child.is_dir && currentExpanded.has(child.path)) {
          if (currentCache.has(child.path)) {
            walk(child.path, depth + 1);
          } else if (currentLoading.has(child.path)) {
            out.push({
              id: `${child.id}::loading`,
              path: `${child.path}::loading`,
              name: 'Loading...',
              isDir: false,
              depth: depth + 1,
              loading: true,
              ignored: child.ignored,
            });
          }
        }
      }
    }

    walk(root, 0);
    return out;
  });

  return { rows, expanded, toggle, refresh, refreshAncestors, files, presetExpanded };
}
