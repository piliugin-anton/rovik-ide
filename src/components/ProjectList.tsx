// ProjectList.tsx: projects, most recently opened first, for the switcher
// and the empty Files tab (lib/projects.ts). A project whose folder is gone
// shows "folder missing", with Relink (point it at its folder) and Delete.
// Delete is hidden for every project this window has open: its chats are
// the window's, and deleting them from under it would close what it shows
// (docs/design/workspaces-plan.md, 3.4).
import { For, Show } from 'solid-js';
import type { ProjectSummary } from '../lib/chatApi';

function ago(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  const d = Math.round(s / 86400);
  return d < 30 ? `${d} day${d === 1 ? '' : 's'} ago` : new Date(ms).toLocaleDateString();
}

export default function ProjectList(props: {
  projects: ProjectSummary[];
  /** The project shown now. */
  currentId?: string | null;
  /** The projects this window has open: no Delete on any of them. */
  openIds?: ReadonlySet<string>;
  onOpen: (p: ProjectSummary) => void;
  onRelink: (p: ProjectSummary) => void;
  onDelete: (p: ProjectSummary) => void;
}) {
  return (
    <ul class="project-list" role="listbox" aria-label="Projects">
      <For each={props.projects} fallback={<li class="project-list-empty">No projects yet</li>}>
        {(p) => (
          <li
            class="project-row"
            classList={{ current: p.id === props.currentId, missing: p.missing }}
            role="option"
            aria-selected={p.id === props.currentId}
          >
            <button
              type="button"
              class="project-row-main"
              disabled={p.missing}
              title={p.missing ? `${p.path} no longer exists` : p.path}
              onClick={() => props.onOpen(p)}
            >
              <span class="project-row-name">
                {p.name}
                <Show when={p.missing}>
                  <span class="project-row-badge">folder missing</span>
                </Show>
              </span>
              <span class="project-row-path">{p.path}</span>
              <span class="project-row-meta">
                {p.chatCount} chat{p.chatCount === 1 ? '' : 's'} · {ago(p.lastOpenedAt)}
              </span>
            </button>
            <span class="project-row-actions">
              <Show when={p.missing}>
                <button type="button" class="link-button" onClick={() => props.onRelink(p)}>
                  Relink…
                </button>
              </Show>
              <Show when={!props.openIds?.has(p.id)}>
                <button
                  type="button"
                  class="link-button project-row-delete"
                  title="Delete this project's chat history"
                  onClick={() => props.onDelete(p)}
                >
                  Delete
                </button>
              </Show>
            </span>
          </li>
        )}
      </For>
    </ul>
  );
}
