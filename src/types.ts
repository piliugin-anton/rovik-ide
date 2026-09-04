export interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
}

/** A flattened, visible row in the file tree — what actually gets rendered/virtualized. */
export interface TreeRow {
  path: string;
  name: string;
  isDir: boolean;
  depth: number;
  loading?: boolean;
}

export interface OpenFile {
  path: string;
  name: string;
  content: string;
  originalContent: string;
  dirty: boolean;
}

export interface OpenFile {
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
  stream: boolean
}

export interface ChatToolCall {
  id: string;
  name: string;
  arguments: string; // raw JSON string, as returned by the API
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  toolCalls?: ChatToolCall[];
  pending?: boolean;
  error?: string;
}

export type ChatStreamEvent =
  | {
      type: "Delta";
      data: string;
    }
  | {
      type: "Done";
      data?: never;
    }
  | {
      type: "Error";
      data: string;
    };