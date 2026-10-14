// ChatInputContextMenu.tsx
//
// The chat input's right-click menu (Cut / Copy / Paste / Select all).
// Owned by ChatPanel, which holds the input; this is the menu itself. Same
// look as the app's other popups (TerminalContextMenu, FileContextMenu).
import type { JSX } from 'solid-js';
import ContextMenuItem from './ContextMenuItem';

const isMac = navigator.platform.toUpperCase().includes('MAC');
/** The input's own keys for the same actions: Ctrl (Cmd on macOS). */
const keys = (key: string) => (isMac ? `⌘${key}` : `Ctrl+${key}`);

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

export interface ChatInputContextMenuProps {
  ref: (el: HTMLDivElement) => void;
  x: number;
  y: number;
  /** Text or chips are selected in the input: Cut, Copy. */
  hasSelection: boolean;
  /** The input isn't empty: Select all. */
  hasContent: boolean;
  onCut: () => void;
  onCopy: () => void;
  onPaste: () => void;
  onSelectAll: () => void;
}

export default function ChatInputContextMenu(props: ChatInputContextMenuProps) {
  return (
    <div
      ref={props.ref}
      class="context-menu chat-input-context-menu"
      style={{ top: `${props.y}px`, left: `${props.x}px` }}
      // A right-click on the menu itself shouldn't open the browser's.
      onContextMenu={(e) => e.preventDefault()}
      // Pressing an item leaves focus and the selection in the input: the
      // items act on them.
      onMouseDown={(e) => e.preventDefault()}
    >
      <ul class="context-menu-list" role="menu" aria-label="Edit">
        <ContextMenuItem
          label="Cut"
          shortcut={keys('X')}
          onClick={props.onCut}
          disabled={!props.hasSelection}
          icon={
            <Icon>
              <circle cx="4.5" cy="4.5" r="1.5" />
              <circle cx="4.5" cy="11.5" r="1.5" />
              <path d="M13 3 5.7 9M13 13 5.7 7" />
            </Icon>
          }
        />
        <ContextMenuItem
          label="Copy"
          shortcut={keys('C')}
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
          shortcut={keys('V')}
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
          shortcut={keys('A')}
          onClick={props.onSelectAll}
          disabled={!props.hasContent}
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
