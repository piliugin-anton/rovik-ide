// editorBridge.ts
//
// Messages between the editor and the chat, without prop chains through
// App > ChatPanel > MessageList > ToolCallBlock:
//   chat -> editor: open a file at a line (read_file's "Open in editor");
//   editor -> chat: reference the selected lines ("Add to chat").

import type { FileSuggestion } from '../types';

export interface OpenFileRequest {
  /** As the model gave it: relative to the project root, or absolute. */
  path: string;
  /** 1-based line to put the cursor on and center in the editor. */
  line?: number;
}

const listeners = new Set<(request: OpenFileRequest) => void>();

export function requestOpenFile(request: OpenFileRequest) {
  for (const listener of listeners) listener(request);
}

export function onOpenFileRequest(listener: (request: OpenFileRequest) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
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
