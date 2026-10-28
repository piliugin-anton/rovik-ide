import { createMemo, createSignal, createEffect } from 'solid-js';
import { readDirEntries } from './commands';
import { isAtOrUnder, parentOf } from './fileTools';
import { innermostFolder } from './openFolders';
import type { DirEntryInfo, TreeRow } from '../types';

/** A folder the tree shows. */
export interface TreeRoot {
  path: string;
  /** The row's name, when it has one (the folder's stored name). */
  name: string;
  /** Shown as a row at the top: a workspace's folders are, a project's one root isn't. */
  row: boolean;
  /** The backend's id for the folder, once resolved: the id of its row. */
  id?: string;
  /** The folder isn't there: its row says so, and none of it is read (D20). */
  missing: boolean;
}

/**
 * The selected rows that copy, cut and delete act on: a workspace folder's
 * own row is never one of them (it leaves the workspace instead, D9).
 */
export function withoutFolderRows(ids: ReadonlySet<string>, rows: readonly TreeRow[]): string[] {
  const folderRows = new Set(rows.filter((row) => row.folderRow).map((row) => row.id));
  return [...ids].filter((id) => !folderRows.has(id));
}

/**
 * The selected rows that an action acts on (delete, rename, the menu's
 * counts). A window folder's own row is left out: it leaves the workspace
 * instead (D9), so no action may take it as its target.
 */
export function actionableRows(rows: readonly TreeRow[]): TreeRow[] {
  return rows.filter((row) => !row.folderRow);
}

/**
 * Lazy, cache-backed file tree.
 *
 * Directories are only read from disk when expanded, and only one level at a
 * time — we never walk the whole project up front. Combined with row
 * virtualization in <FileTree>, this is what keeps the UI responsive on
 * large projects (tens of thousands of files): work scales with what's
 * expanded and visible, not with total project size.
 *
 * It takes the window's folders, each read with its own root (a listing is
 * read through the folder it's in), and with one folder shown as no row the
 * rows are a single project's.
 */
export function createFileTreeController(roots: () => readonly TreeRoot[]) {
  // path -> children, or undefined if never fetched
  const [cache, setCache] = createSignal<Map<string, DirEntryInfo[]>>(new Map());
  const [expanded, setExpanded] = createSignal<Set<string>>(new Set());
  const [loading, setLoading] = createSignal<Set<string>>(new Set());
  const [lastRoots, setLastRoots] = createSignal<string | null>(null);
  // Saved expansions, per folder, applied the next time that folder is one of
  // the tree's roots.
  const [pendingExpanded, setPendingExpandedState] = createSignal<
    { root: string; paths: string[] }[]
  >([]);

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

  /** The folder that holds `path`: the innermost of the tree's roots. */
  function rootOf(path: string): string | null {
    return innermostFolder(
      roots().map((root) => root.path),
      path
    );
  }

  /** Whether `path` is one of the tree's roots, which are never collapsed. */
  function isRoot(path: string): boolean {
    return roots().some((root) => root.path === path);
  }

  /**
   * Whether `path` is in a folder that isn't there: nothing of it is read,
   * so its row's Try again or Locate… is what brings it back (D20).
   */
  function isMissing(path: string): boolean {
    const at = rootOf(path);
    return at !== null && roots().some((root) => root.path === at && root.missing);
  }

  async function loadDir(path: string) {
    const root = rootOf(path);
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

  // Register saved expansion sets to apply the next time their folders are
  // the tree's roots. Call this *before* the folders change, so they're in
  // place when the reset effect below runs.
  function presetExpanded(root: string, paths: string[]) {
    setPendingExpandedState((prev) => [...prev.filter((p) => p.root !== root), { root, paths }]);
  }

  // (Re)load the roots whenever the window's folders change, and reset state.
  createEffect(() => {
    const current = roots();
    const key = current.map((root) => root.path).join('\n');
    if (key !== lastRoots()) {
      setCache(new Map<string, DirEntryInfo[]>());
      const pending = pendingExpanded();
      const next = new Set<string>();
      for (const root of current) {
        // A root's own listing is always read: its row shows it open, and a
        // project's root has no row to open. A folder that isn't there has
        // nothing to read.
        if (!root.missing) next.add(root.path);
        for (const saved of pending.filter((p) => p.root === root.path)) {
          for (const path of saved.paths) next.add(path);
        }
      }
      setExpanded(next);
      setPendingExpandedState([]);
      setLastRoots(key);
    }
    for (const root of current) {
      if (!root.missing && !cache().has(root.path)) {
        void loadDir(root.path);
      }
    }
    // Fetch any expanded directory that isn't cached yet — covers both
    // manual toggling and a restored expansion set, with no ordering
    // requirement between parent/child. A folder that isn't there is left
    // alone: reading it fails, and it can't be forgotten (a root never is),
    // so reading it again would go on for as long as the window is open.
    for (const p of expanded()) {
      if (!isMissing(p) && !cache().has(p) && !loading().has(p)) {
        void loadDir(p);
      }
    }
  });

  // A folder that was missing and is there now is listed open, as a root
  // that was never missing is. Not a reset: the other folders' expansions stay.
  let missingBefore = new Set<string>();
  createEffect(() => {
    const current = roots();
    const back = current.filter((root) => !root.missing && missingBefore.has(root.path));
    missingBefore = new Set(current.filter((root) => root.missing).map((root) => root.path));
    if (back.length === 0) return;
    setExpanded((prev) => {
      const next = new Set(prev);
      for (const root of back) next.add(root.path);
      return next;
    });
  });

  /**
   * A file or folder renamed or moved: what was expanded in it stays
   * expanded at its new place, instead of the folder closing. Before its
   * listings reload, which forget expanded folders that aren't there any
   * more.
   */
  function moved(from: string, to: string) {
    setExpanded((prev) => {
      const inside = [...prev].filter((p) => isAtOrUnder(p, from));
      if (inside.length === 0) return prev;
      const next = new Set(prev);
      for (const p of inside) {
        next.delete(p);
        next.add(to + p.slice(from.length));
      }
      return next;
    });
    // Its contents are read again at the new place.
    setCache((prev) => {
      const stale = [...prev.keys()].filter((p) => isAtOrUnder(p, from));
      if (stale.length === 0) return prev;
      const next = new Map(prev);
      for (const p of stale) next.delete(p);
      return next;
    });
  }

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

  /** Drops a folder and everything under it from the expanded set and the cache. */
  function forget(path: string) {
    if (isRoot(path)) return; // a folder of the window is never collapsed
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

  function refresh(changedPath?: string) {
    if (roots().length === 0) return;

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
   * Reloads every loaded directory on the way from `path` up to the folder
   * it's in. For a path whose parent folders were just (re)created, e.g. by
   * saving a tab whose folder had been deleted: each level gains an entry.
   */
  function refreshAncestors(path: string) {
    const root = rootOf(path);
    if (!root) return;
    const rootTrimmed = root.replace(/[\\/]+$/, '');
    let dir = parentOf(path);
    while (dir && dir.replace(/[\\/]+$/, '').length >= rootTrimmed.length) {
      if (cache().has(dir)) void loadDir(dir);
      if (dir === rootTrimmed) break;
      dir = parentOf(dir);
    }
  }

  const rows = createMemo<TreeRow[]>(() => {
    const current = roots();
    if (current.length === 0) return [];
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

    for (const root of current) {
      if (!root.row) {
        walk(root.path, 0);
        continue;
      }
      // A folder row, keyed by its path: rows of the same file seen through
      // two folders (a link) keep their own, and a folder's row is never
      // renamed, deleted or moved (D9), so its path is its id.
      out.push({
        id: root.id ?? root.path,
        path: root.path,
        name: root.name,
        isDir: true,
        depth: 0,
        folderRow: true,
        missing: root.missing,
      });
      // Nothing of a folder that isn't there is listed: there's nothing to read.
      if (!root.missing && currentExpanded.has(root.path)) walk(root.path, 1);
    }
    return out;
  });

  return {
    rows,
    expanded,
    toggle,
    refresh,
    refreshAncestors,
    files,
    presetExpanded,
    moved,
    /** The folder that holds `path`, among the tree's. */
    rootOf,
  };
}
