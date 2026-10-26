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
  /**
   * Model accepts image input: images attached in the chat are sent to it,
   * and the agent gets the browser_screenshot tool.
   */
  vision: boolean;
  /** Model accepts PDF files: PDFs attached in the chat are sent to it. */
  pdf: boolean;
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
  /** A connector's settings, when the model is a connector's (lib/connectors.ts). */
  connector?: Record<string, unknown>;
}

/**
 * Which OpenAI API a model is served on: `/responses`, or
 * `/chat/completions` for providers and models without it. Detected by the
 * backend (src-tauri/crates/rovik-core/src/chat_request/responses.rs) and
 * saved with the model; one with none saved is asked on `/responses` first.
 */
export type ApiRoute = 'chat_completions' | 'responses';

export interface ProviderModel {
  id: string; // exactly as the API expects it, e.g. "anthropic/claude-sonnet-4.5"
  ownedBy?: string; // `owned_by` from GET /models, when present
  enabled: boolean; // shown in the chat model picker
  vision?: boolean; // accepts image input (set by the user; /models doesn't say)
  pdf?: boolean; // accepts PDF files (set by the user, as vision)
  contextLength?: number; // context window in tokens, if GET /models reported it
  contextOverride?: number; // typed in provider settings; wins over contextLength
  noTools?: boolean; // the model can't use tool calls
  firstTokenTimeoutSec?: number; // override of the 60 s first-token timeout
  /** How a reasoning effort is sent, and what's been learned (lib/reasoning.ts). */
  reasoning?: import('./lib/reasoning').ReasoningSetting;
  /** The API route found to work (detected, not set by the user). */
  apiRoute?: ApiRoute;
}

/**
 * One source of models: an OpenAI-compatible endpoint with its credentials,
 * or a connector (lib/connectors.ts), a coding harness such as Claude Code
 * installed on the computer the chat runs on. A connector's `baseUrl` is
 * `connector://<kind>`, its token or key is `apiKey`, and what the harness
 * needs to run is `settings`; everything else (the models, the picker, the
 * saved model of a chat, the keychain) works as for a provider.
 */
export interface Provider {
  id: string;
  name: string; // e.g. "OpenRouter"
  baseUrl: string;
  apiKey: string;
  stream: boolean;
  models: ProviderModel[];
  /** A connector's kind; absent for a model provider. */
  connector?: ConnectorKind;
  /** A connector's settings (lib/connectors.ts, by kind). */
  settings?: Record<string, unknown>;
}

/** The harnesses that can be connected (rovik_protocol::connectors). */
export type ConnectorKind = 'claude-code';

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
  /**
   * Attachment ids (SHA-256): browser_screenshot results' screenshots, or
   * the images and PDFs attached to a user message (its PDFs are named in
   * `meta.pdfs`).
   */
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
/** Where a run paused and why (`PauseInfo`, src-tauri agent_run/guard.rs). */
export interface RunPause {
  /**
   * After a number of tool calls, or because it looked stuck; for the chat's
   * move to or from a host, or for an upgrade of the server's Rovik.
   */
  kind: 'checkpoint' | 'loop' | 'move' | 'drain';
  /** Tool calls run so far in the run, Continues included. */
  toolCalls: number;
  /** A loop's: why it looked stuck. */
  reason?: string;
  /** A move's: where the chat went, none for a move back to this computer. */
  host?: string;
}

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
  /** submit_plan results: the plan as checked (full, or after `edits`). */
  plan?: string;
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
  /** bash results: the exit code (null when killed or signalled, or while it still runs). */
  exitCode?: number | null;
  /** bash (`background`) results: the id of the command it left running, while it runs. */
  background?: string;
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
   * User messages to a connector: the text runs one of its commands
   * (`/compact`), so it goes as typed and alone (lib/connectorCommands.ts).
   */
  command?: boolean;
  /**
   * An empty assistant message marking where a run paused (never sent to the
   * model): after a number of tool calls, because it looked stuck, or for a
   * move or an upgrade (lib/runPause.ts).
   */
  paused?: RunPause;
  /** The verification gate's record of a round of checks (lib/verification.ts). */
  verification?: import('./lib/verification').VerificationMeta;
  /** An empty assistant message: an Autopilot task's stage (lib/autopilot.ts). */
  stage?: import('./lib/autopilot').StageMeta;
  /** An empty assistant message: the installs of a task's Prepare stage (lib/verification.ts). */
  prepare?: import('./lib/verification').PrepareMeta;
  /** An empty assistant message: the tests a task's Tests stage wrote first (lib/autopilot.ts). */
  tests?: import('./lib/autopilot').TestsMeta;
  /** An empty assistant message: an acceptance check of a task's change (lib/autopilot.ts). */
  acceptance?: import('./lib/autopilot').AcceptanceMeta;
  /** Autopilot, Thorough: one of the task's planners and its plan (T01). */
  planner?: import('./lib/autopilot').PlannerMeta;
  /** Autopilot, Thorough: the judge of the planners' plans (T02). */
  judge?: import('./lib/autopilot').JudgeMeta;
  /** Autopilot, Thorough: the adversarial tester's tests (T04). */
  breaker?: import('./lib/autopilot').BreakerMeta;
  /** Autopilot, Thorough: the task's two implementers, each in its own copy (T05). */
  candidates?: import('./lib/autopilot').CandidatesMeta;
  /** Autopilot, Thorough: the pick of the better candidate (T06). */
  pick?: import('./lib/autopilot').PickMeta;
  /** A task's report, its final message: the facts it was written from (lib/autopilot.ts). */
  report?: import('./lib/autopilot').ReportMeta;
  /** An assistant message a connector's harness wrote: what it did (lib/connectors.ts). */
  connector?: import('./lib/connectors').ConnectorMeta;
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
    | 'plan'
    | 'subagentBudget'
    | 'subagentFormat'
    | 'integrity'
    | 'review'
    /** The saved plan's problems, found when it's implemented (B07b). */
    | 'planCheck'
    /** An Autopilot stage's run: its request, which the IDE wrote. */
    | 'autopilot'
    /** Autopilot: a loop the run goes on from, told to change approach (U03). */
    | 'loop'
    /** Autopilot: criteria an acceptance check found unmet (S08). */
    | 'acceptance'
    /** Autopilot, Thorough: the planners' plan submitted for the run, or why none was (T01). */
    | 'thoroughPlan'
    /** Autopilot: what the task's report is written from (S09). */
    | 'report'
    /** Checks that fail only as on the code before the change: whether fixing
     * them is its task, the agent decides (agent_run/before.rs). */
    | 'preexisting';
  /** Dismiss the open checklist: don't carry it into the next request. */
  checklistClosed?: boolean;
  /** The checklist carry/continue note this harness message refers to. */
  fromMessageId?: string | null;
  /** A Plan-mode plan: the file it's saved in (lib/plans.ts). */
  planFile?: string;
  /**
   * The model's reasoning for this reply: `text` as shown, `ms` how long it
   * took; `details` and `model` are what is sent back to that model
   * (src-tauri/src/reasoning_echo.rs).
   */
  reasoning?: { text?: string; ms?: number; details?: unknown[]; model?: string };
  /** A user message: the mode it was sent in (lib/chatMode.ts). */
  mode?: import('./lib/chatMode').ChatMode;
  /**
   * A user message sent to the run while it worked (steering): part of the
   * request it was sent into, never a request of its own (lib/requests.ts).
   * `queuedAt`: when it was sent, in milliseconds since the epoch.
   */
  steer?: { queuedAt: number };
  /**
   * A user message: the PDFs among its `attachments`, with their names and
   * pages (the backend sends them under these names).
   */
  pdfs?: import('./lib/composerAttachments').ComposerPdf[];
  /** Agent message implementing a saved plan: the first turn forces the `todo` tool. */
  implementsPlan?: boolean;
  /** Its plan's file: the backend sends the model the plan from it (lib/plans.ts). */
  planPath?: string;
  /** The change reviewer's record (lib/changeReview.ts). */
  review?: import('./lib/changeReview').ReviewMeta;
  /** The plan checker's problems with a plan Agent mode was asked to implement (B07b). */
  planCheck?: { file: string; problems: string[] };
  /** A `task` result: the subagent that answered it (lib/subagents.ts). */
  subagent?: import('./lib/subagents').SubagentMeta;
  /** A parallel group's result (task kind "workers"): each worker's step and outcome. */
  group?: import('./lib/subagents').GroupMeta;
  /** A subagent chat's first message: the lead's brief. */
  subagentBrief?: { kind: string; parentCallId: string };
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
  /** The model's safety classifiers declined the request; `message` says why. */
  | 'refused'
  | 'cancelled'
  /** The agent's server restarted mid-reply, and settled what was stored of it. */
  | 'restarted';

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

/** Where a chat is when it isn't simply here: on a host, or moving there or back. */
export type ChatPlace =
  | { kind: 'host'; host: string }
  | { kind: 'moving'; host: string }
  | { kind: 'movingBack'; host: string };

/** A chat as listed: metadata only, no messages. */
export interface DialogSummary {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  messageCount: number;
  preview?: string | null;
  /** Absent for a chat on this computer. */
  place?: ChatPlace;
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

/** Files loaded for the editor's IntelliSense (rovik_core::typings). */
export interface LoadedFiles {
  files: { path: string; content: string }[];
  /** Links to folders: files are under the target, the path resolves to it. */
  links: { path: string; target: string }[];
  /** A limit was hit: some files weren't loaded. */
  truncated: boolean;
}

/** The dependencies the project reaches: their declarations and manifests. */
export interface LoadedTypings extends LoadedFiles {
  /** The packages loaded, each once: as imported, and its real folder. */
  packages: { name: string; dir: string }[];
  /** Imports no installed package was found for. */
  missing: string[];
}

/** What a project opens with: its sources and the dependencies they reach. */
export interface ProjectDefinitions {
  sources: LoadedFiles;
  typings: LoadedTypings;
}

/** The imports of one file, for loading the packages they reach. */
export interface ImportSite {
  /** The importing file's absolute path. */
  from: string;
  specifiers: string[];
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
  /** The branch's upstream ("origin/feature"); none for a branch with no
   *  remote branch yet, and off a branch. */
  upstream?: string | null;
  /** The remote the branch pushes to, where publishing it makes it; none
   *  with no remote, and off a branch. */
  push_remote?: string | null;
};

/** A branch the Git panel lists (src-tauri/src/git/branches.rs, `git_branches`). */
export interface GitBranch {
  /** `main`, or `origin/main` for a remote-tracking branch. */
  name: string;
  is_remote: boolean;
}

/** Where HEAD is (src-tauri/src/git/branches.rs, `git_head`). */
export interface GitHead {
  /** The branch HEAD is on; none when it's detached, or before the first commit. */
  branch: string | null;
  /** The repository's main branch: the one `origin/HEAD` names, else the
   *  first of `init.defaultBranch`, `main` and `master` that's a branch;
   *  none without any of them. */
  main_branch: string | null;
}

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
  result:
    | 'up-to-date'
    | 'fast-forward'
    | 'merged'
    | 'rebased'
    | 'conflicts'
    | 'needs-strategy'
    /** A rebase onto a branch would rewrite commits already pushed (git_rebase). */
    | 'rewrites-pushed';
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
