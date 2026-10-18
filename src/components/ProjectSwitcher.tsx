// ProjectSwitcher.tsx: the open project, above the sidebar tabs. Its menu
// lists the projects (ProjectList) and opens another folder; it replaces
// the "Open project..." row the Files tab used to have.
import { createMemo, createSignal, onCleanup, Show } from 'solid-js';
import type { ProjectSummary } from '../lib/chatApi';
import { currentProject, projectList, refreshProjects } from '../lib/projects';
import ProjectList from './ProjectList';

const isMac = navigator.platform.toUpperCase().includes('MAC');

export default function ProjectSwitcher(props: {
  onOpenFolder: () => void;
  onNewProject: () => void;
  /** Another Rovik window, for another project. */
  onNewWindow: () => void;
  onOpen: (p: ProjectSummary) => void;
  onRelink: (p: ProjectSummary) => void;
  onDelete: (p: ProjectSummary) => void;
}) {
  const [open, setOpen] = createSignal(false);
  const [filter, setFilter] = createSignal('');
  let root: HTMLDivElement | undefined;
  let filterInput: HTMLInputElement | undefined;

  const shown = createMemo(() => {
    const q = filter().trim().toLowerCase();
    const list = projectList();
    return q
      ? list.filter((p) => p.name.toLowerCase().includes(q) || p.path.toLowerCase().includes(q))
      : list;
  });

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

  return (
    <div class="project-switcher" ref={root}>
      <button
        type="button"
        class="project-switcher-trigger"
        aria-haspopup="listbox"
        aria-expanded={open()}
        title={currentProject()?.path ?? 'No project open'}
        onClick={toggle}
      >
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
        <span class="project-switcher-name" classList={{ none: !currentProject() }}>
          {currentProject()?.name ?? 'No project open'}
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
                const first = shown().find((p) => !p.missing && p.id !== currentProject()?.id);
                if (first) {
                  close();
                  props.onOpen(first);
                }
              }
            }}
          />
          <div class="project-switcher-list">
            <ProjectList
              projects={shown()}
              currentId={currentProject()?.id}
              onOpen={(p) => {
                close();
                props.onOpen(p);
              }}
              onRelink={(p) => {
                close();
                props.onRelink(p);
              }}
              onDelete={props.onDelete}
            />
          </div>
          <button
            type="button"
            class="project-switcher-open"
            onClick={() => {
              close();
              props.onOpenFolder();
            }}
          >
            Open project…
            <span class="project-switcher-shortcut">{isMac ? '⌘O' : 'Ctrl+O'}</span>
          </button>
          <button
            type="button"
            class="project-switcher-open project-switcher-new"
            onClick={() => {
              close();
              props.onNewProject();
            }}
          >
            New project…
          </button>
          <button
            type="button"
            class="project-switcher-open project-switcher-new-window"
            title="Another Rovik window, for another project"
            onClick={() => {
              close();
              props.onNewWindow();
            }}
          >
            New window
          </button>
        </div>
      </Show>
    </div>
  );
}
