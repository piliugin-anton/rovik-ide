// openFolders.ts
//
// What a window has open: a folder (a project), or a workspace, a named
// list of folders shown together (docs/design/workspaces-plan.md). The tree,
// the watcher, search, the Git panel and the chats take these folders, so a
// project is the list of one and everything built for a project works for a
// folder of a workspace.
import { isAbsolutePath, isAtOrUnder, toAbsolutePath } from './fileTools';
import { fileName } from './toolGroups';
import type { ProjectInfo, Workspace } from './chatApi';

/** A folder of a workspace, as the workspace keeps it: its stored name (D8) and path. */
export interface WorkspaceFolder {
  name: string;
  path: string;
}

/** What a window has open: a folder (a project), or a workspace. */
export type WindowTarget =
  | { kind: 'folder'; path: string }
  | { kind: 'workspace'; id: string; name: string; folders: WorkspaceFolder[] };

/** One folder the window has open. */
export interface OpenFolder {
  path: string;
  /** Its stored name in the workspace (D8); a project's folder name. */
  name: string;
  /** The project it is, once resolved (chat_open_project). */
  project?: ProjectInfo;
  /** The folder isn't there: deleted, or a drive or share that isn't mounted (D20). */
  missing: boolean;
}

/**
 * A workspace as a target: its folders as the database keeps them, each with
 * the stored name its chats write (D8).
 */
export function workspaceAsTarget(workspace: Workspace): WindowTarget {
  return {
    kind: 'workspace',
    id: workspace.id,
    name: workspace.name,
    folders: workspace.folders.map(({ name, path }) => ({ name, path })),
  };
}

/**
 * `target` without the folder named `name`: what a workspace is claimed as
 * once that folder has left it. A project is the list of one and has no
 * folder row to remove, so it's returned as it is.
 */
export function withoutFolder(target: WindowTarget, name: string): WindowTarget {
  if (target.kind === 'folder') return target;
  return { ...target, folders: target.folders.filter((folder) => folder.name !== name) };
}

/**
 * The folders a target has open, in the order it shows them. Whether a
 * folder is there is found when the target is opened, so none starts missing.
 */
export function openFolders(target: WindowTarget | null): OpenFolder[] {
  if (!target) return [];
  if (target.kind === 'folder') {
    return [{ path: target.path, name: fileName(target.path), missing: false }];
  }
  return target.folders.map((f) => ({ path: f.path, name: f.name, missing: false }));
}

/**
 * The folder among `paths` that holds `path`: the innermost one, or null when
 * it's in none of them. The files of a folder in a workspace are its own, so
 * its formatter's config, its Markdown `/` links and its diagnostics are its
 * own (D14).
 */
export function innermostFolder(paths: readonly string[], path: string): string | null {
  let found: string | null = null;
  for (const folder of paths) {
    if (!isAtOrUnder(path, folder)) continue;
    if (!found || folder.length > found.length) found = folder;
  }
  return found;
}

/** The open folder `path` is in, or null when it's in none of them. */
export function folderOf(folders: readonly OpenFolder[], path: string): OpenFolder | null {
  const at = innermostFolder(
    folders.map((folder) => folder.path),
    path
  );
  return at ? (folders.find((folder) => folder.path === at) ?? null) : null;
}

/**
 * `paths` split by the open folder each is in, one entry per folder in the
 * order given. The tree keeps its saved expansion per root, and its roots are
 * the window's folders: a workspace's session is kept under `workspace:<id>`,
 * which no root is named, so what it saved is registered folder by folder. A
 * path in none of them is left out.
 */
export function pathsByFolder(
  paths: readonly string[],
  folders: readonly string[]
): { folder: string; paths: string[] }[] {
  const by = new Map<string, string[]>(folders.map((folder) => [folder, []]));
  for (const path of paths) {
    const at = innermostFolder(folders, path);
    by.get(at ?? '')?.push(path);
  }
  return folders.map((folder) => ({ folder, paths: by.get(folder) ?? [] }));
}

/**
 * The folder the user is working in: the active tab's, else the tree's
 * selected row's, else the first, else none (nothing open). The tab decides
 * because it's the file being edited; a path in no folder of the window
 * (a tab of a folder removed, say) leaves it to the selection.
 */
export function activeFolder(
  folders: readonly OpenFolder[],
  activePath: string | null,
  selectedPath: string | null
): OpenFolder | null {
  if (activePath) {
    const at = folderOf(folders, activePath);
    if (at) return at;
  }
  if (selectedPath) {
    const at = folderOf(folders, selectedPath);
    if (at) return at;
  }
  return folders.length > 0 ? folders[0] : null;
}

/** The key a window's session is kept under (project-sessions, terminals, the commit draft). */
export function sessionKey(target: WindowTarget | null): string | null {
  if (!target) return null;
  return target.kind === 'folder' ? target.path : `workspace:${target.id}`;
}

/** The workspace a window's `sessionKey` names; null for a folder's (an absolute path) or none. */
export function workspaceIdOf(key: string | null | undefined): string | null {
  return key?.startsWith('workspace:') ? key.slice('workspace:'.length) : null;
}

/**
 * Whose chats a window lists and searches (docs/design/workspaces-plan.md,
 * 3.8): its owners.
 */
export interface ChatOwners {
  /**
   * What the window shows, as `sessionKey` names it (null: nothing open).
   * Another one lists its chats from the start; the same one with other
   * projects keeps the open chat while its own project is among them.
   */
  target: string | null;
  /**
   * The projects whose chats are listed, each once: each folder's that's
   * there, and a workspace's own first. Null with no folder open: the chats
   * with no project.
   */
  projects: string[] | null;
}

/**
 * The window's owners, once each of its folders that's there knows its
 * project; undefined until then, as the chat panel waits for a project's
 * folder to resolve. A missing folder isn't resolved, so it has none and
 * waits for nothing (D20). With nothing open, the chats with no project are
 * the window's once they're claimed for it (`noProjectReady`).
 */
export function chatOwners(
  target: WindowTarget | null,
  folders: readonly OpenFolder[],
  noProjectReady: boolean
): ChatOwners | undefined {
  if (!target) return noProjectReady ? { target: null, projects: null } : undefined;
  const projects: string[] = target.kind === 'workspace' ? [target.id] : [];
  for (const folder of folders) {
    if (folder.missing) continue;
    if (!folder.project) return undefined;
    if (!projects.includes(folder.project.id)) projects.push(folder.project.id);
  }
  return { target: sessionKey(target), projects };
}

/** Whether `owners` list the chats of `projectId` (null: the chats with no project). */
export function listedBy(owners: ChatOwners, projectId: string | null): boolean {
  if (owners.projects === null) return projectId === null;
  return projectId !== null && owners.projects.includes(projectId);
}

/** Whether two sets of owners are the same: the same target, the same projects in order. */
export function sameOwners(a: ChatOwners | undefined, b: ChatOwners | undefined): boolean {
  if (a === undefined || b === undefined) return a === b;
  if (a.target !== b.target) return false;
  if (a.projects === null || b.projects === null) return a.projects === b.projects;
  return (
    a.projects.length === b.projects.length && a.projects.every((id, i) => b.projects![i] === id)
  );
}

/**
 * Where a chat works (3.2), as the window has it: a folder chat's folder,
 * or a workspace chat's folders (every one of the workspace's for All
 * folders) in the workspace's order, a missing one among them (D20). A
 * folder chat names its files as paths in its folder (`src/x.ts`), a
 * workspace chat as paths under its folders' names (`api/src/x.ts`).
 */
export interface ChatRoot {
  kind: 'folder' | 'workspace';
  folders: OpenFolder[];
}

/**
 * The folders a workspace chat checked, as the database keeps them (3.1):
 * `"all"`, or their names as JSON. A workspace's chat that names none
 * follows the workspace, as the backend reads it (`chat_folders`); names
 * that can't be read name no folder, and its run says why.
 */
function checkedNames(stored: string | null | undefined): string[] | 'all' {
  if (!stored || stored === 'all') return 'all';
  try {
    const names: unknown = JSON.parse(stored);
    if (Array.isArray(names) && names.every((name) => typeof name === 'string')) return names;
  } catch {
    // Not a list of names: no folder.
  }
  return [];
}

/**
 * Where `dialog` works (3.2). A chat of `workspace`, the window's (null: a
 * folder's window), works in the folders checked for it that the workspace
 * still has, or in each of them for All folders; any other chat in the
 * window's folder whose project it is. Null when that folder isn't open here
 * (it left the window, it's missing, it isn't resolved yet), or when the
 * chat has no project (no folder open).
 */
export function chatRootOf(
  dialog: { projectId?: string | null; folders?: string | null } | null | undefined,
  folders: readonly OpenFolder[],
  workspace: string | null
): ChatRoot | null {
  const id = dialog?.projectId;
  if (!id) return null;
  if (id === workspace) {
    const names = checkedNames(dialog.folders);
    return {
      kind: 'workspace',
      folders: folders.filter((folder) => names === 'all' || names.includes(folder.name)),
    };
  }
  const folder = folders.find((at) => !at.missing && at.project?.id === id);
  return folder ? { kind: 'folder', folders: [folder] } : null;
}

/**
 * The place a request about chat `dialog`'s files names them in (a file it
 * wrote, opened from its tool call): the chat's own. A chat the list doesn't
 * have (a subagent's, `undefined` here) is shown inside the open chat,
 * `open`, and works where it does. The window's first folder when neither
 * works here; null with no folder open.
 */
export function chatFolderFor(
  dialog: { projectId?: string | null; folders?: string | null } | undefined,
  open: { projectId?: string | null; folders?: string | null } | null | undefined,
  folders: readonly OpenFolder[],
  workspace: string | null
): ChatRoot | null {
  const root = chatRootOf(dialog ?? open, folders, workspace);
  if (root) return root;
  return folders.length > 0 ? { kind: 'folder', folders: [folders[0]] } : null;
}

/**
 * Which folders a chat works in, as its row and the label by its input say
 * them (3.8): its folder's name, a workspace chat's names checked, or "all"
 * for one that follows the workspace. Null when that can't be said: the
 * chat has no project, or doesn't work here.
 */
export function chatFolderNames(
  dialog: { projectId?: string | null; folders?: string | null },
  folders: readonly OpenFolder[],
  workspace: string | null
): string[] | 'all' | null {
  const root = chatRootOf(dialog, folders, workspace);
  if (!root) return null;
  if (root.kind === 'workspace' && checkedNames(dialog.folders) === 'all') return 'all';
  return root.folders.map((folder) => folder.name);
}

/**
 * The file a chat's path names (3.2): a folder chat's `src/x.ts` is in its
 * folder, a workspace chat's `api/src/x.ts` in its folder named `api`, and
 * an absolute path is itself. Null when the chat has no folder here, and
 * for a workspace chat's path whose first part names none of its folders.
 */
export function chatPathToAbsolute(root: ChatRoot | null, path: string): string | null {
  if (!root) return null;
  if (isAbsolutePath(path)) return path;
  if (root.kind === 'folder') return toAbsolutePath(path, root.folders[0].path);
  const [name, ...rest] = path.replace(/^\.[\\/]/, '').split(/[\\/]+/);
  const folder = root.folders.find((at) => at.name === name);
  return folder ? toAbsolutePath(rest.join('/'), folder.path) : null;
}

/**
 * The name a chat gives the file at `abs` (3.2): its path in the chat's
 * folder, under that folder's name in a workspace chat, with `/` between
 * its parts on every system, as the chat's tools take it. Null for a file
 * outside the chat's folders: the chat can't read it, so nothing names it
 * to the chat.
 */
export function chatPathOf(root: ChatRoot | null, abs: string): string | null {
  if (!root) return null;
  const folder = folderOf(root.folders, abs);
  if (!folder) return null;
  const slashed = (path: string) => path.replace(/\\/g, '/').replace(/\/+$/, '');
  const inFolder = slashed(abs).slice(slashed(folder.path).length).replace(/^\/+/, '');
  if (root.kind === 'folder') return inFolder;
  return inFolder ? `${folder.name}/${inFolder}` : folder.name;
}

/**
 * What a chat's run is sent (3.2): a folder chat's folder as its root; no
 * root for a workspace chat, whose folders the backend reads itself (4.1),
 * nor for a chat with no folder (none open). Why it can't run here instead:
 * its folder isn't open in this window, or one of a workspace chat's
 * folders is missing (D20).
 */
export function runPlace(
  dialog: { projectId?: string | null; folders?: string | null } | null | undefined,
  root: ChatRoot | null
): { root: string | null } | { why: string } {
  if (!root) {
    if (!dialog?.projectId) return { root: null };
    return {
      why: dialog.folders
        ? 'This chat’s workspace isn’t open here: open it to go on with the chat.'
        : 'This chat’s folder isn’t open here: open the folder it works in to go on with it.',
    };
  }
  if (root.kind === 'folder') return { root: root.folders[0].path };
  const missing = root.folders.filter((folder) => folder.missing).map((folder) => folder.name);
  const last = missing.pop();
  if (last === undefined) return { root: null };
  return {
    why: missing.length
      ? `${missing.join(', ')} and ${last} aren’t there: locate them, or remove them from the workspace, to go on with this chat.`
      : `${last} isn’t there: locate it, or remove it from the workspace, to go on with this chat.`,
  };
}
