// editorTabs.ts
//
// The editor's tabs, in the order the tab strip shows them: files (and the
// read-only versions of files: stashes, commits, comparisons) and browser
// tabs, which show a web page (BrowserView.tsx, lib/browserTabs.ts). App
// keeps them in one list. What works on files keeps working on the files
// alone (`withFiles` puts its changes back among the browser tabs), and a
// browser tab is opened, navigated and loaded again here.
import type { OpenFile } from '../types';

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
