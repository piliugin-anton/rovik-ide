// editorBridge.ts
//
// Messages between the editor and the chat, without prop chains through
// App > ChatPanel > MessageList > ToolCallBlock:
//   editor -> chat: reference the selected lines ("Add to chat");
//   tab -> editor: a clicked tab's editor takes the keyboard.
// Opening a file from the chat is lib/openInEditor.ts.

import { createSignal } from 'solid-js';
import type { FileSuggestion } from '../types';

/*
 * A click on a file's tab gives its editor the keyboard, at the place it
 * was left (lib/viewStates.ts), as editors do. Only a click: a tab the
 * agent or a link switches to mustn't take the keyboard from the chat. The
 * request waits for the editor, which mounts once its tab is active.
 */
const [focusRequest, setFocusRequest] = createSignal<string | null>(null);

/** The tab of `path` was clicked: its editor should take the keyboard. */
export function requestEditorFocus(path: string) {
  setFocusRequest(path);
}

/** The pending request, for the editor of that path to take. */
export const editorFocusRequest = focusRequest;

/** True when the editor of `path` was asked to take the keyboard (once). */
export function takeEditorFocus(path: string): boolean {
  if (focusRequest() !== path) return false;
  setFocusRequest(null);
  return true;
}

/*
 * Whether a path the chat mentions is a file in the project, so a code
 * block offers "Open in editor" only when there's a file to open. App
 * answers (it knows the project root).
 */

let fileResolver: ((path: string) => Promise<string | null>) | null = null;

export function setProjectFileResolver(
  resolver: ((path: string) => Promise<string | null>) | null
) {
  fileResolver = resolver;
}

/** The file's absolute path if it exists in the project, else null. */
export function resolveProjectFile(path: string): Promise<string | null> {
  return fileResolver ? fileResolver(path).catch(() => null) : Promise.resolve(null);
}

/*
 * Editor -> chat: "Add to chat" on a selection. The chat panel isn't mounted
 * while another sidebar tab is shown, so references wait here until it is
 * ready to take them (takeChatReferences).
 */

const pendingReferences: FileSuggestion[] = [];
const chatListeners = new Set<() => void>();

export function addToChat(reference: FileSuggestion) {
  pendingReferences.push(reference);
  // A copy: App's listener mounts the chat panel, which subscribes mid-loop.
  for (const listener of [...chatListeners]) listener();
}

/** Called when a reference is added; App shows the chat, the chat takes it. */
export function onAddToChat(listener: () => void): () => void {
  chatListeners.add(listener);
  return () => chatListeners.delete(listener);
}

export function takeChatReferences(): FileSuggestion[] {
  return pendingReferences.splice(0);
}
