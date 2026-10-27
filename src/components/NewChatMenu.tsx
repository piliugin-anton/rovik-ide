// NewChatMenu.tsx
//
// New chat's menu in a window with several folders (docs/design/
// workspaces-plan.md, D2, 4.12): All folders, then the folders, each with a
// box, the one the user is working in checked as it opens, and Start chat,
// which says where. One folder checked makes a folder chat; several, or All
// folders, a workspace chat across them, which follows the workspace for
// All folders. Checking All folders checks every box, and unchecking a box
// unchecks it; checking every box isn't the same, as folders added later
// join only a chat set to All folders. A chat across folders needs the
// virtual-files driver here (WSL's Rovik on Windows): without it a folder
// checked unchecks the one checked before, as a folder chat works in one,
// and All folders says why on its row, which opens the page that says how.
// A missing folder is listed, and can't be checked, nor can a copy that
// still has its original's chats. Space checks the box in focus, Enter
// starts the chat, and Escape, Tab or a press elsewhere makes none. Opened
// from a button it floats there; the chat panel with no chat shows it in
// place, to start one (3.8).
import { createEffect, createResource, createSignal, onCleanup, onMount } from 'solid-js';
import FolderMenu from './FolderMenu';
import { openAppSettings } from '../lib/appSettings';
import { cloud } from '../lib/cloud';
import { chatRootOf, type OpenFolder } from '../lib/openFolders';
import { acrossFoldersBlocked, vfsStatus } from '../lib/virtualFiles';
import { onWindows, WSL_HOST } from '../lib/wsl';

export interface NewChatMenuProps {
  /** The window's folders, in its order. */
  folders: readonly OpenFolder[];
  /** Checked as it opens: the folder the user is working in. */
  initial: OpenFolder | null;
  /** The button it opened from: it floats there. Absent: shown in place. */
  anchor?: HTMLElement;
  /** Start chat: the folders checked, in the window's order, or `'all'` for All folders. */
  onStart: (folders: OpenFolder[] | 'all') => void;
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
  const first = chatRootOf({ projectId: folder.project.id }, folders, null)?.folders[0];
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
  /** The boxes checked, by path. */
  const [checked, setChecked] = createSignal<ReadonlySet<string>>(
    new Set(first() ? [first()!.path] : [])
  );
  /** All folders is checked: the chat follows the workspace. */
  const [all, setAll] = createSignal(false);
  /** The driver here, read as the menu opens: null until it's known, or when it can't be. */
  const [driver] = createResource(() =>
    vfsStatus()
      .then((status) => status.driver)
      .catch(() => null)
  );
  /** Why there's no chat across folders here (null: there can be one). */
  const blocked = () =>
    acrossFoldersBlocked(
      onWindows(),
      driver() ?? null,
      cloud.hosts.find((host) => host.id === WSL_HOST)
    );
  /** The folders checked, in the window's order, while they're still ones a chat can be in. */
  const chosen = () =>
    props.folders.filter(
      (folder) => checked().has(folder.path) && !cantChatIn(folder, props.folders)
    );
  const start = () => {
    if (all()) props.onStart('all');
    else if (chosen().length > 0) props.onStart(chosen());
  };
  /** A box checked or unchecked: one at a time without a chat across folders. */
  const toggle = (folder: OpenFolder) => {
    const was = checked().has(folder.path);
    if (blocked()) setChecked(new Set(was ? [] : [folder.path]));
    else {
      const next = new Set(checked());
      if (was) next.delete(folder.path);
      else next.add(folder.path);
      setChecked(next);
    }
    setAll(false);
  };
  /** All folders checked checks every box; unchecked, the boxes stay as they are. */
  const toggleAll = () => {
    if (!all()) setChecked(new Set(props.folders.map((folder) => folder.path)));
    setAll(!all());
  };
  /** Why All folders can't be checked: no chat across folders here, or a folder that can't be. */
  const allUnavailable = () =>
    blocked() ??
    props.folders.map((folder) => cantChatIn(folder, props.folders)).find((why) => why) ??
    null;
  /** What Start chat says: where the chat will work. */
  const startLabel = () => {
    if (all()) return 'Start chat in all folders';
    const folders = chosen();
    if (folders.length === 1) return `Start chat in ${folders[0].name}`;
    return folders.length > 1 ? `Start chat in ${folders.length} folders` : 'Start chat';
  };
  const canStart = () => all() || chosen().length > 0;

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
        checked: new Set(chosen().map((folder) => folder.path)),
        onToggle: toggle,
        unavailable: (folder) => cantChatIn(folder, props.folders),
        onSubmit: start,
        all: {
          checked: all(),
          onToggle: toggleAll,
          unavailable: allUnavailable(),
          // The page that says how to get a chat across folders.
          onUnavailable: blocked()
            ? () => {
                props.onClose?.();
                openAppSettings(onWindows() ? 'wsl' : 'vfs');
              }
            : undefined,
        },
      }}
      onClose={() => props.onClose?.()}
    >
      <li
        class="context-menu-item new-chat-start"
        classList={{ disabled: !canStart() }}
        role="menuitem"
        aria-disabled={canStart() ? undefined : true}
        tabIndex={-1}
        title={canStart() ? undefined : 'Check a folder for the chat to work in'}
        onClick={(e) => {
          e.stopPropagation();
          start();
        }}
      >
        <span class="context-menu-label">{startLabel()}</span>
      </li>
    </FolderMenu>
  );
}
