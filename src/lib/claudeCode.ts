// claudeCode.ts
//
// Claude Code as a connector (lib/connectors.ts): its settings, the
// choices its form offers, and its `ConnectorSpec`. Its form's fields are
// ClaudeCodeFields.tsx; its harness is the core's connectors/claude_code.rs.
import type { Provider } from '../types';
import type { ChatCall, Choice, ConnectorSpec } from './connectors';

/** Claude Code's settings (claude_code.rs `Settings`, the same fields and defaults). */
export type ClaudeCodeSettings = {
  /** The program: `claude`, found on the PATH, or a path to it. */
  program: string;
  /** How it signs in: this computer's login, a `claude setup-token` token, or an API key. */
  auth: 'login' | 'token' | 'apiKey';
  /**
   * `rovik`: Rovik's permissions and approvals decide each call Claude Code
   * would ask about (claude_code.rs `ROVIK_PERMISSIONS`); the others are
   * Claude Code's own modes.
   */
  permissionMode:
    'rovik' | 'acceptEdits' | 'bypassPermissions' | 'default' | 'plan' | 'dontAsk' | 'auto';
  /** `--allowedTools` rules, one per line. */
  allowedTools: string;
  disallowedTools: string;
  /** The most agentic turns one message may take; empty: no limit. */
  maxTurns?: number;
  /** The most one message may cost (estimated, USD); empty: no limit. */
  maxBudgetUsd?: number;
  /** `CLAUDE_CONFIG_DIR`: another login and settings folder; empty: its own. */
  configDir: string;
  /** More arguments, as typed on a command line. */
  extraArgs: string;
  /** `--bare`: none of the user's hooks, skills, plugins or CLAUDE.md. */
  bare: boolean;
  /**
   * How chats use it: `own`, Claude Code with its own tools and its own
   * prompt, under its permission mode; `rovik`, as the model behind
   * Rovik's agent, with Rovik's prompt and Rovik's tools over MCP.
   */
  tools: 'own' | 'rovik';
};

export const CLAUDE_CODE_DEFAULTS: ClaudeCodeSettings = {
  program: 'claude',
  auth: 'login',
  permissionMode: 'rovik',
  allowedTools: '',
  disallowedTools: '',
  configDir: '',
  extraArgs: '',
  bare: false,
  tools: 'own',
};

export const USE_MODES: Choice<ClaudeCodeSettings['tools']>[] = [
  {
    value: 'own',
    label: 'Claude Code',
    hint: 'Its own tools and prompt; Rovik shows each step it takes.',
    detail:
      'Claude Code runs its own tools (Read, Edit, Bash…) with its own system prompt and the CLAUDE.md it reads, under the permission mode below; Rovik shows each step it took. In Plan mode it plans in its own plan mode. Autopilot and Rovik’s subagents need Rovik’s tools.',
  },
  {
    value: 'rovik',
    label: 'Rovik’s agent on Claude Code',
    hint: 'Rovik’s prompt and tools, with Claude Code as the model.',
    detail:
      'Claude Code is the model behind Rovik’s agent: Rovik’s system prompt, Rovik’s tools over MCP, each call under Permissions and approval in the chat’s copy of the project, with Rovik’s checks, checklist, plans, Autopilot and subagents.',
  },
];

export const PERMISSION_MODES: Choice<ClaudeCodeSettings['permissionMode']>[] = [
  {
    value: 'rovik',
    label: 'Follow Rovik’s permissions',
    hint: 'Rovik decides each call it would ask about, as for its own agent.',
    detail:
      'Each call Claude Code would ask about goes to Permissions and approval, as the agent’s own would: commands by the allowed list, edits reviewed in the editor, approvals by you or the AI reviewer. Reads in the project and its read-only commands run without asking.',
  },
  {
    value: 'acceptEdits',
    label: 'Accept edits',
    hint: 'Edits and common file commands run; other commands need a rule.',
    detail:
      'Edits files and runs common file commands without asking; other commands need an allowed-tools rule. Nothing waits for a prompt: what the mode and the rules don’t allow is denied.',
  },
  {
    value: 'bypassPermissions',
    label: 'Everything',
    hint: 'Every tool runs without asking.',
    detail: 'Runs every tool without asking, like the IDE’s own agent with approval off.',
  },
  {
    value: 'auto',
    label: 'Auto',
    hint: 'A classifier reviews each action instead of a person.',
  },
  {
    value: 'default',
    label: 'Manual',
    hint: 'Only the allowed rules run; anything else is denied.',
    detail:
      'Only the allowed-tools rules run; anything else is denied, since nobody is there to answer.',
  },
  { value: 'plan', label: 'Plan', hint: 'Reads and plans, changes nothing.' },
];

export const AUTH_KINDS: Choice<ClaudeCodeSettings['auth']>[] = [
  {
    value: 'login',
    label: 'This computer’s login',
    hint: 'The account `claude` is logged in to here.',
    detail:
      'The account `claude` is logged in to here: Set up signs it in, or run `claude` once. On a cloud host, the host’s login.',
  },
  {
    value: 'token',
    label: 'Token',
    hint: 'A long-lived token from `claude setup-token`, kept in the keychain.',
    detail:
      'A long-lived token from `claude setup-token`: works unattended and on cloud hosts. It’s kept in the keychain.',
  },
  {
    value: 'apiKey',
    label: 'API key',
    hint: 'An Anthropic API key, billed to its Console account.',
  },
];

/**
 * The model a Claude Code chat's title is asked of: Claude Code's small,
 * fast one, as Claude Code titles its own sessions. A title needs no more,
 * and the chat's model may be Opus or Fable.
 */
export const CLAUDE_CODE_TITLE_MODEL = 'haiku';

/** The command that installs it, for the user. */
export const CLAUDE_CODE_INSTALL = 'curl -fsSL https://claude.ai/install.sh | bash';

/**
 * A connector's settings with Claude Code's defaults for what isn't set.
 * What it no longer has is left out: the system prompt is the tools'
 * (its own with its own, Rovik's with Rovik's).
 */
export function claudeCodeSettings(p: Pick<Provider, 'settings'>): ClaudeCodeSettings {
  const { systemPrompt: _gone, ...s } = (p.settings ?? {}) as Partial<ClaudeCodeSettings> & {
    systemPrompt?: unknown;
  };
  return {
    ...CLAUDE_CODE_DEFAULTS,
    ...s,
    program: typeof s.program === 'string' && s.program.trim() ? s.program : 'claude',
  };
}

/** Its checklist's statuses as Rovik's checklist has them. */
const TODO_STATUS: Record<string, string> = {
  pending: 'pending',
  in_progress: 'in_progress',
  completed: 'done',
};

/**
 * One of Claude Code's tool calls as the call of Rovik's tool of the same
 * kind (`ConnectorSpec.chatCall`): its file tools as `read` and `write`
 * (an edit as a partial write), its shell as `bash`, its searches as
 * `search_code`, its web tools as `browse_url` and `web_search`, its
 * checklist as `todo`. The paths are the project's (the core makes them
 * relative to the folder it ran in). Its other tools have no like.
 */
export function claudeCodeChatCall(
  name: string,
  input: Record<string, unknown>
): ChatCall | undefined {
  const text = (key: string) => (typeof input[key] === 'string' ? input[key] : undefined);
  const count = (key: string) => (typeof input[key] === 'number' ? input[key] : undefined);
  switch (name) {
    case 'Read': {
      const start = count('offset');
      const lines = count('limit');
      const range = start
        ? { start_line: start, ...(lines ? { end_line: start + lines - 1 } : {}) }
        : {};
      return { name: 'read', arguments: { path: text('file_path'), ...range } };
    }
    case 'Write':
      return { name: 'write', arguments: { path: text('file_path'), content: text('content') } };
    case 'Edit':
      return {
        name: 'write',
        arguments: {
          path: text('file_path'),
          old_text: text('old_string'),
          content: text('new_string'),
        },
      };
    case 'Bash':
      return { name: 'bash', arguments: { command: text('command') } };
    case 'Grep':
    case 'Glob':
      return { name: 'search_code', arguments: { query: text('pattern'), path: text('path') } };
    case 'LS':
      return { name: 'list_dir', arguments: { path: text('path') } };
    case 'WebFetch':
      return { name: 'browse_url', arguments: { url: text('url') } };
    case 'WebSearch':
      return { name: 'web_search', arguments: { query: text('query') } };
    case 'TodoWrite': {
      const todos = Array.isArray(input.todos) ? (input.todos as Record<string, unknown>[]) : [];
      const items = todos.map((t) => ({
        title: typeof t?.content === 'string' ? t.content : '',
        status: TODO_STATUS[String(t?.status)] ?? 'pending',
      }));
      return { name: 'todo', arguments: { items } };
    }
    default:
      return undefined;
  }
}

/** Claude Code, as the app knows it. */
export const CLAUDE_CODE: ConnectorSpec = {
  kind: 'claude-code',
  title: 'Claude Code',
  about:
    'Anthropic’s coding agent, run headless in the project folder with its own tools. Chats that pick one of its models hand it the task; its steps, tokens and cost show with the reply.',
  install: CLAUDE_CODE_INSTALL,
  settings: (raw) => claudeCodeSettings({ settings: raw }),
  usesRovikTools: (raw) => claudeCodeSettings({ settings: raw }).tools === 'rovik',
  summary: (raw) => {
    const s = claudeCodeSettings({ settings: raw });
    const mode = PERMISSION_MODES.find((m) => m.value === s.permissionMode)?.label;
    return `${s.program} · ${s.tools === 'rovik' ? 'Rovik’s agent' : (mode ?? s.permissionMode)}`;
  },
  titleModel: CLAUDE_CODE_TITLE_MODEL,
  planMode: true,
  modelsHint:
    'Test the connection to list its models (aliases such as sonnet and opus, and full names), or add one by ID below.',
  chatCall: claudeCodeChatCall,
};
