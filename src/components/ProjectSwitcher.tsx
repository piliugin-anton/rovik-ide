// ProjectSwitcher.tsx: the open project or workspace, above the sidebar tabs.
// Its menu lists the projects and workspaces (ProjectList), opens another
// folder, makes a new project or a workspace, adds folders to the open
// workspace, and opens a new window (docs/design/workspaces-plan.md, 3.4,
// D3); it replaces the "Open project..." row the Files tab used to have.
import { createMemo, createSignal, onCleanup, Show } from 'solid-js';
import type { ProjectSummary } from '../lib/chatApi';
import type { WindowTarget } from '../lib/openFolders';
import { currentProject, projectList, refreshProjects, workspaceList } from '../lib/projects';
import ProjectList, {
  listed,
  listedId,
  listedMatches,
  listedOpenable,
  type Listed,
} from './ProjectList';

const isMac = navigator.platform.toUpperCase().includes('MAC');

export default function ProjectSwitcher(props: {
  /** What the window has open: in a workspace, the trigger shows its name and lists its folders. */
  target: WindowTarget | null;
  /** The projects and the workspace this window has open: no Delete on any of them. */
  openIds?: ReadonlySet<string>;
  onOpenFolder: () => void;
  onNewProject: () => void;
  /** Create a workspace…: the dialog (NewWorkspaceDialog). */
  onNewWorkspace: () => void;
  /** Add folder to workspace…: offered while a workspace is open. */
  onAddFolder: () => void;
  /** Another Rovik window, for another project. */
  onNewWindow: () => void;
  onOpen: (entry: Listed) => void;
  onRelink: (p: ProjectSummary) => void;
  onDelete: (entry: Listed) => void;
}) {
  const [open, setOpen] = createSignal(false);
  const [filter, setFilter] = createSignal('');
  let root: HTMLDivElement | undefined;
  let filterInput: HTMLInputElement | undefined;

  const workspace = () => (props.target?.kind === 'workspace' ? props.target : null);
  /** The row shown as open: the workspace, else the folder's project. */
  const currentId = () => workspace()?.id ?? currentProject()?.id;
  const shown = createMemo(() =>
    listed(projectList(), workspaceList()).filter((entry) => listedMatches(entry, filter()))
  );

  const toggle = () => {
    const next = !open();
    setOpen(next);
    if (next) {
      setFilter('');
      void refreshProjects();
      queueMicrotask(() => filterInput?.focus());
    }
  };
  const close = () => setOpen(false);
  const onOutside = (e: PointerEvent) => {
    if (open() && root && !root.contains(e.target as Node)) close();
  };
  window.addEventListener('pointerdown', onOutside, true);
  onCleanup(() => window.removeEventListener('pointerdown', onOutside, true));

  /** A menu item: the menu closes, then it acts. */
  const item = (run: () => void) => () => {
    close();
    run();
  };

  return (
    <div class="project-switcher" ref={root}>
      <button
        type="button"
        class="project-switcher-trigger"
        classList={{ workspace: !!workspace() }}
        aria-haspopup="listbox"
        aria-expanded={open()}
        title={
          workspace()
            ?.folders.map((folder) => folder.path)
            .join('\n') ??
          currentProject()?.path ??
          'No project open'
        }
        onClick={toggle}
      >
        <Show
          when={workspace()}
          fallback={
            <svg
              width="14"
              height="14"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M1.75 4.25a1 1 0 0 1 1-1h3.5l1.5 1.5h5.5a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1H2.75a1 1 0 0 1-1-1v-8Z" />
            </svg>
          }
        >
          {/* Stacked folders: a workspace's several. */}
          <svg
            class="project-switcher-stack"
            width="14"
            height="14"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M3.75 3.25V2.5a.75.75 0 0 1 .75-.75h2.75l1.25 1.25h4.5a.75.75 0 0 1 .75.75v5.5" />
            <path d="M1.75 6a1 1 0 0 1 1-1h3.25l1.5 1.5h4.75a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H2.75a1 1 0 0 1-1-1V6Z" />
          </svg>
        </Show>
        <span class="project-switcher-name" classList={{ none: !workspace() && !currentProject() }}>
          {workspace()?.name ?? currentProject()?.name ?? 'No project open'}
        </span>
        <svg
          class="project-switcher-caret"
          width="10"
          height="10"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M4 6l4 4 4-4" />
        </svg>
      </button>
      <Show when={open()}>
        <div class="project-switcher-menu">
          <input
            ref={filterInput}
            class="project-switcher-filter"
            placeholder="Filter projects"
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') close();
              if (e.key === 'Enter') {
                const first = shown().find(
                  (entry) => listedOpenable(entry) && listedId(entry) !== currentId()
                );
                if (first) item(() => props.onOpen(first))();
              }
            }}
          />
          <div class="project-switcher-list">
            <ProjectList
              entries={shown()}
              currentId={currentId()}
              openIds={props.openIds}
              onOpen={(entry) => item(() => props.onOpen(entry))()}
              onRelink={(p) => item(() => props.onRelink(p))()}
              onDelete={props.onDelete}
            />
          </div>
          <button type="button" class="project-switcher-open" onClick={item(props.onOpenFolder)}>
            Open project…
            <span class="project-switcher-shortcut">{isMac ? '⌘O' : 'Ctrl+O'}</span>
          </button>
          <button
            type="button"
            class="project-switcher-open project-switcher-new"
            onClick={item(props.onNewProject)}
          >
            New project…
          </button>
          <button
            type="button"
            class="project-switcher-open project-switcher-new project-switcher-new-workspace"
            title="Several folders in this window, each its own project"
            onClick={item(props.onNewWorkspace)}
          >
            Create a workspace…
          </button>
          <Show when={workspace()}>
            <button
              type="button"
              class="project-switcher-open project-switcher-new project-switcher-add-folder"
              onClick={item(props.onAddFolder)}
            >
              Add folder to workspace…
            </button>
          </Show>
          <button
            type="button"
            class="project-switcher-open project-switcher-new-window"
            title="Another Rovik window, for another project"
            onClick={item(props.onNewWindow)}
          >
            New window
          </button>
        </div>
      </Show>
    </div>
  );
}
