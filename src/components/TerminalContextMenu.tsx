// TerminalContextMenu.tsx
//
// The terminal's right-click menu (Copy / Paste / Select all). Owned by
// Terminal.tsx, which holds the xterm instance; this is the menu itself.
// Same look as the app's other popups (FileContextMenu): menu background,
// rounded items, quiet icons.
import type { JSX } from 'solid-js';
import ContextMenuItem from './ContextMenuItem';

function Icon(props: { children: JSX.Element }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      {props.children}
    </svg>
  );
}

export interface TerminalContextMenuProps {
  ref: (el: HTMLDivElement) => void;
  x: number;
  y: number;
  /** Terminal has selected text: Copy. */
  hasSelection: boolean;
  onCopy: () => void;
  onPaste: () => void;
  onSelectAll: () => void;
}

export default function TerminalContextMenu(props: TerminalContextMenuProps) {
  return (
    <div
      ref={props.ref}
      class="context-menu terminal-context-menu"
      style={{ top: `${props.y}px`, left: `${props.x}px` }}
      // A right-click on the menu itself shouldn't open the browser's.
      onContextMenu={(e) => e.preventDefault()}
    >
      <ul class="context-menu-list" role="menu" aria-label="Terminal actions">
        <ContextMenuItem
          label="Copy"
          shortcut="Ctrl+Shift+C"
          onClick={props.onCopy}
          disabled={!props.hasSelection}
          icon={
            <Icon>
              <rect x="5.5" y="5.5" width="8" height="8" rx="1" />
              <path d="M2.5 10.5V3a1 1 0 0 1 1-1H10" />
            </Icon>
          }
        />
        <ContextMenuItem
          label="Paste"
          shortcut="Ctrl+Shift+V"
          onClick={props.onPaste}
          icon={
            <Icon>
              <path d="M6 2.5h4a.5.5 0 0 1 .5.5v1H5.5V3a.5.5 0 0 1 .5-.5Z" />
              <path d="M4 4.5h8a1 1 0 0 1 1 1V13a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5.5a1 1 0 0 1 1-1Z" />
            </Icon>
          }
        />

        <li class="context-menu-separator" role="separator" />

        <ContextMenuItem
          label="Select all"
          onClick={props.onSelectAll}
          icon={
            <Icon>
              <path d="M2.5 2.5h11v11h-11Z" stroke-dasharray="2 1.5" />
              <path d="M5.5 8.5 7.5 10.5 10.5 6" />
            </Icon>
          }
        />
      </ul>
    </div>
  );
}
