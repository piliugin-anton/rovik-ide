// editorTabs.ts
//
// The editor's tabs, in the order the tab strip shows them: files (and the
// read-only versions of files: stashes, commits, comparisons) and browser
// tabs, which show a web page (BrowserView.tsx, lib/browserTabs.ts). App
// keeps them in one list. What works on files keeps working on the files
// alone (`withFiles` puts its changes back among the browser tabs), and a
// browser tab is opened, navigated and loaded again here.
import type { OpenFile } from '../types';
import { isAtOrUnder, toRelativePath } from './fileTools';
import { innermostFolder } from './openFolders';

/** A tab showing a web page. */
export interface BrowserTab {
  kind: 'browser';
  /** Its key among the tabs (`browser:<n>`), as a file's path is its. */
  path: string;
  /** The page's address, as its address bar shows it. */
  address: string;
  /** How many times it was loaded again: each time is a new tab object. */
  loads: number;
}

export type EditorTab = OpenFile | BrowserTab;

export const isBrowserTab = (tab: EditorTab): tab is BrowserTab =>
  'kind' in tab && tab.kind === 'browser';

export const isFileTab = (tab: EditorTab): tab is OpenFile => !isBrowserTab(tab);

/**
 * The tabs once their files are `files`: each file in the place of its
 * tab before, found by id or path (a renamed file keeps its id, a
 * re-created one its path), new ones last, gone ones gone; the browser
 * tabs stay where they are.
 */
export function withFiles(tabs: readonly EditorTab[], files: readonly OpenFile[]): EditorTab[] {
  const left = [...files];
  const kept = tabs.flatMap((tab): EditorTab[] => {
    if (isBrowserTab(tab)) return [tab];
    const at = left.findIndex((f) => f.id === tab.id || f.path === tab.path);
    return at < 0 ? [] : left.splice(at, 1);
  });
  return [...kept, ...left];
}

/**
 * The tabs with `address` open in one: the browser tab already showing it,
 * loaded again, else a new one last, keyed `newPath()`. With its key.
 */
export function withPage(
  tabs: readonly EditorTab[],
  address: string,
  newPath: () => string
): { tabs: EditorTab[]; path: string } {
  const open = tabs.find((t): t is BrowserTab => isBrowserTab(t) && t.address === address);
  if (open) return { tabs: reloaded(tabs, open.path), path: open.path };
  const tab: BrowserTab = { kind: 'browser', path: newPath(), address, loads: 0 };
  return { tabs: [...tabs, tab], path: tab.path };
}

/** The tabs with the browser tab `path` showing `address`, loaded anew. */
export function navigated(tabs: readonly EditorTab[], path: string, address: string): EditorTab[] {
  return tabs.map((t) =>
    isBrowserTab(t) && t.path === path ? { ...t, address, loads: t.loads + 1 } : t
  );
}

/** The tabs with the browser tab `path` loaded again. */
export function reloaded(tabs: readonly EditorTab[], path: string): EditorTab[] {
  return tabs.map((t) => (isBrowserTab(t) && t.path === path ? { ...t, loads: t.loads + 1 } : t));
}

/** `path`'s segments inside `folder`, or null when it isn't in it. */
function segmentsInside(path: string, folder: string): string[] | null {
  if (!isAtOrUnder(path, folder)) return null;
  return toRelativePath(path, folder)
    .split(/[\\/]+/)
    .filter(Boolean);
}

/**
 * The files with unsaved text that would close: all of them, or with
 * `folder`, only those in it (a folder leaving the workspace closes its own
 * tabs, D9). A read-only file has nothing to save.
 */
export function unsavedIn<T extends { path: string; dirty: boolean; readOnly?: boolean }>(
  files: readonly T[],
  folder?: string
): T[] {
  return files.filter(
    (f) => f.dirty && !f.readOnly && (folder === undefined || isAtOrUnder(f.path, folder))
  );
}

/**
 * Where each tab whose name another tab has is, for its title (C03): the
 * shortest end of its parent path's segments that no other tab of that name
 * shares — "api" and "web" for two `package.json`, or "a/src" and "b/src"
 * for two `index.ts` in one project. With several folders the segments start
 * with the folder's stored name; with one they are the path inside it. Only
 * files in one of the window's folders take part (a `disk:` or `chat:` tab
 * has no place among them), and a name one tab has gets no hint.
 */
export function tabHints(
  tabs: readonly EditorTab[],
  folders: readonly { path: string; name: string }[]
): Map<string, string> {
  const hints = new Map<string, string>();
  const several = folders.length > 1;
  const paths = folders.map((folder) => folder.path);
  const byName = new Map<string, OpenFile[]>();
  for (const tab of tabs) {
    if (!isFileTab(tab)) continue;
    if (innermostFolder(paths, tab.path) === null) continue;
    const same = byName.get(tab.name) ?? [];
    same.push(tab);
    byName.set(tab.name, same);
  }
  for (const same of byName.values()) {
    if (same.length < 2) continue;
    const segments = new Map<string, string[]>();
    for (const tab of same) {
      const at = innermostFolder(paths, tab.path)!;
      const folder = folders.find((f) => f.path === at)!;
      // The file's own name last: the hint is where its folder is, not it.
      const inside = segmentsInside(tab.path, folder.path) ?? [];
      const parent = inside.slice(0, -1);
      segments.set(tab.path, several ? [folder.name, ...parent] : parent);
    }
    for (const tab of same) {
      const mine = segments.get(tab.path) ?? [];
      const others = same.filter((o) => o.path !== tab.path).map((o) => segments.get(o.path) ?? []);
      let hint = mine.join('/');
      for (let length = 1; length <= mine.length; length++) {
        const end = mine.slice(-length);
        if (!others.some((other) => endsWith(other, end))) {
          hint = end.join('/');
          break;
        }
      }
      hints.set(tab.path, hint);
    }
  }
  return hints;
}

/** Whether `segments` ends with `end`, segment by segment. */
function endsWith(segments: readonly string[], end: readonly string[]): boolean {
  if (end.length > segments.length) return false;
  const at = segments.length - end.length;
  return end.every((segment, i) => segments[at + i] === segment);
}
