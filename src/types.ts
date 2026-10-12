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
  /** A preview tab (e.g. stashed content): shown read-only, never saved. */
  readOnly?: boolean;
  /** Which stash a read-only preview tab shows. */
  stash?: { index: number; file: string; message?: string };
  /** Which commit a read-only preview tab shows. */
  commit?: { oid: string; file: string; summary?: string };
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
  /** The API route the model was found to work on, once known. */
  apiRoute?: ApiRoute;
}

/**
 * Which OpenAI API a model is served on: `/chat/completions`, or
 * `/responses` for models only offered there. Detected by the backend
 * (src-tauri/src/chat_request/responses.rs) and saved with the model.
 */
export type ApiRoute = 'chat_completions' | 'responses';

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
  /** The API route found to work (detected, not set by the user). */
  apiRoute?: ApiRoute;
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
  /** The first matches in path order, up to the result limit. */
  lines: ToolSearchLine[];
  /** Matches and files in `lines`. */
  matchCount: number;
  fileCount: number;
  /** More matches exist than are in `lines`. */
  truncated: boolean;
  /** Matches and files in the whole project, shown or not. */
  totalMatches: number;
  totalFiles: number;
  /** false when counting stopped early: the totals are at least this many. */
  totalsComplete: boolean;
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
  /** Typed-input reviewer (Jev): each check and its probability, 0..1. */
  checks?: ReviewCheck[];
}

/** One check a typed-input reviewer scored. */
export interface ReviewCheck {
  label: string;
  score: number;
}

/** Small per-message metadata, stored with the message. */
export interface ToolMeta {
  /** Tool results: counts for the block header. */
  count?: number;
  /** list_dir results: the folder listed, relative to the project ("" for its root). */
  dir?: string;
  /** Whole-file writes: lines added and removed (against the file before). */
  added?: number;
  removed?: number;
  /** read by symbol: the lines of the declaration it found. */
  range?: [number, number];
  pageTitle?: string;
  /** The request failed before anything was shown. */
  failure?: StoredFailure;
  /** Part of the reply arrived, then the request failed. */
  interrupted?: StoredFailure;
  /** bash results: the exit code (null when killed or signalled). */
  exitCode?: number | null;
  /** bash results: a long wait for a CPU slot before it ran. */
  waited?: string;
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
  harness?:
    | 'verification'
    | 'checklist'
    | 'checklistCarried'
    | 'checklistContinued'
    | 'pauseSummary'
    | 'repetition'
    | 'plan';
  /** Dismiss the open checklist: don't carry it into the next request. */
  checklistClosed?: boolean;
  /** The checklist carry/continue note this harness message refers to. */
  fromMessageId?: string | null;
  /** A Plan-mode plan: the file it's saved in (lib/plans.ts). */
  planFile?: string;
  /** The model's reasoning for this reply, and how long it took. */
  /**
   * The model's reasoning: `text` as shown; `details` and `model` are what
   * is sent back to that model (src-tauri/src/reasoning_echo.rs).
   */
  reasoning?: { text?: string; ms?: number; details?: unknown[]; model?: string };
  /** A user message: the mode it was sent in (lib/chatMode.ts). */
  mode?: import('./lib/chatMode').ChatMode;
  /** Agent message implementing a saved plan: the first turn forces the `todo` tool. */
  implementsPlan?: boolean;
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
  | { type: 'ReasoningSupport'; accepted: string[]; rejected: string[]; unresolved: string[] }
  /** The API route the model was found to work on, to save with it. */
  | { type: 'ApiRoute'; route: ApiRoute }
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

/** Files loaded for the editor's IntelliSense (src-tauri/src/typings). */
export interface LoadedFiles {
  files: { path: string; content: string }[];
  /** A limit was hit: some files weren't loaded. */
  truncated: boolean;
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
    /** Added / removed lines vs HEAD (`git diff --numstat`). */
    additions: number;
    deletions: number;
  }[];
  /** The repository's working folder (may be above the project folder). */
  workdir: string;
  /** A merge / rebase in progress (src-tauri/src/git_sync.rs). */
  operation?: GitOperation | null;
};

/** A merge / rebase / … in progress (src-tauri/src/git_sync.rs). */
export interface GitOperation {
  kind: 'merge' | 'rebase' | 'cherry-pick' | 'revert' | 'bisect';
  /** The branch it's on (a rebase detaches HEAD). */
  branch: string | null;
  /** The merged branch, or what a rebase replays onto. */
  incoming: string | null;
  /** Rebase: the step being applied (1-based), of how many. */
  step: number | null;
  total: number | null;
  /** Rebase: summary of the commit being applied. */
  commit: string | null;
  conflicts: string[];
  /** The app can continue it (merge, rebase); others can only be aborted. */
  canContinue: boolean;
}

export type PullStrategy = 'merge' | 'rebase' | 'ff-only';

/** What `git pull` does here, per git config (git_pull_config). */
export interface GitPullConfig {
  strategy: PullStrategy | null;
  /** The key it comes from: "pull.rebase", "branch.main.rebase", "pull.ff". */
  source: string | null;
  noFf: boolean;
}

export interface GitPullOutcome {
  result: 'up-to-date' | 'fast-forward' | 'merged' | 'rebased' | 'conflicts' | 'needs-strategy';
  conflicts: string[];
}

export interface GitPushOutcome {
  /** "rejected": the remote has commits this branch doesn't. */
  result: 'pushed' | 'rejected';
  message: string | null;
}

/** A run of a conflicted file: shared lines, or a conflict. */
export type GitMergeSegment =
  | { kind: 'common'; text: string }
  | { kind: 'conflict'; ours: string; base: string; theirs: string };

/** One conflicted file, its sides and how git would merge them. */
export interface GitConflictFile {
  path: string;
  /** null: that side deleted the file. */
  ours: string | null;
  theirs: string | null;
  base: string | null;
  /** Not text: only a whole side can be taken. */
  binary: boolean;
  segments: GitMergeSegment[];
  /** Resolved (staged) already. */
  resolved: boolean;
  /** The working tree's version now. */
  current: string | null;
  oursLabel: string;
  theirsLabel: string;
}

export type GitPanelPhase = 'uninitialized' | 'ready' | 'no-repo' | 'error';

/** One entry of `git stash list`, newest first (src-tauri/src/git.rs). */
export interface GitStashEntry {
  index: number;
  message: string;
}

/** One file touched by a stash, from `git_stash_show` (src-tauri/src/git.rs). */
export interface GitStashFile {
  path: string;
  status: GitFileStatusCode;
  /** Added / removed lines in this stash. */
  additions: number;
  deletions: number;
  /** Whether this file matches the active history search query. */
  matched?: boolean;
  /** Why it matched: "filename", "content", or "both". */
  match_kind?: 'filename' | 'content' | 'both' | null;
}

/** One recent commit for the history view (src-tauri/src/git.rs, git_log). */
export interface GitCommitEntry {
  oid: string;
  short: string;
  summary: string;
  author: string;
  /** Commit time, seconds since the epoch. */
  time: number;
  /** Total files changed in this commit. */
  file_count?: number;
  /** Files matching the active history search (`undefined` when no query). */
  matched_file_count?: number | null;
}

/** One page of history (src-tauri/src/git.rs, git_log → GitLogPage).
 * `offset`/`next_offset` count scanned (raw) commits from `tip`; pass
 * `next_offset` back as `offset`, and `tip` back as `tip`, to continue.
 * `has_more` means older commits remain beyond this page's scan budget —
 * a query can return few matches and still be worth searching further. */
export interface GitLogPage {
  commits: GitCommitEntry[];
  has_more: boolean;
  next_offset: number;
  scanned: number;
  /** The commit the walk started from (HEAD on the first page). */
  tip?: string | null;
}

/** One file's stashed content plus the base it changed from (src-tauri/src/git.rs). */
export interface GitStashFilePreview {
  /** The stashed version; null when the file was deleted in the stash. */
  content: string | null;
  /** The version at the stash's base commit; null when the file is new. */
  base: string | null;
}

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
