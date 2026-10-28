// TerminalNewMenu.tsx
//
// The terminal panel's "+" menu, once a cloud host is connected: a new
// terminal on this computer ("Local"), or on one of the connected hosts
// ("Build (remote)"). Owned by TerminalPanel, which places and closes it;
// this is the menu itself, in the look of the app's other menus. It takes
// the keyboard too: the first item has the focus as it opens, the arrows
// move it, Enter or Space opens the item in focus, and Escape closes the
// menu, as Tab does on its way to what comes after "+".
import { For, onMount, Show, type JSX } from 'solid-js';
import { focusMenuItem, onMenuKey } from '../lib/menuKeys';
import { remoteLabel, type TerminalHost } from '../lib/terminal';
import { Icon } from './MenuIcon';

/**
 * The connected hosts a terminal can open on, as menu items: this menu's,
 * and the folder menu's slot when a workspace has several folders (C04).
 */
export function TerminalHostItems(props: {
  hosts: TerminalHost[];
  onPick: (host: TerminalHost | null) => void;
}) {
  return (
    <For each={props.hosts}>
      {(host) => (
        <li
          class="context-menu-item"
          role="menuitem"
          tabIndex={-1}
          onClick={(e) => {
            e.stopPropagation();
            props.onPick({ id: host.id, name: host.name });
          }}
        >
          <span class="context-menu-icon" aria-hidden="true">
            <Icon>
              <path d="M4.5 12.5h7a3 3 0 0 0 .4-5.97 4 4 0 0 0-7.7 1.07 2.5 2.5 0 0 0 .3 4.9Z" />
            </Icon>
          </span>
          <span class="context-menu-label">{remoteLabel(host.name)}</span>
        </li>
      )}
    </For>
  );
}

export interface TerminalNewMenuProps {
  ref: (el: HTMLDivElement) => void;
  x: number;
  y: number;
  /** The hosts a terminal can open on now. */
  hosts: TerminalHost[];
  /** A new terminal: on this computer (null), or on the host. */
  onPick: (host: TerminalHost | null) => void;
  /** Escape or Tab: the menu closes, and "+" has the focus again. */
  onClose: () => void;
}

export default function TerminalNewMenu(props: TerminalNewMenuProps) {
  let list: HTMLUListElement | undefined;

  onMount(() => focusMenuItem(list, 0));

  const item = (label: string, host: TerminalHost | null, icon: JSX.Element, shortcut?: string) => (
    <li
      class="context-menu-item"
      role="menuitem"
      tabIndex={-1}
      onClick={(e) => {
        e.stopPropagation();
        props.onPick(host);
      }}
    >
      <span class="context-menu-icon" aria-hidden="true">
        {icon}
      </span>
      <span class="context-menu-label">{label}</span>
      <Show when={shortcut}>
        <span class="context-menu-shortcut">{shortcut}</span>
      </Show>
    </li>
  );

  return (
    <div
      ref={props.ref}
      class="context-menu terminal-new-menu"
      style={{ top: `${props.y}px`, left: `${props.x}px` }}
      // A right-click on the menu itself shouldn't open the browser's.
      onContextMenu={(e) => e.preventDefault()}
      onKeyDown={(e) => onMenuKey(e, list, props.onClose)}
    >
      <ul ref={list} class="context-menu-list" role="menu" aria-label="New terminal">
        {item(
          'Local',
          null,
          <Icon>
            <rect x="2" y="2.5" width="12" height="8.5" rx="1" />
            <path d="M5.5 13.5h5M8 11v2.5" />
          </Icon>,
          'Ctrl+Shift+`'
        )}
        <TerminalHostItems hosts={props.hosts} onPick={props.onPick} />
      </ul>
    </div>
  );
}
