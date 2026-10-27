// FolderMenu.tsx
//
// The pick-one menu of a workspace's folders: which folder a new terminal
// starts in (C04). The look and keys of the app's other menus, which the
// panel's "+" menu had before a workspace could have several folders: the
// first item has the focus as it opens, the arrows move it, Enter or Space
// opens the item in focus, and Escape closes the menu, as Tab does. Its
// `children` are a caller's own items, after a separator.
import { For, onMount, Show, type JSX } from 'solid-js';
import { focusMenuItem, onMenuKey } from '../lib/menuKeys';
import type { OpenFolder } from '../lib/openFolders';
import { Icon } from './MenuIcon';

export interface FolderMenuProps {
  ref: (el: HTMLDivElement) => void;
  x: number;
  y: number;
  /** The folders to pick from, in the window's order. */
  folders: readonly OpenFolder[];
  onPick: (folder: OpenFolder) => void;
  /** Escape or Tab: the menu closes, and what opened it has the focus again. */
  onClose: () => void;
  /** More items after the folders, after a separator. */
  children?: JSX.Element;
}

export default function FolderMenu(props: FolderMenuProps) {
  let list: HTMLUListElement | undefined;

  onMount(() => focusMenuItem(list, 0));

  return (
    <div
      ref={props.ref}
      class="context-menu folder-menu"
      style={{ top: `${props.y}px`, left: `${props.x}px` }}
      // A right-click on the menu itself shouldn't open the browser's.
      onContextMenu={(e) => e.preventDefault()}
      onKeyDown={(e) => onMenuKey(e, list, props.onClose)}
    >
      <ul ref={list} class="context-menu-list" role="menu" aria-label="Folder">
        <For each={props.folders}>
          {(folder) => (
            <li
              class="context-menu-item"
              role="menuitem"
              tabIndex={-1}
              title={folder.path}
              onClick={(e) => {
                e.stopPropagation();
                props.onPick(folder);
              }}
            >
              <span class="context-menu-icon" aria-hidden="true">
                <Icon>
                  <path d="M2 4.5a1 1 0 0 1 1-1h3.5l1.2 1.5H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" />
                </Icon>
              </span>
              <span class="context-menu-label">{folder.name}</span>
            </li>
          )}
        </For>
        <Show when={props.children}>
          <li class="context-menu-separator" role="separator" />
          {props.children}
        </Show>
      </ul>
    </div>
  );
}
