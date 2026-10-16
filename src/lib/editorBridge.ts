// editorBridge.ts
//
// Messages between the editor and the chat, without prop chains through
// App > ChatPanel > MessageList > ToolCallBlock:
//   chat -> editor: open a file at a line (read's "Open in editor");
//   editor -> chat: reference the selected lines ("Add to chat").

import type { FileSuggestion } from '../types';

/**
 * "Open in editor" anywhere in the chat: opens the file in a tab (switching
 * to it if it's already open) and centers the start position, if there is
 * one, in the editor.
 */
export interface OpenFileRequest {
  /** As the model gave it: relative to the project root, or absolute. */
  path: string;
  /** 1-based line to put the cursor on and center. */
  line?: number;
  /**
   * Code from the file, when the line isn't known (a code block in a
   * reply, an edit not applied yet): the line is where it's found.
   */
  snippet?: string;
  /**
   * The agent wrote this file: open it with its changes marked against
   * git, as the Git tab does, and centered on the first one when no line
   * or snippet says where to go.
   */
  changes?: boolean;
  /**
   * With `changes`, the chat that wrote it. An isolated chat's write isn't
   * in the project's file yet (a remote chat's is on its host): the file
   * then opens as the chat has it, compared with the project's version,
   * instead of the project's file.
   */
  dialogId?: string | null;
}

const listeners = new Set<(request: OpenFileRequest) => void>();

export function requestOpenFile(request: OpenFileRequest) {
  for (const listener of listeners) listener(request);
}

export function onOpenFileRequest(listener: (request: OpenFileRequest) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/**
 * A read-only tab comparing two versions of a file: `after` shown, with the
 * lines that differ from `before` marked (an isolated chat's change against
 * the project).
 */
export interface CompareRequest {
  /** Identifies the tab, so asking again switches to it. */
  id: string;
  title: string;
  /** For the language. */
  fileName: string;
  before: string;
  after: string;
}

const compareListeners = new Set<(request: CompareRequest) => void>();

export function requestCompare(request: CompareRequest) {
  for (const listener of compareListeners) listener(request);
}

export function onCompareRequest(listener: (request: CompareRequest) => void): () => void {
  compareListeners.add(listener);
  return () => compareListeners.delete(listener);
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
