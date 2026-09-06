export interface DirEntryInfo {
  id: string;
  name: string;
  path: string;
  is_dir: boolean;
  ignored: boolean;
}

/** A flattened, visible row in the file tree — what actually gets rendered/virtualized. */
export interface TreeRow {
  id: string;
  path: string;
  name: string;
  isDir: boolean;
  depth: number;
  loading?: boolean;
  ignored?: boolean;
}

export interface OpenFile {
  id: string;
  path: string;
  name: string;
  content: string;
  originalContent: string;
  dirty: boolean;
  language: string;
}

export interface ModelProfile {
  id: string;
  label: string; // user-chosen display name
  model: string; // e.g. "gpt-4o-mini"
  baseUrl: string; // e.g. "https://api.openai.com/v1"
  apiKey: string;
  stream: boolean;
}

export interface ChatToolCall {
  id: string;
  name: string;
  arguments: string; // raw JSON string, as returned by the API
}

export interface ToolDirEntry {
  path: string;
  name: string;
  is_dir: boolean;
  depth: number;
}

export interface ToolSearchMatch {
  path: string;
  line: number;
  text: string;
}

export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant' | 'system' | 'tool';
  content: string;
  toolCalls?: ChatToolCall[];
  tool_call_id?: string; // if you later send tool results back
  pending?: boolean;
  error?: string;
}

export interface OpenAIChatOptions {
  max_tokens?: number | null;
  temperature?: number | null;
  top_p?: number | null;
  stop?: string[] | null;
  reasoning_effort?: 'none' | 'low' | 'medium' | 'high' | null;
}

export type ChatStreamEvent =
  | { type: 'Delta'; content: string }
  | { type: 'Done' }
  | { type: 'Error'; message: string }
  | { type: 'ToolCallStart'; id: string; name: string }
  | { type: 'ToolCallArgumentsDelta'; id: string; arguments_delta: string }
  | { type: 'ToolCallComplete'; id: string; name: string; arguments: string };

export interface ChatDialog {
  id: string;
  title: string;
  messages: ChatMessage[];
  createdAt: number;
  updatedAt: number;
}

export type WatchOperation = 'create' | 'modify' | 'remove' | 'unknown';

export interface FileWatchPayload {
  path: string;
  operation: WatchOperation;
  is_dir: boolean;
}

export interface MonacoLibEntry {
  name: string;
  content: string;
}
