// FolderMenu.tsx
//
// The menu of a workspace's folders: pick one (which folder a new terminal
// starts in, C04), or check their boxes (which folders a new chat works in,
// D2). The look and keys of the app's other menus, which the panel's "+"
// menu had before a workspace could have several folders: the first item
// has the focus as it opens, the arrows move it, Enter or Space opens the
// item in focus, and Escape closes the menu, as Tab does. With boxes, Space
// checks the folder in focus and Enter does what the menu is for (Start
// chat), wherever the focus is. Its `children` are a caller's own items,
// after a separator. A menu with no place to float at is shown where it's
// put, and leaves the focus where it is (the chat panel with no chat): Tab
// reaches its first folder.
import { For, onMount, Show, type JSX } from 'solid-js';
import { focusMenuItem, onMenuKey } from '../lib/menuKeys';
import type { OpenFolder } from '../lib/openFolders';
import { Icon } from './MenuIcon';

/** A menu of boxes: which folders are checked, and what checking does. */
export interface FolderChecks {
  /** The folders checked, by path. */
  checked: ReadonlySet<string>;
  onToggle: (folder: OpenFolder) => void;
  /** Why a folder's box can't be checked (a missing folder), or null when it can. */
  unavailable: (folder: OpenFolder) => string | null;
  /** Enter: what the menu is for. */
  onSubmit: () => void;
}

export interface FolderMenuProps {
  ref?: (el: HTMLDivElement) => void;
  /** Where it floats; absent, it's shown where it's put. */
  x?: number;
  y?: number;
  /** The folders, in the window's order. */
  folders: readonly OpenFolder[];
  /** Picking one folder (a new terminal's). */
  onPick?: (folder: OpenFolder) => void;
  /** Boxes to check instead of a folder to pick (a new chat's). */
  check?: FolderChecks;
  /** Escape or Tab: the menu closes, and what opened it has the focus again. */
  onClose: () => void;
  /** More items after the folders, after a separator. */
  children?: JSX.Element;
}

export default function FolderMenu(props: FolderMenuProps) {
  let list: HTMLUListElement | undefined;
  const floats = () => props.x !== undefined && props.y !== undefined;

  onMount(() => {
    if (floats()) focusMenuItem(list, 0);
  });

  const onKey = (e: KeyboardEvent) => {
    const check = props.check;
    if (check && e.key === 'Enter') {
      e.preventDefault();
      e.stopPropagation();
      check.onSubmit();
      return;
    }
    onMenuKey(e, list, props.onClose);
  };

  return (
    <div
      ref={props.ref}
      class="context-menu folder-menu"
      classList={{ 'folder-menu-inline': !floats() }}
      style={floats() ? { top: `${props.y}px`, left: `${props.x}px` } : undefined}
      // A right-click on the menu itself shouldn't open the browser's.
      onContextMenu={(e) => e.preventDefault()}
      onKeyDown={onKey}
    >
      <ul ref={list} class="context-menu-list" role="menu" aria-label="Folder">
        <For each={props.folders}>
          {(folder, at) => {
            const why = () => props.check?.unavailable(folder) ?? null;
            const checked = () => !!props.check?.checked.has(folder.path);
            return (
              <li
                class="context-menu-item"
                classList={{ disabled: !!why() }}
                role={props.check ? 'menuitemcheckbox' : 'menuitem'}
                aria-checked={props.check ? checked() : undefined}
                aria-disabled={why() ? true : undefined}
                tabIndex={!floats() && at() === 0 ? 0 : -1}
                title={why() ?? folder.path}
                onClick={(e) => {
                  e.stopPropagation();
                  if (!props.check) {
                    props.onPick?.(folder);
                    return;
                  }
                  if (!why()) props.check.onToggle(folder);
                }}
              >
                <Show
                  when={props.check}
                  fallback={
                    <span class="context-menu-icon" aria-hidden="true">
                      <Icon>
                        <path d="M2 4.5a1 1 0 0 1 1-1h3.5l1.2 1.5H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" />
                      </Icon>
                    </span>
                  }
                >
                  <span
                    class="context-menu-icon folder-menu-box"
                    classList={{ checked: checked() }}
                    aria-hidden="true"
                  >
                    <Show when={checked()}>
                      <Icon>
                        <path d="m4 8.5 2.5 2.5L12 5.5" />
                      </Icon>
                    </Show>
                  </span>
                </Show>
                <span class="context-menu-label">{folder.name}</span>
              </li>
            );
          }}
        </For>
        <Show when={props.children}>
          <li class="context-menu-separator" role="separator" />
          {props.children}
        </Show>
      </ul>
    </div>
  );
}
