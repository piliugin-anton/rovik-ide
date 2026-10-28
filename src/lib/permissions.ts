// permissions.ts
//
// What the agent may do, and what needs approval first. Two layers:
//
//   1. Permission: may this run at all? `bash` is restricted by the
//      allowed-commands list (or "any command"), the web tools by the
//      allowed hosts (or "any host"). A refused call never reaches approval.
//   2. Approval: commands, file edits and the other tool calls each have
//      their own rule: whether approval is required, and who gives it (the
//      user, or the AI reviewer model).
//
// Stored with the AI settings (lib/aiSettings.ts), and sent to the backend,
// which enforces them (src-tauri/src/agent_run/permissions.rs). The command
// analysis below is a copy of the backend's, for the settings' hints: keep
// the two in step.
import type { ModelSelection } from '../types';

/** Default for the "pause every N tool calls" setting (agent_run/guard.rs). */
export const DEFAULT_TOOL_CALL_CHECKPOINT = 50;
/** The setting's choices; null = never pause on count alone. */
export const TOOL_CALL_CHECKPOINT_CHOICES: (number | null)[] = [25, 50, 100, 200, null];

/** Who approves: the user, or the AI reviewer. */
export type Approver = 'user' | 'ai';

export interface ApprovalRule {
  /** Every call in the group waits for approval. */
  required: boolean;
  by: Approver;
}

export interface AgentPermissions {
  /** Every command is permitted; the list below is ignored. */
  allowAnyCommand: boolean;
  /** Command prefixes, e.g. "git status" permits "git status -s". */
  allowedCommands: string[];
  /** Every host may be reached over the network; the list below is ignored. */
  allowAnyHost: boolean;
  /**
   * The hosts the agent's work may reach otherwise, as `normalizeHostEntry`
   * keeps them: `docs.rs`, `*.github.com`, `10.0.0.5`. This computer's own
   * services (localhost) are always reachable
   * (docs/design/allowed-hosts-plan.md).
   */
  allowedHosts: string[];
  /** bash. */
  commandApproval: ApprovalRule;
  /** write. */
  editApproval: ApprovalRule;
  /** Every other tool: reading and searching files, web access. */
  toolApproval: ApprovalRule;
  /**
   * The tools of the user's MCP servers (`mcp`, and a connector's own MCP
   * calls): they change things outside the project, so they ask by default
   * (docs/design/mcp-plan.md, Q07).
   */
  mcpApproval: ApprovalRule;
  /** The model that reviews actions approved "by AI". */
  approvalModel: ModelSelection | null;
  /** Tried in order when the approval model fails. */
  approvalFallbacks: ModelSelection[];
  /**
   * Send the review as a JSON object instead of a text prompt (the input
   * format of the Typesafe Jev model). See src-tauri/src/agent_run/review.rs.
   */
  approvalTypedInput: boolean;
  /**
   * Pause the agent for the user after every this many tool calls in one
   * run; null never pauses on count (loop detection still does). See
   * src-tauri/src/agent_run/guard.rs.
   */
  toolCallCheckpoint: number | null;
  /**
   * When the agent finishes a run in which it changed files, run the
   * project's checks and send failures back to it (agent_run/verify.rs).
   */
  verifyChanges: boolean;
  /**
   * When the agent must keep a checklist (the todo tool, Agent mode):
   * 'auto' lets it decide by the "3 or more steps" rule and refuses work
   * once when a task clearly outgrew it without one; 'always' starts every
   * request with a checklist; 'off' never asks (src-tauri/src/agent_run).
   */
  checklist: ChecklistMode;
  /**
   * An isolated chat that's still running resolves its conflicts with the
   * project itself (conflict markers in its files); off, they wait for the
   * user (src-tauri/src/overlay/chats.rs).
   */
  chatResolvesConflicts: boolean;
}

export type ChecklistMode = 'off' | 'auto' | 'always';

export const DEFAULT_ALLOWED_COMMANDS = [
  'ls',
  'pwd',
  'cat',
  'head',
  'tail',
  'wc',
  'grep',
  'rg',
  'tree',
  'git status',
  'git diff',
  'git log',
  'git show',
  'npm test',
  'npm run test',
  'npm run lint',
  'npx tsc --noEmit',
  'cargo check',
  'cargo test',
  'cargo clippy',
];

export const DEFAULT_PERMISSIONS: AgentPermissions = {
  allowAnyCommand: false,
  allowedCommands: DEFAULT_ALLOWED_COMMANDS,
  allowAnyHost: true,
  allowedHosts: [],
  // The allowed-commands list is the safeguard by default; approval is
  // opt-in, so the agent works as before until the user turns it on.
  commandApproval: { required: false, by: 'user' },
  editApproval: { required: false, by: 'user' },
  toolApproval: { required: false, by: 'user' },
  mcpApproval: { required: true, by: 'user' },
  approvalModel: null,
  approvalFallbacks: [],
  approvalTypedInput: false,
  toolCallCheckpoint: DEFAULT_TOOL_CALL_CHECKPOINT,
  verifyChanges: true,
  checklist: 'auto',
  chatResolvesConflicts: true,
};

const isSelection = (v: unknown): v is ModelSelection =>
  !!v &&
  typeof v === 'object' &&
  typeof (v as ModelSelection).providerId === 'string' &&
  typeof (v as ModelSelection).modelId === 'string';

export const sameModel = (
  a: ModelSelection | null | undefined,
  b: ModelSelection | null | undefined
) => !!a && !!b && a.providerId === b.providerId && a.modelId === b.modelId;

function normalizeRule(raw: unknown): ApprovalRule | null {
  if (!raw || typeof raw !== 'object') return null;
  const r = raw as Record<string, unknown>;
  let required: boolean;
  if (typeof r.required === 'boolean') required = r.required;
  // Earlier "when": never / changes (only file changes) / always. Approval
  // for file changes becomes approval for everything in the group, so
  // nothing that used to ask runs unasked now.
  else if (r.when === 'never') required = false;
  else if (r.when === 'changes' || r.when === 'always') required = true;
  else return null;
  return { required, by: r.by === 'ai' ? 'ai' : 'user' };
}

/** Accepts whatever was stored (or nothing) and fills in the defaults. */
export function normalizePermissions(raw: unknown): AgentPermissions {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;

  /*
   * Older formats, applied to both groups:
   *  - one rule for everything (approvalEnabled, approvalMode,
   *    allowFileChangesWithoutApproval);
   *  - per-category allow/ask/off (writeFiles: 'ask').
   */
  let legacy: ApprovalRule | null = null;
  if (
    typeof r.approvalEnabled === 'boolean' ||
    typeof r.allowFileChangesWithoutApproval === 'boolean'
  ) {
    legacy = {
      required: r.approvalEnabled === true || r.allowFileChangesWithoutApproval === false,
      by: r.approvalMode === 'ai' ? 'ai' : 'user',
    };
  } else if (r.writeFiles === 'ask') {
    legacy = { required: true, by: 'user' };
  }

  const allowed = Array.isArray(r.allowedCommands)
    ? r.allowedCommands.filter((c): c is string => typeof c === 'string' && !!c.trim())
    : DEFAULT_PERMISSIONS.allowedCommands;
  const hosts = Array.isArray(r.allowedHosts)
    ? r.allowedHosts.flatMap((h) => {
        const entry = typeof h === 'string' ? normalizeHostEntry(h) : null;
        return entry ? [entry] : [];
      })
    : DEFAULT_PERMISSIONS.allowedHosts;

  return {
    allowAnyCommand:
      typeof r.allowAnyCommand === 'boolean'
        ? r.allowAnyCommand
        : DEFAULT_PERMISSIONS.allowAnyCommand,
    allowedCommands: allowed,
    allowAnyHost: r.allowAnyHost !== false,
    allowedHosts: [...new Set(hosts)],
    commandApproval:
      normalizeRule(r.commandApproval) ?? legacy ?? DEFAULT_PERMISSIONS.commandApproval,
    toolApproval: normalizeRule(r.toolApproval) ?? legacy ?? DEFAULT_PERMISSIONS.toolApproval,
    // Saved before the MCP rule: MCP calls ask, whatever the other rules say.
    mcpApproval: normalizeRule(r.mcpApproval) ?? DEFAULT_PERMISSIONS.mcpApproval,
    // Edits used to follow the tool-calls rule.
    editApproval:
      normalizeRule(r.editApproval) ??
      normalizeRule(r.toolApproval) ??
      legacy ??
      DEFAULT_PERMISSIONS.editApproval,
    approvalModel: isSelection(r.approvalModel) ? r.approvalModel : null,
    approvalFallbacks: Array.isArray(r.approvalFallbacks)
      ? r.approvalFallbacks.filter(isSelection)
      : [],
    approvalTypedInput: r.approvalTypedInput === true,
    toolCallCheckpoint:
      r.toolCallCheckpoint === null
        ? null
        : typeof r.toolCallCheckpoint === 'number' && r.toolCallCheckpoint >= 1
          ? Math.floor(r.toolCallCheckpoint)
          : DEFAULT_TOOL_CALL_CHECKPOINT,
    verifyChanges: r.verifyChanges !== false,
    checklist:
      r.checklist === 'off' || r.checklist === 'always'
        ? r.checklist
        : DEFAULT_PERMISSIONS.checklist,
    chatResolvesConflicts: r.chatResolvesConflicts !== false,
  };
}

/*
 * ---------------------------------------------------------------------------
 * Allowed hosts
 * ---------------------------------------------------------------------------
 *
 * A copy of the backend's reading of an entry (rovik-core's
 * allowed_hosts.rs), which matches hosts against the list: the cases in
 * test-fixtures/allowed-hosts.json hold both to the same answers.
 */

/**
 * An allowed-host entry as typed, as the settings keep it, or null when it
 * can't be one: a name (`docs.rs`), its subdomains (`*.github.com`) or an
 * address (`10.0.0.5`, `[fd00::1]`), read as a URL's host is (lower case,
 * punycode). A URL's scheme, sign-in, port and path are dropped.
 */
export function normalizeHostEntry(raw: string): string | null {
  let text = raw.trim();
  const scheme = text.indexOf('://');
  if (scheme >= 0) text = text.slice(scheme + 3);
  text = text.split(/[/?#]/)[0];
  text = text.slice(text.lastIndexOf('@') + 1);
  const subdomains = text.startsWith('*.');
  if (subdomains) text = text.slice(2);
  const host = urlHost(withoutPort(text));
  if (host === null || !subdomains) return host;
  // `*.10.0.0.5` names nothing.
  return host.startsWith('[') || /^\d+\.\d+\.\d+\.\d+$/.test(host) ? null : `*.${host}`;
}

/** `text` without a port after it; a bare IPv6 address keeps its colons. */
function withoutPort(text: string): string {
  if (text.startsWith('[')) {
    const end = text.indexOf(']');
    return end >= 0 ? text.slice(0, end + 1) : text;
  }
  const colon = text.indexOf(':');
  return colon >= 0 && !text.slice(colon + 1).includes(':') ? text.slice(0, colon) : text;
}

/** `text`, a host, as a URL has it, without a trailing dot; null when it isn't one. */
function urlHost(text: string): string | null {
  const trimmed = text.trim();
  const bracketed = trimmed.includes(':') && !trimmed.startsWith('[') ? `[${trimmed}]` : trimmed;
  let host: string;
  try {
    host = new URL(`http://${bracketed}/`).hostname.replace(/\.+$/, '');
  } catch {
    return null;
  }
  if (host.startsWith('[')) return host;
  const named = host.length <= 253 && host.split('.').every((label) => /^[a-z0-9_-]+$/.test(label));
  return named ? host : null;
}

/*
 * ---------------------------------------------------------------------------
 * Shell command analysis
 * ---------------------------------------------------------------------------
 *
 * Deliberately simple and conservative: a command is split into simple
 * commands at && || ; | and newlines (quotes respected); each must match the
 * allowed list. Anything that can't be checked that way (command
 * substitution, subshells) is refused unless any command is allowed. A
 * command counts as read-only only when every part is on the known
 * read-only list below; anything unknown is treated as modifying files.
 */

interface ParsedCommand {
  /** Simple commands, each as words (leading harmless VAR=value assignments removed). */
  segments: string[][];
  /** Output redirected into a file (> or >>), other than /dev/null. */
  writesViaRedirect: boolean;
  /** Command substitution or subshells: $( ), backticks, <( ), ( ). */
  opaque: boolean;
}

export function parseCommand(command: string): ParsedCommand {
  const segments: string[][] = [];
  let words: string[] = [];
  let word = '';
  let inWord = false;
  let quote: '"' | "'" | null = null;
  let writesViaRedirect = false;
  let opaque = false;

  const endWord = () => {
    if (inWord) words.push(word);
    word = '';
    inWord = false;
  };
  const endSegment = () => {
    endWord();
    const body = dropAssignments(words);
    if (body.length) segments.push(body);
    words = [];
  };

  for (let i = 0; i < command.length; i++) {
    const c = command[i];
    const next = command[i + 1];

    if (quote) {
      if (c === quote) quote = null;
      else if (c === '\\' && quote === '"' && next !== undefined) word += command[++i];
      else {
        if (quote === '"' && (c === '`' || (c === '$' && next === '('))) opaque = true;
        word += c;
      }
      continue;
    }

    if (c === "'" || c === '"') {
      quote = c;
      inWord = true;
      continue;
    }
    if (c === '\\' && next !== undefined) {
      word += next;
      inWord = true;
      i++;
      continue;
    }
    if (
      c === '`' ||
      (c === '$' && next === '(') ||
      (c === '<' && next === '(') ||
      c === '(' ||
      c === ')'
    ) {
      opaque = true;
      inWord = true;
      word += c;
      continue;
    }
    if (c === '&' && next === '>') {
      // "&>file": redirects both streams; the ">" is handled next.
      continue;
    }
    if (c === '&' || c === '|' || c === ';' || c === '\n') {
      endSegment();
      if ((c === '&' || c === '|') && next === c) i++;
      continue;
    }
    if (c === '>') {
      // A descriptor number right before ">" (the 2 in "2>") isn't a word.
      if (/^\d+$/.test(word)) {
        word = '';
        inWord = false;
      } else {
        endWord();
      }
      let j = i + 1;
      if (command[j] === '>') j++;
      if (command[j] === '&') {
        // ">&2" duplicates a descriptor; no file is written.
        j++;
        while (j < command.length && /[\d-]/.test(command[j])) j++;
        i = j - 1;
        continue;
      }
      while (command[j] === ' ' || command[j] === '\t') j++;
      let target = '';
      while (j < command.length && !/[\s;&|<>()]/.test(command[j])) target += command[j++];
      if (target !== '/dev/null' && target.toLowerCase() !== 'nul') writesViaRedirect = true;
      i = j - 1;
      continue;
    }
    if (c === '<') {
      // Input redirection: drop the file name, it doesn't change permissions.
      endWord();
      let j = i + 1;
      while (command[j] === ' ' || command[j] === '\t') j++;
      while (j < command.length && !/[\s;&|<>()]/.test(command[j])) j++;
      i = j - 1;
      continue;
    }
    if (c === ' ' || c === '\t') {
      endWord();
      continue;
    }
    word += c;
    inWord = true;
  }
  endSegment();

  return { segments, writesViaRedirect, opaque };
}

/**
 * Variables a leading `NAME=value` may set without changing which program
 * runs or what it runs. Mirrors `HARMLESS_VARIABLES` in permissions.rs.
 */
const HARMLESS_VARIABLES = new Set([
  'CI',
  'NO_COLOR',
  'FORCE_COLOR',
  'CLICOLOR',
  'CLICOLOR_FORCE',
  'CARGO_TERM_COLOR',
  'TERM',
  'COLUMNS',
  'LANG',
  'LANGUAGE',
  'LC_ALL',
  'LC_CTYPE',
  'LC_MESSAGES',
  'LC_COLLATE',
  'TZ',
  'RUST_LOG',
  'RUST_BACKTRACE',
  'PYTHONUNBUFFERED',
  'PYTHONDONTWRITEBYTECODE',
]);

/** A harmless variable set to a plain value: no path, quotes or expansions. */
function harmlessAssignment(word: string): boolean {
  const eq = word.indexOf('=');
  if (eq < 0) return false;
  return (
    HARMLESS_VARIABLES.has(word.slice(0, eq)) && /^[A-Za-z0-9_.,:@+-]*$/.test(word.slice(eq + 1))
  );
}

/**
 * Drops leading harmless assignments. Any other assignment stays part of the
 * command (`GIT_EXTERNAL_DIFF=./x git diff` runs `./x`), so an allowed
 * `git diff` doesn't permit it.
 */
function dropAssignments(words: string[]): string[] {
  let i = 0;
  while (i < words.length && harmlessAssignment(words[i])) i++;
  return words.slice(i);
}

/** "git status" permits "git status -s", not "git stash". */
function matchesAllowed(segment: string[], allowed: string[][]): boolean {
  return allowed.some(
    (entry) =>
      entry.length > 0 && entry.length <= segment.length && entry.every((w, i) => w === segment[i])
  );
}

/**
 * A `cd` into one of the project's folders (`cd web`, `cd packages/api`): it
 * runs nothing, so the allowed list doesn't need it. Not a folder outside the
 * project, one the shell would expand, or `cd`'s options. Mirrors
 * `enters_project_folder` in permissions.rs.
 */
function entersProjectFolder(segment: string[]): boolean {
  const [cd, folder] = segment;
  if (segment.length !== 2 || cd !== 'cd' || !folder) return false;
  if (/^[-~/\\]/.test(folder) || /[$%`*?[{:]/.test(folder)) return false;
  let depth = 0;
  for (const part of folder.split(/[/\\]/)) {
    if (part === '..') {
      if (depth === 0) return false;
      depth--;
    } else if (part !== '' && part !== '.') {
      depth++;
    }
  }
  return true;
}

export function splitWords(entry: string): string[] {
  return entry.trim().split(/\s+/).filter(Boolean);
}

const READ_ONLY_COMMANDS = new Set([
  'cd',
  'ls',
  'll',
  'dir',
  'pwd',
  'cat',
  'head',
  'tail',
  'wc',
  'grep',
  'egrep',
  'fgrep',
  'rg',
  'ag',
  'tree',
  'echo',
  'printf',
  'which',
  'whereis',
  'type',
  'whoami',
  'date',
  'env',
  'printenv',
  'stat',
  'file',
  'du',
  'df',
  'diff',
  'cmp',
  'uniq',
  'cut',
  'tr',
  'nl',
  'column',
  'basename',
  'dirname',
  'realpath',
  'readlink',
  'true',
  'false',
  'test',
  'uname',
  'hostname',
  'jq',
  'sha256sum',
  'md5sum',
  'less',
  'more',
  'awk',
]);

const READ_ONLY_GIT = new Set([
  'status',
  'diff',
  'log',
  'show',
  'rev-parse',
  'ls-files',
  'ls-tree',
  'blame',
  'describe',
  'shortlog',
  'grep',
  'reflog',
  'cat-file',
  'whatchanged',
  'name-rev',
  'merge-base',
  'count-objects',
]);

/** Checks, tests, linters: they produce build output but don't edit the project's files. */
const CHECK_COMMANDS: string[][] = [
  ['cargo', 'check'],
  ['cargo', 'test'],
  ['cargo', 'clippy'],
  ['cargo', 'build'],
  ['cargo', 'tree'],
  ['cargo', 'metadata'],
  ['cargo', 'doc'],
  ['npm', 'test'],
  ['npm', 't'],
  ['npm', 'run', 'test'],
  ['npm', 'run', 'lint'],
  ['npm', 'run', 'build'],
  ['npm', 'run', 'typecheck'],
  ['npm', 'ls'],
  ['npm', 'outdated'],
  ['pnpm', 'test'],
  ['pnpm', 'run', 'test'],
  ['pnpm', 'run', 'lint'],
  ['pnpm', 'run', 'build'],
  ['yarn', 'test'],
  ['yarn', 'lint'],
  ['yarn', 'build'],
  ['npx', 'tsc', '--noEmit'],
  ['tsc', '--noEmit'],
  ['npx', 'vitest', 'run'],
  ['npx', 'jest'],
  ['npx', 'eslint'],
  ['npx', 'prettier', '--check'],
  ['pytest'],
  ['python', '-m', 'pytest'],
  ['go', 'test'],
  ['go', 'vet'],
  ['go', 'build'],
  ['make', 'test'],
  ['make', 'check'],
];

function segmentIsReadOnly(seg: string[]): boolean {
  const [cmd, ...rest] = seg;
  if (!cmd) return true;
  // Version and help queries never change anything.
  if (rest.length === 1 && ['--version', '-v', '-V', '--help', '-h', 'version'].includes(rest[0]))
    return true;
  if (cmd === 'find') {
    return !rest.some((a) =>
      ['-delete', '-exec', '-execdir', '-ok', '-okdir', '-fprint', '-fprintf', '-fls'].includes(a)
    );
  }
  if (cmd === 'sort') return !rest.some((a) => a === '-o' || a.startsWith('--output'));
  if (cmd === 'awk') return !rest.some((a) => /system\(|>\s*"/.test(a));
  if (READ_ONLY_COMMANDS.has(cmd)) return true;

  if (cmd === 'git') {
    const sub = rest.find((a) => !a.startsWith('-'));
    if (!sub) return true;
    const args = rest.slice(rest.indexOf(sub) + 1);
    if (READ_ONLY_GIT.has(sub)) return true;
    const flagsOnly = args.every((a) => a.startsWith('-'));
    if (sub === 'branch' || sub === 'tag')
      return (
        flagsOnly && !args.some((a) => /^-(d|D|m|M|c|C|f)$|^--(delete|move|copy|force)/.test(a))
      );
    if (sub === 'remote') return args.length === 0 || (args.length === 1 && args[0] === '-v');
    if (sub === 'stash') return args[0] === 'list' || args[0] === 'show';
    if (sub === 'config')
      return args.some((a) => a === '--get' || a === '--list' || a === '-l' || a === '--get-all');
    return false;
  }

  // Checkers that rewrite files when asked to (eslint --fix, prettier --write).
  if (rest.some((a) => /^(--fix|--write|--in-place)\b/.test(a))) return false;
  return CHECK_COMMANDS.some((pattern) => pattern.every((w, i) => seg[i] === w));
}

export type CommandVerdict =
  { permitted: true; modifiesFiles: boolean } | { permitted: false; reason: string };

/** The simple commands the allowed list doesn't cover: no entry of it, nor a `cd` into a project folder. */
function unallowed(segments: string[][], permissions: AgentPermissions): string[][] {
  const allowed = permissions.allowedCommands.map(splitWords).filter((w) => w.length > 0);
  return segments.filter((seg) => !matchesAllowed(seg, allowed) && !entersProjectFolder(seg));
}

/**
 * What the allowed list lacks for `command` to run: each simple command of it
 * that no entry covers yet, as an entry of its own. An entry permits one
 * simple command, so a check in a folder (`cd api && go test ./...`) needs
 * `go test ./...`, not its whole line.
 */
export function entriesToAllow(command: string, permissions: AgentPermissions): string[] {
  const parts = unallowed(parseCommand(command).segments, permissions).map((seg) => seg.join(' '));
  return [...new Set(parts)];
}

export function checkCommand(command: string, permissions: AgentPermissions): CommandVerdict {
  const parsed = parseCommand(command);
  if (parsed.segments.length === 0) return { permitted: false, reason: 'The command is empty.' };

  const modifiesFiles =
    parsed.writesViaRedirect || parsed.opaque || !parsed.segments.every(segmentIsReadOnly);

  if (permissions.allowAnyCommand) return { permitted: true, modifiesFiles };

  if (parsed.opaque) {
    return {
      permitted: false,
      reason:
        'Command substitution and subshells ($( ), backticks, parentheses) can’t be checked against the allowed-commands list. Run the parts as separate commands.',
    };
  }

  const refused = unallowed(parsed.segments, permissions);
  if (refused.length > 0) {
    const names = refused.map((seg) => `"${seg.join(' ')}"`).join(', ');
    return {
      permitted: false,
      reason: `Not in the user's allowed commands: ${names}. Allowed: ${
        permissions.allowedCommands.length ? permissions.allowedCommands.join(', ') : '(none)'
      }.`,
    };
  }

  return { permitted: true, modifiesFiles };
}

/*
 * ---------------------------------------------------------------------------
 * Tool calls
 * ---------------------------------------------------------------------------
 */

function parseToolArgs(argsJson: string): Record<string, unknown> {
  try {
    const parsed = argsJson.trim() ? JSON.parse(argsJson) : {};
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch {
    return {};
  }
}

/** Whether some approval is set to be done by the AI reviewer. */
export function usesAiReview(p: AgentPermissions): boolean {
  return [p.commandApproval, p.editApproval, p.toolApproval, p.mcpApproval].some(
    (r) => r.required && r.by === 'ai'
  );
}

/** One line describing a tool call, as the chat's tool blocks show it. */
export function describeToolCall(name: string, argsJson: string): string {
  const args = parseToolArgs(argsJson);
  const str = (k: string) => (typeof args[k] === 'string' ? (args[k] as string) : undefined);

  switch (name) {
    case 'bash':
      return `Run \`${str('command') ?? '…'}\`${args.background === true ? ' in the background' : ''}`;
    case 'background': {
      const id = str('id') ?? 'a background command';
      if (args.action === 'output') return `Read what ${id} printed`;
      if (args.action === 'stop') return `Stop ${id}`;
      return 'List the commands running in the background';
    }
    case 'read':
      return `Read ${str('path') ?? 'a file'}`;
    case 'list_dir':
      return `List ${str('path') || 'the project root'}`;
    case 'search_code':
      return `Search the project for “${str('query') ?? '…'}”`;
    case 'neighbours':
      return `Graph neighbours of ${str('path') ?? 'a file'}`;
    case 'callers':
      return `Callers of ${str('symbol') ?? '…'}`;
    case 'outline':
      return `Outline ${str('path') ?? 'a file'}`;
    case 'diagnostics':
      return str('path') ? `Diagnostics for ${str('path')}` : 'Diagnostics summary';
    case 'write': {
      if (args.old_text !== undefined && args.old_text !== null) {
        const start = args.start_line;
        const end = args.end_line ?? start;
        const range = start !== undefined ? ` (only lines ${start}-${end})` : '';
        return `Edit ${str('path') ?? 'a file'}${range}`;
      }
      const content = str('content');
      const size = content !== undefined ? ` (${content.split('\n').length} lines)` : '';
      return `Write ${str('path') ?? 'a file'}${size}`;
    }
    case 'web_search':
      return `Search the web for “${str('query') ?? '…'}”`;
    case 'browse_url':
      return `Open ${str('url') ?? 'a web page'}`;
    case 'browser_screenshot':
      return `Take a screenshot of ${str('url') ?? 'a web page'}`;
    case 'mcp': {
      // The server's tool, as the backend's approval names it.
      const tool = str('tool') ?? 'an MCP tool';
      if (args.describe === true) return `Read about ${tool}`;
      const given = args.arguments;
      return given === undefined || given === null
        ? `Call ${tool}`
        : `Call ${tool} with ${JSON.stringify(given).slice(0, 200)}`;
    }
    default:
      return `${name}(${argsJson.slice(0, 200)})`;
  }
}

/*
 * ---------------------------------------------------------------------------
 * One line each for the Settings menu
 * ---------------------------------------------------------------------------
 */

/** "no approval", "your approval" or "AI approval". */
export function describeRule(rule: ApprovalRule): string {
  return !rule.required ? 'no approval' : rule.by === 'ai' ? 'AI approval' : 'your approval';
}

/** The Permissions & approval page in one line. */
export function permissionsSummary(p: AgentPermissions): string {
  const commands = p.allowAnyCommand ? 'any' : `${p.allowedCommands.length} listed`;
  const hosts = p.allowAnyHost ? 'any' : `${p.allowedHosts.length} listed`;
  return `Commands: ${commands}, ${describeRule(p.commandApproval)} · Hosts: ${hosts} · Edits: ${describeRule(
    p.editApproval
  )} · Other tools: ${describeRule(p.toolApproval)} · MCP tools: ${describeRule(p.mcpApproval)}`;
}

const CHECKLIST_SUMMARY: Record<ChecklistMode, string> = {
  auto: 'checklist when needed',
  always: 'checklist always',
  off: 'no checklist',
};

/** The Agent runs page in one line. */
export function agentRunsSummary(p: AgentPermissions): string {
  const pause =
    p.toolCallCheckpoint === null
      ? 'Never pauses on count'
      : `Pauses every ${p.toolCallCheckpoint} tool calls`;
  const checks = p.verifyChanges ? 'checks changes when a run finishes' : 'no checks after a run';
  return `${pause} · ${CHECKLIST_SUMMARY[p.checklist]} · ${checks}`;
}
