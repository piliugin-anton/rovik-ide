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

/**
 * A resolved provider + model pair, ready to send requests with. Built from
 * the active selection at runtime; not persisted.
 */
export interface ModelProfile {
  id: string; // `${providerId}::${modelId}`
  label: string; // "Provider/developer/model"
  model: string; // e.g. "gpt-4o-mini"
  baseUrl: string; // e.g. "https://api.openai.com/v1"
  apiKey: string;
  stream: boolean;
  /** Model accepts image input; enables the screenshot tool. */
  vision: boolean;
  /** Context window in tokens: the user's override, else the reported value. */
  contextTokens?: number;
}

export interface ProviderModel {
  id: string; // exactly as the API expects it, e.g. "anthropic/claude-sonnet-4.5"
  ownedBy?: string; // `owned_by` from GET /models, when present
  enabled: boolean; // shown in the chat model picker
  vision?: boolean; // accepts image input (set by the user; /models doesn't say)
  contextLength?: number; // context window in tokens, if GET /models reported it
  contextOverride?: number; // typed in provider settings; wins over contextLength
}

/** One OpenAI-compatible endpoint + credentials, with the models it serves. */
export interface Provider {
  id: string;
  name: string; // e.g. "OpenRouter"
  baseUrl: string;
  apiKey: string;
  stream: boolean;
  models: ProviderModel[];
}

export interface ModelSelection {
  providerId: string;
  modelId: string;
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

export interface ToolSearchLine {
  path: string;
  line: number;
  text: string;
  /** false for context lines around a match. */
  isMatch: boolean;
  /** Match positions within `text` (UTF-16 indexes). */
  ranges?: [number, number][];
  /** 1-based column of the first match in the original line. */
  column?: number;
}

/** Ask the editor to select a range in a file (e.g. a search result). */
export interface RevealRequest {
  path: string;
  line: number;
  column: number;
  length: number;
  /** Distinguishes repeated requests for the same spot. */
  nonce: number;
}

export interface ToolSearchResult {
  lines: ToolSearchLine[];
  matchCount: number;
  fileCount: number;
  /** Stopped at the result limit; more matches exist. */
  truncated: boolean;
}

export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant' | 'system' | 'tool';
  content: string;
  toolCalls?: ChatToolCall[];
  tool_call_id?: string; // if you later send tool results back
  pending?: boolean;
  error?: string;
  /** User message whose request never got an answer; kept for the record, not sent again. */
  failed?: boolean;
  /** Attachment ids (SHA-256) on a tool result, e.g. browser screenshots. */
  attachments?: string[];
  /** Position in the chat, assigned by the database. */
  seq?: number;
  /** A large tool result whose full text lives in the database (`content` is a preview). */
  bodyExternal?: boolean;
  bodyBytes?: number;
  /** Small summary computed when a tool finishes (counts for the block header). */
  meta?: ToolMeta;
  finalized?: boolean;
  createdAt?: number;
}

export interface ToolMeta {
  count?: number;
  pageTitle?: string;
}

/** A chat as listed: metadata only, no messages. */
export interface DialogSummary {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  messageCount: number;
  preview?: string | null;
}

export interface DialogCursor {
  updatedAt: number;
  id: string;
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

export type FileSuggestion = {
  id: string;
  path: string;
  relativePath: string;
  name: string;
};

export interface LastProjectOpenFile {
  id: string;
  path: string;
  name: string;
  language: string;
}

export interface LastProjectData {
  path: string | null;
  openedFiles: LastProjectOpenFile[] | null;
  expandedPaths: string[] | null;
  terminalCollapsed: boolean;
}

export type GitFileStatusCode = 'M' | 'A' | 'D' | 'R' | 'U' | '?';

export type GitStatus = {
  branch: string;
  behind: number;
  ahead: number;
  files: {
    path: string;
    status: GitFileStatusCode;
  }[];
};

export type GitPanelPhase = 'uninitialized' | 'ready' | 'no-repo' | 'error';

export interface WebSearchHit {
  title: string;
  url: string;
  snippet: string;
}

export interface BrowsedPage {
  url: string;
  title: string;
  text: string;
  truncated: boolean;
  links: { text: string; url: string }[];
}

export interface BrowserScreenshot {
  url: string;
  title: string;
  attachmentId: string;
  width: number;
  height: number;
}
