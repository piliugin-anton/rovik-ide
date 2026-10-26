// openFolders.ts
//
// What a window has open: a folder (a project), or a workspace, a named
// list of folders shown together (docs/design/workspaces-plan.md). The tree,
// the watcher, search, the Git panel and the chats take these folders, so a
// project is the list of one and everything built for a project works for a
// folder of a workspace.
import { isAtOrUnder } from './fileTools';
import { fileName } from './toolGroups';
import type { ProjectInfo } from './chatApi';

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
