// FileContextMenu.tsx
//
// The file tree's right-click menu. App owns when it shows, where, and what
// each action does; this is the menu itself. Same look as the app's other
// popups (the model picker): menu background, rounded items, quiet icons.
import type { JSX } from 'solid-js';
import ContextMenuItem from './ContextMenuItem';

const isMac = navigator.platform.toUpperCase().includes('MAC');
/** Matches App's handleKeyDown, which takes these with Ctrl (Cmd on macOS). */
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

export interface FileContextMenuProps {
  ref: (el: HTMLDivElement) => void;
  x: number;
  y: number;
  canNewFolder: boolean;
  /** Something is selected: copy, cut, delete. */
  hasSelection: boolean;
  /** Exactly one item: rename. */
  singleSelection: boolean;
  canPaste: boolean;
  onNewFile: () => void;
  onNewFolder: () => void;
  onRename: () => void;
  onCopy: () => void;
  onCut: () => void;
  onPaste: () => void;
  onDelete: () => void;
}

export default function FileContextMenu(props: FileContextMenuProps) {
  return (
    <div
      ref={props.ref}
      class="context-menu file-context-menu"
      style={{ top: `${props.y}px`, left: `${props.x}px` }}
      // A right-click on the menu itself shouldn't open the browser's.
      onContextMenu={(e) => e.preventDefault()}
    >
      <ul class="context-menu-list" role="menu" aria-label="File actions">
        <ContextMenuItem
          label="New file"
          onClick={props.onNewFile}
          icon={
            <Icon>
              <path d="M4 1.5h5.5L12.5 4.5V14.5H4V1.5Z" />
              <path d="M9.5 1.5V4.5H12.5" />
              <path d="M8 7.5V11.5M6 9.5H10" />
            </Icon>
          }
        />
        <ContextMenuItem
          label="New folder"
          onClick={props.onNewFolder}
          disabled={!props.canNewFolder}
          icon={
            <Icon>
              <path d="M2 4.5a1 1 0 0 1 1-1h3.5l1.2 1.5H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" />
              <path d="M8 8v3.5M6.25 9.75h3.5" />
            </Icon>
          }
        />

        <li class="context-menu-separator" role="separator" />

        <ContextMenuItem
          label="Rename"
          onClick={props.onRename}
          disabled={!props.singleSelection}
          icon={
            <Icon>
              <path d="M11 2.5 13.5 5 5.5 13 2.5 13.5 3 10.5 11 2.5Z" />
            </Icon>
          }
        />

        <li class="context-menu-separator" role="separator" />

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
          label="Paste"
          shortcut={keys('V')}
          onClick={props.onPaste}
          disabled={!props.canPaste}
          icon={
            <Icon>
              <path d="M6 2.5h4a.5.5 0 0 1 .5.5v1H5.5V3a.5.5 0 0 1 .5-.5Z" />
              <path d="M4 4.5h8a1 1 0 0 1 1 1V13a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5.5a1 1 0 0 1 1-1Z" />
            </Icon>
          }
        />

        <li class="context-menu-separator" role="separator" />

        <ContextMenuItem
          label="Delete"
          shortcut={isMac ? '⌘⌫' : 'Ctrl+Del'}
          onClick={props.onDelete}
          disabled={!props.hasSelection}
          isDanger
          icon={
            <Icon>
              <path d="M3 4.5h10M6.5 4.5V3a1 1 0 0 1 1-1h1a1 1 0 0 1 1 1v1.5" />
              <path d="M4.5 4.5 5 13.5a1 1 0 0 0 1 1h4a1 1 0 0 0 1-1l.5-9" />
              <path d="M6.5 7v5M9.5 7v5" />
            </Icon>
          }
        />
      </ul>
    </div>
  );
}
