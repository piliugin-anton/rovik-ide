// codex.ts
//
// Codex as a connector (lib/connectors.ts): its settings, the choices its
// form offers, and its `ConnectorSpec`. Its form's fields are
// CodexFields.tsx; its harness is the core's connectors/codex/.
import type { Provider } from '../types';
import type { ChatCall, Choice, ConnectorSpec } from './connectors';

/** Codex's settings (codex/mod.rs `Settings`, the same fields and defaults). */
export type CodexSettings = {
  /** The program: `codex`, found on the PATH, or a path to it. */
  program: string;
  /** How it signs in: this computer's login, an access token, or an API key. */
  auth: 'login' | 'token' | 'apiKey';
  /**
   * `rovik`: Rovik's permissions and approvals decide each command and
   * patch; the others are Codex's own sandbox and approval presets.
   */
  permissionMode: 'rovik' | 'auto' | 'workspace' | 'readOnly' | 'fullAccess';
  /** `CODEX_HOME`: another login and settings folder; empty: its own. */
  configDir: string;
  /** More arguments for `codex app-server`, as typed on a command line. */
  extraArgs: string;
  /**
   * How chats use it: `own`, Codex with its own tools and its own prompt,
   * under its permission mode; `rovik`, as the model behind Rovik's agent,
   * with Rovik's prompt and Rovik's tools over MCP.
   */
  tools: 'own' | 'rovik';
};

export const CODEX_DEFAULTS: CodexSettings = {
  program: 'codex',
  auth: 'login',
  permissionMode: 'rovik',
  configDir: '',
  extraArgs: '',
  tools: 'own',
};

export const CODEX_USE_MODES: Choice<CodexSettings['tools']>[] = [
  {
    value: 'own',
    label: 'Codex',
    hint: 'Its own tools and prompt; Rovik shows each step it takes.',
    detail:
      'Codex runs its own tools (its shell, its patches…) with its own system prompt and the AGENTS.md it reads, under the permission mode below; Rovik shows each step it took. In Plan mode it plans in its own plan mode, changing nothing. Autopilot and Rovik’s subagents need Rovik’s tools.',
  },
  {
    value: 'rovik',
    label: 'Rovik’s agent on Codex',
    hint: 'Rovik’s prompt and tools, with Codex as the model.',
    detail:
      'Codex is the model behind Rovik’s agent: Rovik’s system prompt, Rovik’s tools over MCP, each call under Permissions and approval in the chat’s copy of the project, with Rovik’s checks, checklist, plans, Autopilot and subagents.',
  },
];

export const CODEX_PERMISSION_MODES: Choice<CodexSettings['permissionMode']>[] = [
  {
    value: 'rovik',
    label: 'Follow Rovik’s permissions',
    hint: 'Rovik decides each command and patch, as for its own agent.',
    detail:
      'Each command and each patch goes to Permissions and approval as the agent’s own would, before it runs: commands by the allowed list, a patch reviewed in the editor as the edits it makes, approvals by you or the AI reviewer. What Rovik allows runs as its own commands do.',
  },
  {
    value: 'auto',
    label: 'Auto',
    hint: 'Works in the project; Codex’s reviewer decides on anything more.',
    detail:
      'Codex works in the project’s folder in its sandbox, and its own reviewer decides when it asks for more (network, files outside it).',
  },
  {
    value: 'workspace',
    label: 'Project folder',
    hint: 'Edits and runs in the project, without network, never asking.',
  },
  { value: 'readOnly', label: 'Read only', hint: 'Reads and answers, changes nothing.' },
  {
    value: 'fullAccess',
    label: 'Everything',
    hint: 'Anything, without a sandbox or asking.',
    detail:
      'Runs every command and patch without a sandbox or asking, like the IDE’s own agent with approval off.',
  },
];

export const CODEX_AUTH_KINDS: Choice<CodexSettings['auth']>[] = [
  {
    value: 'login',
    label: 'This computer’s login',
    hint: 'The ChatGPT account `codex` is logged in to here.',
    detail:
      'The ChatGPT account `codex` is logged in to here: Set up signs it in (`codex login`; on a cloud host, a code to enter on OpenAI’s page). On a cloud host, the host’s login.',
  },
  {
    value: 'token',
    label: 'Access token',
    hint: 'A Codex access token, kept in the keychain.',
    detail:
      'A Codex access token (CODEX_ACCESS_TOKEN): works unattended and on cloud hosts. It’s kept in the keychain.',
  },
  {
    value: 'apiKey',
    label: 'API key',
    hint: 'An OpenAI API key, billed to its platform account.',
    detail:
      'An OpenAI API key, kept in the keychain: each run gives it to Codex, which keeps it in memory and never stores it.',
  },
];

/** The command that installs it, for the user. */
export const CODEX_INSTALL =
  'curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh';

/** A connector's settings with Codex's defaults for what isn't set. */
export function codexSettings(p: Pick<Provider, 'settings'>): CodexSettings {
  const s = (p.settings ?? {}) as Partial<CodexSettings>;
  return {
    ...CODEX_DEFAULTS,
    ...s,
    program: typeof s.program === 'string' && s.program.trim() ? s.program : 'codex',
  };
}

/** Its plan's statuses as Rovik's checklist has them. */
const PLAN_STATUS: Record<string, string> = {
  pending: 'pending',
  inProgress: 'in_progress',
  completed: 'done',
};

/**
 * A unified diff's hunks as the text they replace and the text they make:
 * the lines it keeps and removes, and those it keeps and adds.
 */
function sidesOf(diff: string): { old: string; new: string } {
  const old: string[] = [];
  const made: string[] = [];
  for (const line of diff.split('\n')) {
    if (line.startsWith('@@') || line.startsWith('\\')) continue;
    const text = line.slice(1);
    if (line.startsWith('-')) old.push(text);
    else if (line.startsWith('+')) made.push(text);
    else if (line.startsWith(' ')) {
      old.push(text);
      made.push(text);
    }
  }
  return { old: old.join('\n'), new: made.join('\n') };
}

/**
 * One of Codex's steps as the call of Rovik's tool of the same kind
 * (`ConnectorSpec.chatCall`): a command its shell ran as what Codex read
 * it as doing (a read of one file, a listing, a search), else as `bash`; a
 * file a patch adds as a `write` of it, one it changes as a partial
 * write; its web search as `web_search`; its plan as `todo`. The paths are
 * the project's (the core makes them relative to the folder it ran in).
 * Its other steps have no like.
 */
export function codexChatCall(name: string, input: Record<string, unknown>): ChatCall | undefined {
  const text = (key: string) => (typeof input[key] === 'string' ? input[key] : undefined);
  switch (name) {
    case 'shell': {
      const actions = Array.isArray(input.actions)
        ? (input.actions as Record<string, unknown>[])
        : [];
      const only = actions.length === 1 ? actions[0] : undefined;
      const field = (key: string) =>
        typeof only?.[key] === 'string' ? (only[key] as string) : undefined;
      switch (only?.type) {
        case 'read':
          return { name: 'read', arguments: { path: field('path') } };
        case 'listFiles':
          return { name: 'list_dir', arguments: { path: field('path') ?? '.' } };
        case 'search':
          return { name: 'search_code', arguments: { query: field('query'), path: field('path') } };
        default:
          return { name: 'bash', arguments: { command: text('command') } };
      }
    }
    case 'edit': {
      const path = text('movePath') ?? text('path');
      const diff = text('diff') ?? '';
      if (input.kind === 'add') return { name: 'write', arguments: { path, content: diff } };
      if (input.kind !== 'update') return undefined;
      const sides = sidesOf(diff);
      return { name: 'write', arguments: { path, old_text: sides.old, content: sides.new } };
    }
    case 'web_search':
      return { name: 'web_search', arguments: { query: text('query') } };
    case 'view_image':
      return { name: 'read', arguments: { path: text('path') } };
    case 'update_plan': {
      const plan = Array.isArray(input.plan) ? (input.plan as Record<string, unknown>[]) : [];
      const items = plan.map((p) => ({
        title: typeof p?.step === 'string' ? p.step : '',
        status: PLAN_STATUS[String(p?.status)] ?? 'pending',
      }));
      return { name: 'todo', arguments: { items } };
    }
    default:
      return undefined;
  }
}

/** Codex, as the app knows it. */
export const CODEX: ConnectorSpec = {
  kind: 'codex',
  title: 'Codex',
  about:
    'OpenAI’s coding agent, run in the project folder with its own tools. Chats that pick one of its models hand it the task; its steps and tokens show with the reply.',
  install: CODEX_INSTALL,
  settings: (raw) => codexSettings({ settings: raw }),
  usesRovikTools: (raw) => codexSettings({ settings: raw }).tools === 'rovik',
  summary: (raw) => {
    const s = codexSettings({ settings: raw });
    const mode = CODEX_PERMISSION_MODES.find((m) => m.value === s.permissionMode)?.label;
    return `${s.program} · ${s.tools === 'rovik' ? 'Rovik’s agent' : (mode ?? s.permissionMode)}`;
  },
  planMode: true,
  modelsHint: 'Test the connection to list the models your account offers, or add one by ID below.',
  chatCall: codexChatCall,
};
