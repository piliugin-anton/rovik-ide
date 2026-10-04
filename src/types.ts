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
  /** The file is gone from disk; the tab keeps its text until saved or closed. */
  deleted?: boolean;
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
  /** The model doesn't support tool calls: send no tools. */
  noTools: boolean;
  /** How long to wait for the first token (default 60 s). */
  firstByteTimeoutMs?: number;
  /** Reasoning effort support (lib/reasoning.ts). */
  reasoning: import('./lib/reasoning').ReasoningSetting;
}

export interface ProviderModel {
  id: string; // exactly as the API expects it, e.g. "anthropic/claude-sonnet-4.5"
  ownedBy?: string; // `owned_by` from GET /models, when present
  enabled: boolean; // shown in the chat model picker
  vision?: boolean; // accepts image input (set by the user; /models doesn't say)
  contextLength?: number; // context window in tokens, if GET /models reported it
  contextOverride?: number; // typed in provider settings; wins over contextLength
  noTools?: boolean; // the model can't use tool calls
  firstTokenTimeoutSec?: number; // override of the 60 s first-token timeout
  /** How a reasoning effort is sent, and what's been learned (lib/reasoning.ts). */
  reasoning?: import('./lib/reasoning').ReasoningSetting;
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
  /** Provider data echoed back with the call (Gemini 3 thought signature). */
  extraContent?: unknown;
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
  /** Live status while waiting or retrying (UI only, never stored). */
  status?: MessageStatus;
}

/** The approval decision on one tool call. */
export interface ToolApproval {
  by: 'user' | 'ai';
  approved: boolean;
  /** "By AI": the model that decided. */
  model?: string;
  reason?: string;
}

/** Small per-message metadata, stored with the message. */
export interface ToolMeta {
  /** Tool results: counts for the block header. */
  count?: number;
  pageTitle?: string;
  /** The request failed before anything was shown. */
  failure?: StoredFailure;
  /** Part of the reply arrived, then the request failed. */
  interrupted?: StoredFailure;
  /** run_command results: the exit code (null when killed or signalled). */
  exitCode?: number | null;
  /** Tool results that went through approval: who decided, and why. */
  approval?: ToolApproval;
  /**
   * User messages: the text as sent to the model (with editor context and
   * file references), reused when the turn is retried or continued.
   */
  sentContent?: string;
  /**
   * An empty assistant message marking where a run paused (never sent to the
   * model): after a number of tool calls, or because it looked stuck.
   */
  paused?: { kind: 'checkpoint' | 'loop'; toolCalls: number; reason?: string };
  /** The verification gate's record of a round of checks (lib/verification.ts). */
  verification?: import('./lib/verification').VerificationMeta;
  /**
   * A user-role message the IDE wrote (check failures for the agent). Not
   * shown as a user message, and never mistaken for the user's request.
   */
  harness?: 'verification' | 'checklist';
  /** A Plan-mode plan: the file it's saved in (lib/plans.ts). */
  planFile?: string;
  /** The model's reasoning for this reply, and how long it took. */
  reasoning?: { text: string; ms?: number };
  /** A user message: the mode it was sent in (lib/chatMode.ts). */
  mode?: import('./lib/chatMode').ChatMode;
  /** An ask_user result: the questions and how they were answered. */
  questions?: {
    questions: import('./lib/questions').Question[];
    outcome: import('./lib/questions').QuestionsOutcome;
  };
}

export type ChatErrorKind =
  | 'empty_response'
  | 'stalled'
  | 'timeout'
  | 'rate_limited'
  | 'server_error'
  | 'network'
  | 'auth'
  | 'bad_request'
  | 'not_found'
  | 'context_length'
  | 'length'
  | 'provider'
  | 'cancelled';

/** Typed request failure from the backend (src-tauri/src/chat_request.rs). */
export interface ChatError {
  kind: ChatErrorKind;
  message: string;
  retryable: boolean;
  retryAfterMs: number | null;
  attempts: number;
  status: number | null;
  details: string | null;
  partial: boolean;
}

/** The final outcome of a failed request, as stored. */
export interface StoredFailure {
  kind: ChatErrorKind;
  message: string;
  attempts: number;
  /** When a rate limit resets (epoch ms), if the provider said. */
  resetAt?: number;
  details?: string;
}

/** Live status of a pending reply (UI only, never stored). */
export interface MessageStatus {
  text: string;
  /** Show a countdown to this time (epoch ms). */
  until?: number;
  /** Show the time elapsed since this (epoch ms). */
  since?: number;
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
  /** The forms to send it in (lib/reasoning.ts); absent: the older default. */
  reasoning_formats?: string[] | null;
}

export type ChatStreamEvent =
  | { type: 'Delta'; content: string }
  /** The model's reasoning, apart from the reply. */
  | { type: 'Reasoning'; content: string }
  /** Which reasoning forms the provider accepted or rejected (lib/reasoning.ts). */
  | { type: 'ReasoningSupport'; accepted: string[]; rejected: string[] }
  | { type: 'Done' }
  /** How the reply ended (diagnostics), just before Done. */
  | { type: 'Finished'; finish_reason: string | null; usage: Record<string, unknown> | null }
  | { type: 'Error'; error: ChatError }
  | { type: 'Retrying'; attempt: number; max_attempts: number; delay_ms: number; reason: ChatError }
  | { type: 'ToolCallStart'; id: string; name: string }
  | { type: 'ToolCallArgumentsDelta'; id: string; arguments_delta: string }
  | {
      type: 'ToolCallComplete';
      id: string;
      name: string;
      arguments: string;
      extra_content?: unknown;
    };

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
  /** A reference to these lines only (1-based, inclusive), e.g. an editor selection. */
  lines?: { start: number; end: number };
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
    /** Relative to `workdir`. */
    path: string;
    status: GitFileStatusCode;
    /** Changed in the index / in the working tree. */
    staged: boolean;
    unstaged: boolean;
  }[];
  /** The repository's working folder (may be above the project folder). */
  workdir: string;
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
