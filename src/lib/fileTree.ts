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

  async function loadDir(path: string) {
    if (loading().has(path)) return;
    setLoading((prev) => new Set(prev).add(path));
    try {
      const entries = await readDirEntries(path);
      console.log('[debug] loaded', path, '->', entries.length, 'entries', entries);
      setCache((prev) => {
        const next = new Map(prev);
        next.set(path, entries);
        return next;
      });
    } catch (err) {
      console.error(`Failed to read directory ${path}:`, err);
    } finally {
      setLoading((prev) => {
        const next = new Set(prev);
        next.delete(path);
        return next;
      });
    }
  }

  // (Re)load the root whenever the project folder changes, and reset state.
  createEffect(() => {
    const root = rootPath();
    // Only reset cache and loading state if root changed to a new value
    if (root !== lastRootPath()) {
      setCache(new Map<string, DirEntryInfo[]>()); // reset on actual change
      setExpanded(new Set<string>()); // reset on actual change
      setLastRootPath(root);
    }
    if (root) {
      // Only load if not already cached (avoid infinite reload loop)
      if (!cache().has(root)) {
        setExpanded(new Set([root])); // expand root only when first loading
        void loadDir(root);
      }
    }
  });

  function toggle(entry: { path: string; isDir: boolean }) {
    if (!entry.isDir) return;
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(entry.path)) {
        next.delete(entry.path);
      } else {
        next.add(entry.path);
        if (!cache().has(entry.path)) void loadDir(entry.path);
      }
      return next;
    });
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
        out.push({ path: child.path, name: child.name, isDir: child.is_dir, depth });
        if (child.is_dir && currentExpanded.has(child.path)) {
          if (currentCache.has(child.path)) {
            walk(child.path, depth + 1);
          } else if (currentLoading.has(child.path)) {
            out.push({
              path: `${child.path}::loading`,
              name: 'Loading…',
              isDir: false,
              depth: depth + 1,
              loading: true,
            });
          }
        }
      }
    }

    walk(root, 0);
    return out;
  });

  return { rows, expanded, toggle };
}
