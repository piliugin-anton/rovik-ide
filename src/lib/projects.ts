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
import { notify } from './notifications';

/** undefined: not resolved yet; null: no folder open. */
const [current, setCurrent] = createSignal<ProjectInfo | null | undefined>(undefined);
const [all, setAll] = createSignal<ProjectSummary[]>([]);

export const currentProject = current;
export const projectList = all;

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

function sameProject(a: ProjectSummary, b: ProjectSummary) {
  return (
    a.name === b.name &&
    a.path === b.path &&
    a.chatCount === b.chatCount &&
    a.lastOpenedAt === b.lastOpenedAt &&
    a.missing === b.missing
  );
}

let resolving = 0;

/** The folder just opened (null: none): which project it is, asking if unsure. */
export async function resolveProject(root: string | null) {
  const run = ++resolving;
  if (!root) {
    setCurrent(null);
    void refreshProjects(); // for the recent projects in the empty Files tab
    return;
  }
  setCurrent(undefined);
  try {
    const opened = await chatApi.openProject(root);
    if (run !== resolving) return;
    setCurrent(opened.project);
    void refreshProjects();
    const s = opened.suggestion;
    if (s?.kind === 'copy') askAboutCopy(root, s.original);
    else if (s?.kind === 'moved') askAboutMoved(root, opened.project, s.candidates);
  } catch (err) {
    console.error('Opening the project failed:', err);
    if (run === resolving) setCurrent(null);
  }
}

const chats = (n: number) => `${n} chat${n === 1 ? '' : 's'}`;

function askAboutCopy(root: string, original: ProjectSummary) {
  notify({
    id: 'project-copy',
    kind: 'info',
    title: `This folder looks like a copy of ${original.name}`,
    message:
      `${original.name} is also at ${original.path}. Share its chats (${chats(original.chatCount)}) ` +
      'with this folder, or keep a separate chat history for the copy?',
    actions: [
      {
        label: 'Separate history',
        run: async () => {
          const project = await chatApi.answerCopy(root, original.id, false);
          if (current()?.path === root) setCurrent(project);
          void refreshProjects();
        },
      },
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
    id: 'project-moved',
    kind: 'info',
    title: `This looks like ${first.name}`,
    message:
      `A project with the same git history used to be at ${first.path}, which is gone. ` +
      `Use its chats (${chats(first.chatCount)}) here?`,
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
