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
    const root = rootPath();
    if (!root || loading().has(path)) return;

    setLoading((prev) => new Set(prev).add(path));
    try {
      const entries = await readDirEntries(root, path);
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

  function toggle(entry: { path: string; isDir: boolean; ignored?: boolean }) {
    if (!entry.isDir) return;
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(entry.path)) {
        next.delete(entry.path);
      } else {
        next.add(entry.path);
        // The backend's watcher skips gitignored paths entirely (see
        // is_ignored in filesystem.rs), so fs-changes events never arrive
        // for anything under an ignored directory. We can't rely on the
        // cache being fresh for those — force a re-fetch on every expand
        // instead of only when uncached.
        if (entry.ignored || !cache().has(entry.path)) {
          void loadDir(entry.path);
        }
      }
      return next;
    });
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

  return { rows, expanded, toggle, refresh };
}
