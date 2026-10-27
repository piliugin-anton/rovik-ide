// NewChatMenu.tsx
//
// New chat's menu in a window with several folders (docs/design/
// workspaces-plan.md, D2): the folders, each with a box, the one the user
// is working in checked as it opens, and Start chat, which names the folder
// checked. A folder chat works in one folder, so checking a folder unchecks
// the one checked before; a workspace chat across several comes with F14.
// A missing folder is listed, and can't be checked, nor can a copy that
// still has its original's chats. Space checks the folder in focus, Enter
// starts the chat, and Escape, Tab or a press elsewhere makes none. Opened
// from a button it floats there; the chat panel with no chat shows it in
// place, to start one (3.8).
import { createEffect, createSignal, onCleanup, onMount } from 'solid-js';
import FolderMenu from './FolderMenu';
import { chatRootOf, type OpenFolder } from '../lib/openFolders';

export interface NewChatMenuProps {
  /** The window's folders, in its order. */
  folders: readonly OpenFolder[];
  /** Checked as it opens: the folder the user is working in. */
  initial: OpenFolder | null;
  /** The button it opened from: it floats there. Absent: shown in place. */
  anchor?: HTMLElement;
  /** Start chat, with the folder checked. */
  onStart: (folder: OpenFolder) => void;
  /** Escape, Tab, a press elsewhere: no chat. */
  onClose?: () => void;
}

/**
 * Why `folder` can't be checked for a chat, or null when it can. `folders`:
 * the window's, in its order.
 */
export function cantChatIn(folder: OpenFolder, folders: readonly OpenFolder[]): string | null {
  if (folder.missing)
    return folders.length > 1
      ? `${folder.name} isn’t there: locate it, or remove it from the workspace`
      : `${folder.name} isn’t there`;
  if (!folder.project) return `${folder.name} is still being opened`;
  // A copy has its original's chats until its history is separated: a chat
  // started in either works in the one listed first, which has them.
  const first = chatRootOf({ projectId: folder.project.id }, folders);
  if (first && first.path !== folder.path)
    return `${folder.name} shares its chats with ${first.name} until the copy’s history is separated`;
  return null;
}

export default function NewChatMenu(props: NewChatMenuProps) {
  const first = () => {
    const initial = props.initial;
    const at = initial && props.folders.find((folder) => folder.path === initial.path);
    return at && !cantChatIn(at, props.folders)
      ? at
      : (props.folders.find((folder) => !cantChatIn(folder, props.folders)) ?? null);
  };
  const [checked, setChecked] = createSignal<OpenFolder | null>(first());
  /** The folder checked, while it's still one a chat can be in. */
  const chosen = () => {
    const at = checked();
    const now = at && props.folders.find((folder) => folder.path === at.path);
    return now && !cantChatIn(now, props.folders) ? now : null;
  };
  const start = () => {
    const folder = chosen();
    if (folder) props.onStart(folder);
  };

  let menu: HTMLDivElement | undefined;
  const [at, setAt] = createSignal<{ x: number; y: number } | null>(null);

  // Under its button, its right edge on the button's, kept in the window.
  onMount(() => {
    const anchor = props.anchor;
    if (!anchor || !menu) return;
    const el = menu;
    const r = anchor.getBoundingClientRect();
    const below = r.bottom + 4;
    const fits = below + el.offsetHeight <= window.innerHeight - 10;
    const y = fits ? below : Math.max(0, r.top - 4 - el.offsetHeight);
    const right = window.innerWidth - el.offsetWidth - 10;
    setAt({ x: Math.max(0, Math.min(r.right - el.offsetWidth, right)), y });
  });

  // Closed by a press elsewhere, or the window losing focus or size, as the
  // app's other menus are.
  createEffect(() => {
    const anchor = props.anchor;
    const close = props.onClose;
    if (!anchor || !close) return;
    const outside = (e: Event) => {
      const target = e.target as Node;
      if (!menu?.contains(target) && !anchor.contains(target)) close();
    };
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('blur', close);
    window.addEventListener('resize', close);
    onCleanup(() => {
      window.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('blur', close);
      window.removeEventListener('resize', close);
    });
  });

  return (
    <FolderMenu
      ref={(el) => (menu = el)}
      // Off screen until it's measured: it's placed by its own size.
      x={props.anchor ? (at()?.x ?? -9999) : undefined}
      y={props.anchor ? (at()?.y ?? -9999) : undefined}
      folders={props.folders}
      check={{
        checked: new Set(chosen() ? [chosen()!.path] : []),
        // One folder at a time: a folder chat works in one.
        onToggle: (folder) => setChecked((was) => (was?.path === folder.path ? null : folder)),
        unavailable: (folder) => cantChatIn(folder, props.folders),
        onSubmit: start,
      }}
      onClose={() => props.onClose?.()}
    >
      <li
        class="context-menu-item new-chat-start"
        classList={{ disabled: !chosen() }}
        role="menuitem"
        aria-disabled={chosen() ? undefined : true}
        tabIndex={-1}
        title={chosen() ? undefined : 'Check a folder for the chat to work in'}
        onClick={(e) => {
          e.stopPropagation();
          start();
        }}
      >
        <span class="context-menu-label">
          {chosen() ? `Start chat in ${chosen()!.name}` : 'Start chat'}
        </span>
      </li>
    </FolderMenu>
  );
}
