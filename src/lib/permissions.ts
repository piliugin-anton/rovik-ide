// permissions.ts
//
// What the agent may do, and what needs approval first. Two layers:
//
//   1. Permission: may this run at all? Only `run_command` is restricted,
//      by the allowed-commands list (or "any command"). A refused call
//      never reaches approval.
//   2. Approval: commands, file edits and the other tool calls each have
//      their own rule: whether approval is required, and who gives it (the
//      user, or the AI reviewer model).
//
// Stored with the AI settings (lib/aiSettings.ts).
import type { ModelSelection } from '../types';

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
  /** run_command. */
  commandApproval: ApprovalRule;
  /** write_file and edit_file. */
  editApproval: ApprovalRule;
  /** Every other tool: reading and searching files, web access. */
  toolApproval: ApprovalRule;
  /** The model that reviews actions approved "by AI". */
  approvalModel: ModelSelection | null;
  /** Tried in order when the approval model fails. */
  approvalFallbacks: ModelSelection[];
  /**
   * Send the review as a JSON object instead of a text prompt (the input
   * format of the Typesafe Jev model). See lib/aiApproval.ts.
   */
  approvalTypedInput: boolean;
}

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
  // The allowed-commands list is the safeguard by default; approval is
  // opt-in, so the agent works as before until the user turns it on.
  commandApproval: { required: false, by: 'user' },
  editApproval: { required: false, by: 'user' },
  toolApproval: { required: false, by: 'user' },
  approvalModel: null,
  approvalFallbacks: [],
  approvalTypedInput: false,
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

  return {
    allowAnyCommand:
      typeof r.allowAnyCommand === 'boolean'
        ? r.allowAnyCommand
        : DEFAULT_PERMISSIONS.allowAnyCommand,
    allowedCommands: allowed,
    commandApproval:
      normalizeRule(r.commandApproval) ?? legacy ?? DEFAULT_PERMISSIONS.commandApproval,
    toolApproval: normalizeRule(r.toolApproval) ?? legacy ?? DEFAULT_PERMISSIONS.toolApproval,
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
  };
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
  /** Simple commands, each as words (leading VAR=value assignments removed). */
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

function dropAssignments(words: string[]): string[] {
  let i = 0;
  while (i < words.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(words[i])) i++;
  // "sudo"/"env" style wrappers would hide the real command.
  return words.slice(i);
}

/** "git status" permits "git status -s", not "git stash". */
function matchesAllowed(segment: string[], allowed: string[][]): boolean {
  return allowed.some(
    (entry) =>
      entry.length > 0 && entry.length <= segment.length && entry.every((w, i) => w === segment[i])
  );
}

export function splitWords(entry: string): string[] {
  return entry.trim().split(/\s+/).filter(Boolean);
}

const READ_ONLY_COMMANDS = new Set([
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

  const allowed = permissions.allowedCommands.map(splitWords).filter((w) => w.length > 0);
  const refused = parsed.segments.filter((seg) => !matchesAllowed(seg, allowed));
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

/** Tools that always change files; everything else (reads, web) doesn't. */
const FILE_MODIFYING_TOOLS = new Set(['write_file', 'edit_file']);

export type ToolVerdict =
  | { permitted: true; modifiesFiles: boolean; approval: Approver | null }
  | { permitted: false; reason: string };

function parseToolArgs(argsJson: string): Record<string, unknown> {
  try {
    const parsed = argsJson.trim() ? JSON.parse(argsJson) : {};
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch {
    return {};
  }
}

/** Permission first, then who (if anyone) has to approve. */
export function checkToolCall(
  name: string,
  argsJson: string,
  permissions: AgentPermissions
): ToolVerdict {
  let modifiesFiles = FILE_MODIFYING_TOOLS.has(name);

  if (name === 'run_command') {
    const command = parseToolArgs(argsJson).command;
    if (typeof command !== 'string' || !command.trim()) {
      // The executor reports the missing argument.
      return { permitted: true, modifiesFiles: false, approval: null };
    }
    const verdict = checkCommand(command, permissions);
    if (!verdict.permitted) return verdict;
    modifiesFiles = verdict.modifiesFiles;
  }

  const rule =
    name === 'run_command'
      ? permissions.commandApproval
      : FILE_MODIFYING_TOOLS.has(name)
        ? permissions.editApproval
        : permissions.toolApproval;
  return { permitted: true, modifiesFiles, approval: rule.required ? rule.by : null };
}

/** Whether some approval is set to be done by the AI reviewer. */
export function usesAiReview(p: AgentPermissions): boolean {
  return [p.commandApproval, p.editApproval, p.toolApproval].some(
    (r) => r.required && r.by === 'ai'
  );
}

/** Whether the model should be offered run_command at all. */
export function commandsAvailable(p: AgentPermissions): boolean {
  return p.allowAnyCommand || p.allowedCommands.some((c) => c.trim());
}

/** One line describing a tool call, for approval prompts and reviews. */
export function describeToolCall(name: string, argsJson: string): string {
  const args = parseToolArgs(argsJson);
  const str = (k: string) => (typeof args[k] === 'string' ? (args[k] as string) : undefined);

  switch (name) {
    case 'run_command':
      return `Run \`${str('command') ?? '…'}\``;
    case 'read_file':
      return `Read ${str('path') ?? 'a file'}`;
    case 'list_dir':
      return `List ${str('path') || 'the project root'}`;
    case 'search_code':
      return `Search the project for “${str('query') ?? '…'}”`;
    case 'write_file': {
      const content = str('content');
      const size = content !== undefined ? ` (${content.split('\n').length} lines)` : '';
      return `Write ${str('path') ?? 'a file'}${size}`;
    }
    case 'edit_file': {
      const start = args.start_line;
      const end = args.end_line ?? start;
      const range = start !== undefined ? ` (only lines ${start}-${end})` : '';
      return `Edit ${str('path') ?? 'a file'}${range}`;
    }
    case 'web_search':
      return `Search the web for “${str('query') ?? '…'}”`;
    case 'browse_url':
      return `Open ${str('url') ?? 'a web page'}`;
    case 'browser_screenshot':
      return `Take a screenshot of ${str('url') ?? 'a web page'}`;
    default:
      return `${name}(${argsJson.slice(0, 200)})`;
  }
}

/*
 * ---------------------------------------------------------------------------
 * Never left to the AI reviewer
 * ---------------------------------------------------------------------------
 *
 * A reviewer model only sees the text it's given: it can't tell that a
 * command reads a credential file or that a remote is production. These
 * shapes always go to the user, whatever the reviewer would say. Adapted
 * from OpenRouter's "Auto-Approve Coding Agent Permission Prompts with Jev"
 * recipe.
 */

const RISKY_COMMANDS = [
  // Hidden program names: quotes, escapes, variables, leading dashes/digits.
  /^(\S*['"\\=$]|[-0-9])/,
  /^(sudo|doas|su)\b/,
  // Running a string through another shell or interpreter hides its content.
  /^(bash|sh|zsh|fish|eval|xargs)\b/,
  /^(node|bun|deno|python3?)\s+(-\S+\s+)*(-c|-e|-p|--eval)\b/,
  /^rm\s+(-\S*[rRf]\S*\s+)+/,
  /^git\b.*\b(push|reset\s+--hard|clean\s+-\S*[fd]|branch\s+-D|checkout\s+--\s|restore\b)/,
  /^(npm|pnpm|yarn|bun|cargo)\s+publish\b/,
  /^(wrangler|vercel|fly|flyctl|netlify)\s+deploy\b|^terraform\s+(apply|destroy)\b|^kubectl\s+(delete|apply)\b/,
  /^(curl|wget)\b.*\s(-d|--data\S*|-F|--form|-T|--upload-file|-X\s*(POST|PUT|PATCH|DELETE))\b/,
  /^(chmod|chown)\s+-\S*R/,
  /\.env\b|\.ssh\b|\.aws\b|\.npmrc\b|\.netrc\b|id_rsa|credentials/i,
];

/** Paths whose contents are secrets: edits to them always go to the user. */
const SECRET_PATH =
  /(^|[\\/])(\.env(\.[^\\/]*)?|\.npmrc|\.netrc|id_[a-z0-9]+|[^\\/]*\.(pem|key|pfx|p12))$|(^|[\\/])(\.ssh|\.aws|\.gnupg)([\\/]|$)|credentials/i;

/** Strips wrappers and prefixes until the program name is bare. */
function bareCommand(part: string): string {
  let current = part;
  for (;;) {
    const next = current
      .trim()
      .replace(/^(if|then|elif|else|fi|while|until|do|done|for|in|case|esac|!)\s+/, '')
      .replace(
        /^(command|builtin|exec|env|nohup|time|timeout(\s+\S+)?|nice|watch|npx|bunx|pnpx)\s+/,
        ''
      )
      .replace(/^[A-Za-z_][A-Za-z0-9_]*=[^\s'"\\$]*\s+/, '')
      .replace(/^\\/, '')
      .replace(/^[^\s'"\\$]*\//, '');
    if (next === current) return current;
    current = next;
  }
}

/** A command the AI reviewer must never approve on its own. */
export function isRiskyCommand(command: string): boolean {
  // Substitutions and line continuations can hide anything.
  if (/\$\(|[<>]\(|`|\\\r?\n/.test(command)) return true;
  // Splitting ignores quotes on purpose: a separator inside a quoted
  // argument can only add a prompt, never hide a command.
  return command
    .split(/\s*(?:&&|\|\||;|\||&|\n|[(){}])\s*/)
    .map(bareCommand)
    .some((part) => RISKY_COMMANDS.some((pattern) => pattern.test(part)));
}

/** A file whose edits the AI reviewer must never approve on its own. */
export function isSecretPath(path: string): boolean {
  return SECRET_PATH.test(path.trim());
}
