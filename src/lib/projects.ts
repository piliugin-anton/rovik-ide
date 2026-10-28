// projects.ts
//
// The open project (which chats are shown) and the list of projects, for
// the switcher above the sidebar tabs (ProjectSwitcher). The project a
// folder is comes from the backend (chat_db/projects.rs): by the ID file in
// it, so a rename or move keeps its chats. When a folder looks like a copy
// of a project, or like a moved project that lost its ID file, the user is
// asked once, in a notification; the answer is remembered there.
import { createSignal } from 'solid-js';
import { chatApi, type ProjectInfo, type ProjectSummary, type WorkspaceSummary } from './chatApi';
import { claimNoProject } from './instances';
import type { OpenFolder, WindowTarget } from './openFolders';
import { workspaceAsTarget } from './openFolders';
import { notify } from './notifications';

/** undefined: not resolved yet; null: no folder open. */
const [current, setCurrent] = createSignal<ProjectInfo | null | undefined>(undefined);
const [all, setAll] = createSignal<ProjectSummary[]>([]);
const [workspaces, setWorkspaces] = createSignal<WorkspaceSummary[]>([]);
/**
 * Why the chat panel shows no chats here, if it doesn't: the chats without
 * a project are another window's (lib/instances.ts), or the folder's
 * couldn't be opened.
 */
export type ChatsBlocked = { kind: 'no-project-elsewhere' } | { kind: 'failed'; error: string };
const [blocked, setBlocked] = createSignal<ChatsBlocked | null>(null);

export const currentProject = current;
export const projectList = all;
/** The workspaces, most recently opened first: listed among the projects (3.4). */
export const workspaceList = workspaces;
export const chatsBlocked = blocked;

/**
 * Each of the window's folders' project, by the folder's path, once resolved:
 * a workspace's folders are each a project of their own, whose chats the
 * window lists (docs/design/workspaces-plan.md, 3.8). A folder still being
 * resolved, or whose project couldn't be opened, isn't in it; each new set
 * of folders starts it again (`newFoldersGeneration`).
 */
const [byFolder, setByFolder] = createSignal<ReadonlyMap<string, ProjectInfo>>(new Map());
export const folderProjects = byFolder;

/** `root`'s project is `project` from now on, or none (`null`). */
function setFolderProject(root: string, project: ProjectInfo | null) {
  setByFolder((was) => {
    const next = new Map(was);
    if (project) next.set(root, project);
    else next.delete(root);
    return next;
  });
}

/** The projects and the workspaces, read again: one that can't be read leaves the other listed. */
export async function refreshProjects() {
  const [projects, spaces] = await Promise.allSettled([
    chatApi.listProjects(),
    chatApi.listWorkspaces(),
  ]);
  // A project or workspace that didn't change keeps its object, and its row
  // in the switcher: the list is read again each time the menu opens, and
  // new rows under a click on one (open, Relink…) lost it.
  if (projects.status === 'fulfilled')
    setAll((old) => keepUnchanged(old, projects.value, sameProject));
  else console.error('Listing projects failed:', projects.reason);
  if (spaces.status === 'fulfilled') {
    setWorkspaces((old) => keepUnchanged(old, spaces.value, sameWorkspace));
  } else console.error('Listing workspaces failed:', spaces.reason);
}

/** `next`, each one that's `same` as the one of its ID in `old` being that one. */
function keepUnchanged<T extends { id: string }>(
  old: readonly T[],
  next: T[],
  same: (a: T, b: T) => boolean
): T[] {
  const known = new Map(old.map((item) => [item.id, item]));
  return next.map((item) => {
    const was = known.get(item.id);
    return was && same(was, item) ? was : item;
  });
}

/**
 * The workspace `id` as a target, its folders as the database has them (each
 * with the name its chats write); null when it's gone (D20).
 */
export async function workspaceTarget(id: string): Promise<WindowTarget | null> {
  try {
    const found = (await chatApi.listWorkspaces()).find((workspace) => workspace.id === id);
    return found ? workspaceAsTarget(found) : null;
  } catch (err) {
    console.error('Reading the workspace failed:', err);
    return null;
  }
}

function sameWorkspace(a: WorkspaceSummary, b: WorkspaceSummary) {
  return (
    a.name === b.name &&
    a.chatCount === b.chatCount &&
    a.lastOpenedAt === b.lastOpenedAt &&
    a.folders.length === b.folders.length &&
    a.folders.every(
      (f, i) =>
        f.name === b.folders[i].name &&
        f.path === b.folders[i].path &&
        f.missing === b.folders[i].missing
    )
  );
}

function sameProject(a: ProjectSummary, b: ProjectSummary) {
  return (
    a.name === b.name &&
    a.path === b.path &&
    a.chatCount === b.chatCount &&
    a.lastOpenedAt === b.lastOpenedAt &&
    a.missing === b.missing
  );
}

/**
 * How many times a folder was resolved: an answer that comes for a folder
 * the window resolved again since is the old one, and is dropped. Each
 * folder has its own, so resolving one doesn't drop another's answer
 * (docs/design/workspaces-plan.md, 3.3).
 */
const runs = new Map<string, number>();

/**
 * The window's set of folders, as a generation: each switch makes a new one,
 * so an answer for a folder of an earlier one — a folder the window has left
 * — is dropped too, even though that folder was resolved only once.
 */
let generation = 0;

/**
 * The window shows another set of folders: answers asked before are dropped,
 * and so are the projects the folders it left were.
 */
export function newFoldersGeneration(): number {
  setByFolder(new Map());
  return ++generation;
}

/**
 * The folder just opened (null: none) is which project, asking if unsure.
 * `folders`: the window's folders, so a copy whose original is one of them
 * is only offered its own history (D5). `as`: the generation it's asked in,
 * the window's when it's asked (App.tsx's `resolveFolders` makes one per
 * switch).
 */
export async function resolveProject(
  root: string | null,
  folders: OpenFolder[] = [],
  as = generation
) {
  const run = (runs.get(root ?? '') ?? 0) + 1;
  runs.set(root ?? '', run);
  const current = () => as === generation && run === runs.get(root ?? '');
  if (!root) {
    // The chats without a project, if no other window has them.
    const ours = await claimNoProject();
    if (!current()) return;
    setBlocked(ours ? null : { kind: 'no-project-elsewhere' });
    setCurrent(null);
    void refreshProjects(); // for the recent projects in the empty Files tab
    return;
  }
  setBlocked(null);
  setCurrent(undefined);
  try {
    const opened = await chatApi.openProject(root);
    if (!current()) return;
    setCurrent(opened.project);
    setFolderProject(root, opened.project);
    void refreshProjects();
    const s = opened.suggestion;
    if (s?.kind === 'copy') askAboutCopy(root, s.original, folders);
    else if (s?.kind === 'moved') askAboutMoved(root, opened.project, s.candidates);
  } catch (err) {
    console.error('Opening the project failed:', err);
    // Not the chats without a project instead: another window's, maybe.
    if (current()) {
      setBlocked({ kind: 'failed', error: String(err) });
      setCurrent(null);
      setFolderProject(root, null);
    }
  }
}

const chats = (n: number) => `${n} chat${n === 1 ? '' : 's'}`;

/**
 * The user said which project the folder at `root` is (a copy's own
 * history, a moved project's chats): it's that project from now on, if the
 * window still has the folder open.
 */
function answered(root: string, project: ProjectInfo) {
  if (current()?.path === root) setCurrent(project);
  if (byFolder().has(root)) setFolderProject(root, project);
}

/**
 * The copy's history is its own: a new ID in this folder. Each folder asks
 * its own question, under an id of its own (3.3).
 */
function askAboutCopy(root: string, original: ProjectSummary, folders: OpenFolder[]) {
  const separate = async () => {
    const project = await chatApi.answerCopy(root, original.id, false);
    answered(root, project);
    void refreshProjects();
  };
  // The original is another folder of this window: the two would share their
  // chats, and a workspace holds one project once (D5), so sharing isn't
  // offered.
  const member = folders.some((folder) => folder.project?.id === original.id);
  if (member) {
    notify({
      id: `project-copy:${root}`,
      kind: 'info',
      title: `This folder looks like a copy of ${original.name}`,
      message:
        `${root} looks like a copy of ${original.name}, which is another folder of this ` +
        'window: the two would share their chats. This folder keeps a separate chat history ' +
        'instead.',
      actions: [{ label: 'Separate history', primary: true, run: separate }],
    });
    return;
  }
  notify({
    id: `project-copy:${root}`,
    kind: 'info',
    title: `This folder looks like a copy of ${original.name}`,
    message:
      `${root} looks like a copy of ${original.name}, at ${original.path}. Share its chats ` +
      `(${chats(original.chatCount)}) with this folder, or keep a separate chat history for ` +
      'the copy?',
    actions: [
      { label: 'Separate history', run: separate },
      {
        label: 'Share chats',
        primary: true,
        run: () => void chatApi.answerCopy(root, original.id, true),
      },
    ],
  });
}

/** One candidate at a time: "Not this one" moves on to the next. */
function askAboutMoved(root: string, here: ProjectInfo, candidates: ProjectSummary[]) {
  const [first, ...rest] = candidates;
  if (!first) return;
  notify({
    id: `project-moved:${root}`,
    kind: 'info',
    title: `This looks like ${first.name}`,
    message:
      `A project with the same git history used to be at ${first.path}, which is gone. ` +
      `Does ${root} use its chats (${chats(first.chatCount)})?`,
    actions: [
      {
        label: rest.length ? 'Not this one' : 'No',
        run: async () => {
          await chatApi.declineProjects(root, [first.id]);
          askAboutMoved(root, here, rest);
        },
      },
      {
        label: 'Use its chats',
        primary: true,
        run: async () => {
          const project = await chatApi.answerMoved(root, here.id, first.id);
          answered(root, project);
          void refreshProjects();
        },
      },
    ],
  });
}
