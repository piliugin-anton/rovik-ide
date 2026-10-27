// switchTarget.ts
//
// Opening a target in this window — a folder (a project), or a workspace's
// folders — and what happens in which order
// (docs/design/workspaces-plan.md, 3.3). The window's own state (its tabs,
// tree, terminals and chats) is App.tsx's: this takes the calls it needs,
// so the order, and what a refused switch leaves as it was, can be read and
// tested on their own.
import {
  openFolders,
  withoutFolder,
  workspaceAsTarget,
  type OpenFolder,
  type WindowTarget,
} from './openFolders';
import type { Workspace } from './chatApi';
import type { ClaimTarget } from './instances';

/** What a switch needs of the window. */
export interface SwitchCalls {
  /**
   * Whether the target may open here, without claiming it: another window
   * with it (or a copy sharing its chats) is brought to the front, and
   * false is returned.
   */
  check: (target: WindowTarget | null) => Promise<boolean>;
  /** The tabs with unsaved text, settled (saved or let go, as the user says). False: the user cancelled. */
  settle: () => Promise<boolean>;
  /** The tabs shown now, kept for the project they're of. */
  keepTabs: () => void | Promise<void>;
  /** Claims the target for this window; false when another window took it meanwhile. */
  claim: (target: WindowTarget | null) => Promise<boolean>;
  /** Makes it the target shown, with its tabs as it was left. */
  show: (target: WindowTarget | null) => void | Promise<void>;
  /** The folders that are there, each resolved (`chat_open_project`), all at once. */
  resolve: (folders: OpenFolder[]) => void | Promise<void>;
  /** Whether a folder is there now (a drive that isn't mounted isn't). */
  exists: (path: string) => Promise<boolean>;
}

/** What to claim for a target (instances.ts): a folder, or a workspace by its ID. */
export function claimTargetOf(target: WindowTarget | null): ClaimTarget | null {
  if (!target) return null;
  return target.kind === 'folder'
    ? { kind: 'folder', path: target.path }
    : { kind: 'workspace', id: target.id };
}

/**
 * A target's folders, each marked when it isn't there. A folder that's gone
 * (deleted, or on a drive or share that isn't mounted) is shown and can't
 * be worked in, and its project isn't resolved: it would be a project for a
 * path that isn't there (D20).
 */
export async function presentFolders(
  target: WindowTarget | null,
  exists: SwitchCalls['exists']
): Promise<OpenFolder[]> {
  const folders = openFolders(target);
  const there = await Promise.all(folders.map((folder) => exists(folder.path).catch(() => false)));
  return folders.map((folder, i) => ({ ...folder, missing: !there[i] }));
}

/**
 * The folders of a target that are there, which are the ones resolved: a
 * missing folder (deleted, or on a drive that isn't mounted) is shown and
 * asks nothing, since resolving it would be a project for a path that isn't
 * there (D20).
 */
export function resolvable(open: readonly OpenFolder[]): OpenFolder[] {
  return open.filter((folder) => !folder.missing);
}

/** What a window's start needs of it. */
export interface StartCalls {
  /** Shows the target: its tabs, its tree, its terminal panel. */
  show: (target: WindowTarget | null) => void | Promise<void>;
  /** Resolves the folders it has, each that's there its own project (`resolveFolders`). */
  resolve: (open: OpenFolder[]) => void | Promise<void>;
  exists: (path: string) => Promise<boolean>;
}

/**
 * A window's start: what it opens — a workspace, a folder, or nothing when
 * nothing was asked for and nothing was left (a first start, a fresh **New
 * window**, or a last project another window has) — shown, and then its
 * folders resolved. With nothing open there are no folders to resolve, and
 * its chats are the ones without a project: resolving none is what claims
 * them for this window, so a window that opens with nothing still has its
 * chats (docs/design/workspaces-plan.md, 3.3, D6).
 */
export async function openAtStart(target: WindowTarget | null, calls: StartCalls): Promise<void> {
  await calls.show(target);
  await calls.resolve(await presentFolders(target, calls.exists));
}

/**
 * Opens `target` in this window (null: no folder open). False when it didn't
 * open here: another window has it and was brought to the front, or the
 * user kept their unsaved tabs. Nothing of this window changed then.
 */
export async function switchTarget(
  target: WindowTarget | null,
  calls: SwitchCalls
): Promise<boolean> {
  // 1. Another window has it (or a copy sharing its chats): that one comes
  // to the front, and this window stays as it is.
  if (!(await calls.check(target))) return false;
  // 2. The tabs with unsaved text are settled first: they close.
  if (!(await calls.settle())) return false;
  // The project left keeps its tabs for coming back to it.
  await calls.keepTabs();
  // 3. Claimed, every key or none: another window may have taken it while
  // the user chose.
  if (!(await calls.claim(target))) return false;
  // 4. Shown, with its tabs as the window that had it last left them.
  await calls.show(target);
  // 5. Each folder that's there is resolved. A missing one isn't, and is
  // tried again when the window comes to the front, or by its row's
  // **Try again**.
  await calls.resolve(await presentFolders(target, calls.exists));
  return true;
}

/** What removing a folder, or locating it at a new place, needs of the window. */
export interface FolderChangeCalls {
  /** Settles the unsaved tabs of the folder that goes; false: the user cancelled. */
  settle: (folder: OpenFolder) => Promise<boolean>;
  /** Removes the folder, by its name, from the workspace: refused for the last one. */
  remove: (workspaceId: string, name: string) => Promise<void>;
  /** Whether the new place may open here: another window with it comes to the front. */
  check: (path: string) => Promise<boolean>;
  /** The folder is at `to` from now on, as the backend keeps the workspace then. */
  relocate: (workspaceId: string, name: string, to: string) => Promise<Workspace>;
  /** The folder's tabs, pointed at its new place. */
  follow: (from: string, to: string) => void;
  /** Claims the target for this window; false when another window took it meanwhile. */
  claim: (target: WindowTarget) => Promise<boolean>;
  /** Makes it the target shown. */
  show: (target: WindowTarget) => void | Promise<void>;
  /** The folder at `path` resolved, now that it's there. */
  resolve: (path: string) => void | Promise<void>;
}

/**
 * Removes a folder from a workspace (docs/design/workspaces-plan.md C01): its
 * unsaved tabs are settled, it leaves the workspace, and the window shows the
 * workspace without it. Nothing on disk is touched.
 *
 * What it returns: whether the workspace changed. False only when nothing
 * did — the user kept the folder's unsaved tabs, or the target is a project
 * (the list of one, with no folder row to remove) — and a refused removal (the
 * last folder) throws, with the workspace as it was.
 *
 * The claim follows the backend's change, because the backend reads a
 * workspace's folders from the database when it claims it. Should another
 * window have taken the workspace meanwhile, the window still shows what the
 * database keeps — the folder is gone, and a row for it would be a lie — and
 * the claim itself has already said so (instances.ts's "open elsewhere"
 * notice); a later switch claims or refuses as usual.
 */
export async function removeFolder(
  target: WindowTarget,
  folder: OpenFolder,
  calls: FolderChangeCalls
): Promise<boolean> {
  // A project is the list of one: it has no folder row to remove.
  if (target.kind !== 'workspace') return false;
  if (!(await calls.settle(folder))) return false;
  await calls.remove(target.id, folder.name);
  const next = withoutFolder(target, folder.name);
  await calls.claim(next);
  await calls.show(next);
  return true;
}

/**
 * Locate…: the folder named `folder` is at `to` from now on. The new place is
 * checked first, so the database never points at a folder another window
 * holds; then the backend relocates it (D5), its tabs follow it, and the
 * window shows the workspace as the backend now keeps it.
 *
 * What it returns: whether anything moved. False only when nothing did — the
 * new place is another window's, or the target is a project. As in
 * `removeFolder`, the claim comes after the relocation (the backend claims a
 * workspace's folders as the database has them) and a refusal doesn't leave
 * the window showing the old place: the workspace has moved, the claim's
 * notice says another window has it, and a later switch claims or refuses.
 */
export async function locateFolder(
  target: WindowTarget,
  folder: OpenFolder,
  to: string,
  calls: FolderChangeCalls
): Promise<boolean> {
  // A project is the list of one: it has no folder row to locate.
  if (target.kind !== 'workspace') return false;
  if (!(await calls.check(to))) return false;
  const next = workspaceAsTarget(await calls.relocate(target.id, folder.name, to));
  calls.follow(folder.path, to);
  await calls.claim(next);
  await calls.show(next);
  await calls.resolve(to);
  return true;
}
