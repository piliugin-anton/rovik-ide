// editorBridge.ts
//
// Lets chat components ask the main editor to open a file, without a prop
// chain from App down through ChatPanel > MessageList > ToolCallBlock.
// App registers the one listener.

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
