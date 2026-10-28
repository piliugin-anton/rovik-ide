// ProjectList.tsx: projects and workspaces, most recently opened first, for
// the switcher and the empty Files tab (lib/projects.ts). A project whose
// folder is gone shows "folder missing", with Relink (point it at its
// folder) and Delete. A workspace shows its folders and how many of them are
// missing, and opens with the rest (D20). Delete is hidden for every project
// and workspace this window has open: their chats are the window's, and
// deleting them from under it would close what it shows
// (docs/design/workspaces-plan.md, 3.4).
import { For, Match, Show, Switch } from 'solid-js';
import type { ProjectSummary, WorkspaceSummary } from '../lib/chatApi';
import { plural } from '../lib/textUtils';

function ago(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  const d = Math.round(s / 86400);
  return d < 30 ? `${d} day${d === 1 ? '' : 's'} ago` : new Date(ms).toLocaleDateString();
}

/** A row of the list: a project, or a workspace of several folders (D1). */
export type Listed =
  { kind: 'project'; project: ProjectSummary } | { kind: 'workspace'; workspace: WorkspaceSummary };

/** The ID a row's chats are kept under: the project's, or the workspace's. */
export const listedId = (entry: Listed): string =>
  entry.kind === 'project' ? entry.project.id : entry.workspace.id;

const openedAt = (entry: Listed): number =>
  entry.kind === 'project' ? entry.project.lastOpenedAt : entry.workspace.lastOpenedAt;

/**
 * Each summary's row, the same object for as long as the summary is: <For>
 * then keeps its row, as lib/projects.ts keeps an unchanged summary, so a
 * click on a row isn't lost to the list being read again under it.
 */
const rows = new WeakMap<ProjectSummary | WorkspaceSummary, Listed>();

function rowOf<T extends ProjectSummary | WorkspaceSummary>(summary: T, make: (s: T) => Listed) {
  let row = rows.get(summary);
  if (!row) {
    row = make(summary);
    rows.set(summary, row);
  }
  return row;
}

/** Projects and workspaces together, most recently opened first. */
export function listed(
  projects: readonly ProjectSummary[],
  workspaces: readonly WorkspaceSummary[]
): Listed[] {
  const all: Listed[] = [
    ...projects.map((p) => rowOf(p, (project) => ({ kind: 'project', project }))),
    ...workspaces.map((w) => rowOf(w, (workspace) => ({ kind: 'workspace', workspace }))),
  ];
  return all.sort((a, b) => openedAt(b) - openedAt(a));
}

/**
 * Whether `entry` is what the filter's `query` asks for: a project by its
 * name or its folder, a workspace by its name or any of its folders (3.4).
 */
export function listedMatches(entry: Listed, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  const has = (text: string) => text.toLowerCase().includes(q);
  if (entry.kind === 'project') return has(entry.project.name) || has(entry.project.path);
  return has(entry.workspace.name) || entry.workspace.folders.some((f) => has(f.path));
}

/** Whether a row can be opened now: a project whose folder is gone can't, a workspace can (D20). */
export const listedOpenable = (entry: Listed): boolean =>
  entry.kind === 'workspace' || !entry.project.missing;

/**
 * What Delete asks first: a project's chats go, its folder stays; a
 * workspace goes with its own chats, and its folders and their chats stay
 * (3.4, 3.1).
 */
export function deleteQuestion(entry: Listed): {
  title: string;
  message: string;
  confirmLabel: string;
} {
  if (entry.kind === 'project') {
    const p = entry.project;
    return {
      title: `Delete ${p.name}'s chat history?`,
      message: `Its ${p.chatCount} chat${p.chatCount === 1 ? '' : 's'} are deleted for good. The project folder itself isn't touched.`,
      confirmLabel: 'Delete chats',
    };
  }
  const w = entry.workspace;
  const chats =
    w.chatCount === 1 ? 'Its 1 workspace chat is' : `Its ${w.chatCount} workspace chats are`;
  return {
    title: `Delete the workspace ${w.name}?`,
    message: `${chats} deleted for good. Its folders, and their own chats, aren't touched.`,
    confirmLabel: 'Delete workspace',
  };
}

export default function ProjectList(props: {
  entries: Listed[];
  /** The project or workspace shown now. */
  currentId?: string | null;
  /** The projects and the workspace this window has open: no Delete on any of them. */
  openIds?: ReadonlySet<string>;
  onOpen: (entry: Listed) => void;
  onRelink: (p: ProjectSummary) => void;
  onDelete: (entry: Listed) => void;
}) {
  const deletable = (entry: Listed) => !props.openIds?.has(listedId(entry));
  const deleteButton = (entry: Listed, title: string) => (
    <Show when={deletable(entry)}>
      <button
        type="button"
        class="link-button project-row-delete"
        title={title}
        onClick={() => props.onDelete(entry)}
      >
        Delete
      </button>
    </Show>
  );
  return (
    <ul class="project-list" role="listbox" aria-label="Projects">
      <For each={props.entries} fallback={<li class="project-list-empty">No projects yet</li>}>
        {(entry) => (
          <Switch>
            <Match when={entry.kind === 'project' && entry.project}>
              {(p) => (
                <li
                  class="project-row"
                  classList={{ current: p().id === props.currentId, missing: p().missing }}
                  role="option"
                  aria-selected={p().id === props.currentId}
                >
                  <button
                    type="button"
                    class="project-row-main"
                    disabled={p().missing}
                    title={p().missing ? `${p().path} no longer exists` : p().path}
                    onClick={() => props.onOpen(entry)}
                  >
                    <span class="project-row-name">
                      {p().name}
                      <Show when={p().missing}>
                        <span class="project-row-badge">folder missing</span>
                      </Show>
                    </span>
                    <span class="project-row-path">{p().path}</span>
                    <span class="project-row-meta">
                      {p().chatCount} chat{p().chatCount === 1 ? '' : 's'} · {ago(p().lastOpenedAt)}
                    </span>
                  </button>
                  <span class="project-row-actions">
                    <Show when={p().missing}>
                      <button type="button" class="link-button" onClick={() => props.onRelink(p())}>
                        Relink…
                      </button>
                    </Show>
                    {deleteButton(entry, "Delete this project's chat history")}
                  </span>
                </li>
              )}
            </Match>
            <Match when={entry.kind === 'workspace' && entry.workspace}>
              {(w) => {
                const missing = () => w().folders.filter((folder) => folder.missing).length;
                return (
                  <li
                    class="project-row workspace-row"
                    classList={{ current: w().id === props.currentId }}
                    role="option"
                    aria-selected={w().id === props.currentId}
                  >
                    <button
                      type="button"
                      class="project-row-main"
                      title={w()
                        .folders.map((folder) => folder.path)
                        .join('\n')}
                      onClick={() => props.onOpen(entry)}
                    >
                      <span class="project-row-name">
                        {w().name}
                        <Show when={missing() > 0}>
                          <span class="project-row-badge">
                            {plural(missing(), 'folder')} missing
                          </span>
                        </Show>
                      </span>
                      <span class="project-row-path">
                        {w()
                          .folders.map((folder) => folder.name)
                          .join(', ')}
                      </span>
                      <span class="project-row-meta">
                        Workspace · {plural(w().folders.length, 'folder')} · {ago(w().lastOpenedAt)}
                      </span>
                    </button>
                    <span class="project-row-actions">
                      {deleteButton(entry, 'Delete this workspace and its own chats')}
                    </span>
                  </li>
                );
              }}
            </Match>
          </Switch>
        )}
      </For>
    </ul>
  );
}
