// projects.ts
//
// The open project (which chats are shown) and the list of projects, for
// the switcher above the sidebar tabs (ProjectSwitcher). The project a
// folder is comes from the backend (chat_db/projects.rs): by the ID file in
// it, so a rename or move keeps its chats. When a folder looks like a copy
// of a project, or like a moved project that lost its ID file, the user is
// asked once, in a notification; the answer is remembered there.
import { createSignal } from 'solid-js';
import { chatApi, type ProjectInfo, type ProjectSummary } from './chatApi';
import { claimNoProject } from './instances';
import type { OpenFolder, WindowTarget } from './openFolders';
import { notify } from './notifications';

/** undefined: not resolved yet; null: no folder open. */
const [current, setCurrent] = createSignal<ProjectInfo | null | undefined>(undefined);
const [all, setAll] = createSignal<ProjectSummary[]>([]);
/**
 * Why the chat panel shows no chats here, if it doesn't: the chats without
 * a project are another window's (lib/instances.ts), or the folder's
 * couldn't be opened.
 */
export type ChatsBlocked = { kind: 'no-project-elsewhere' } | { kind: 'failed'; error: string };
const [blocked, setBlocked] = createSignal<ChatsBlocked | null>(null);

export const currentProject = current;
export const projectList = all;
export const chatsBlocked = blocked;

export async function refreshProjects() {
  try {
    const list = await chatApi.listProjects();
    // A project that didn't change keeps its object, and its row in the
    // switcher: the list is read again each time the menu opens, and new
    // rows under a click on one (open, Relink…) lost it.
    setAll((old) => {
      const known = new Map(old.map((p) => [p.id, p]));
      return list.map((p) => {
        const was = known.get(p.id);
        return was && sameProject(was, p) ? was : p;
      });
    });
  } catch (err) {
    console.error('Listing projects failed:', err);
  }
}

/**
 * The workspace `id` as a target, its folders as the database has them (each
 * with the name its chats write); null when it's gone (D20).
 */
export async function workspaceTarget(id: string): Promise<WindowTarget | null> {
  try {
    const found = (await chatApi.listWorkspaces()).find((workspace) => workspace.id === id);
    if (!found) return null;
    return {
      kind: 'workspace',
      id: found.id,
      name: found.name,
      folders: found.folders.map(({ name, path }) => ({ name, path })),
    };
  } catch (err) {
    console.error('Reading the workspace failed:', err);
    return null;
  }
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

/** The window shows another set of folders: answers asked before are dropped. */
export function newFoldersGeneration(): number {
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
    }
  }
}

const chats = (n: number) => `${n} chat${n === 1 ? '' : 's'}`;

/**
 * The copy's history is its own: a new ID in this folder. Each folder asks
 * its own question, under an id of its own (3.3).
 */
function askAboutCopy(root: string, original: ProjectSummary, folders: OpenFolder[]) {
  const separate = async () => {
    const project = await chatApi.answerCopy(root, original.id, false);
    if (current()?.path === root) setCurrent(project);
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
          if (current()?.path === root) setCurrent(project);
          void refreshProjects();
        },
      },
    ],
  });
}
