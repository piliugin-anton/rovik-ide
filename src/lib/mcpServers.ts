// mcpServers.ts
//
// Settings → MCP servers (docs/design/mcp-plan.md, Q01): the user's MCP
// servers, whose tools the agent calls with its one `mcp` tool. Kept with
// the AI settings (aiSettings.ts) and sent with the agent's configuration
// (rovik-protocol's mcp module). A secret's value is never kept here: the
// keychain has it, saved and forgotten through `saveMcpSecrets` (Q02).
// While a server is edited or imported, a secret's typed value is in its
// `value`; `savingServer` takes it out before the server is kept.
import { invoke } from '@tauri-apps/api/core';
import type { StoreKind } from './hosts';

/** An env var of a command's server, or a header of a URL's. */
export interface McpValue {
  name: string;
  /** Empty for a kept secret: the keychain has it. */
  value: string;
  secret: boolean;
  /** A secret whose value is saved: what the app did, not what's typed. */
  saved?: boolean;
}

/**
 * An OAuth client the user registered with a server's sign-in, entered on
 * its form (mcp-plan-2.md, Q14): without one, Rovik signs in as itself.
 */
export interface McpOAuthClient {
  clientId: string;
  /** It has a secret, kept in the keychain under `CLIENT_SECRET_KEY`. */
  secret?: boolean;
  /** Its secret's value is saved: what the app did. */
  saved?: boolean;
  /** Its secret as typed, while the form is open: never kept here. */
  value?: string;
}

/** The key an OAuth client's secret is kept under (rovik-protocol's CLIENT_SECRET_KEY). */
export const CLIENT_SECRET_KEY = 'oauth/client-secret';

/**
 * How Rovik reaches a server: a command it starts, or a URL. A URL's `sse`
 * is the old HTTP+SSE transport (2024-11-05), as its setup's entry says
 * (`"type": "sse"`); one without it that turns out to speak only that is
 * found to (mcp-plan-2.md, Q13). Its `oauth` is the client its sign-in
 * uses, when the user entered one (Q14).
 */
export type McpTransport =
  | { kind: 'command'; command: string; args: string[]; env: McpValue[] }
  | { kind: 'url'; url: string; headers: McpValue[]; sse?: boolean; oauth?: McpOAuthClient };

export interface McpServer {
  /** Made when it's added, never shown: its secrets are kept by it. */
  id: string;
  /** What the agent calls it: `github` in `github.create_issue`. */
  name: string;
  enabled: boolean;
  transport: McpTransport;
  /** The tools the user switched off, by name. */
  disabledTools: string[];
}

/** What Test shows of a server (rovik-core's mcp::Tested). */
export interface McpTested {
  /** Its name and version, as it gives them. */
  server: string;
  /** The protocol version Rovik speaks with it. */
  protocol: string;
  tools: { name: string; description: string; readOnly: boolean }[];
}

/** The most characters a server's name has. */
export const MAX_NAME = 32;

/**
 * Whether `name` can name a server: lower-case letters, digits, `-` and
 * `_`, starting with a letter (rovik-protocol's `is_valid_name`).
 */
export function isValidName(name: string): boolean {
  return name.length <= MAX_NAME && /^[a-z][a-z0-9_-]*$/.test(name);
}

/** The key a secret value is kept under (McpTransport::secret_key). */
export function secretKey(transport: McpTransport, name: string): string {
  return transport.kind === 'command' ? `env/${name}` : `header/${name}`;
}

/** A transport's env vars or headers. */
export function valuesOf(transport: McpTransport): McpValue[] {
  return transport.kind === 'command' ? transport.env : transport.headers;
}

/** `transport` with these env vars or headers. */
export function withValues(transport: McpTransport, values: McpValue[]): McpTransport {
  return transport.kind === 'command'
    ? { ...transport, env: values }
    : { ...transport, headers: values };
}

/** A transport of `kind` with nothing filled in. */
export function emptyTransport(kind: McpTransport['kind']): McpTransport {
  return kind === 'command'
    ? { kind, command: '', args: [], env: [] }
    : { kind, url: '', headers: [] };
}

/** A new server's id. */
export function newServerId(): string {
  return crypto.randomUUID();
}

/** A server to fill in: a command, on, with no name yet. */
export function newServer(): McpServer {
  return {
    id: newServerId(),
    name: '',
    enabled: true,
    transport: emptyTransport('command'),
    disabledTools: [],
  };
}

const isString = (v: unknown): v is string => typeof v === 'string';

function normalizeValue(raw: unknown): McpValue | null {
  if (!raw || typeof raw !== 'object') return null;
  const r = raw as Record<string, unknown>;
  if (!isString(r.name) || !r.name.trim()) return null;
  const secret = r.secret === true;
  return {
    name: r.name.trim(),
    // A secret's value is the keychain's, whatever was stored.
    value: secret || !isString(r.value) ? '' : r.value,
    secret,
    ...(secret && r.saved === true ? { saved: true } : {}),
  };
}

function normalizeTransport(raw: unknown): McpTransport | null {
  if (!raw || typeof raw !== 'object') return null;
  const r = raw as Record<string, unknown>;
  const values = (list: unknown) =>
    (Array.isArray(list) ? list : []).flatMap((v) => normalizeValue(v) ?? []);
  if (r.kind === 'command' && isString(r.command)) {
    return {
      kind: 'command',
      command: r.command,
      args: Array.isArray(r.args) ? r.args.filter(isString) : [],
      env: values(r.env),
    };
  }
  if (r.kind === 'url' && isString(r.url)) {
    const client = r.oauth as Record<string, unknown> | undefined;
    const clientId = isString(client?.clientId) ? client.clientId.trim() : '';
    const secret = client?.secret === true;
    return {
      kind: 'url',
      url: r.url,
      headers: values(r.headers),
      ...(r.sse === true ? { sse: true } : {}),
      ...(clientId
        ? {
            oauth: {
              clientId,
              ...(secret ? { secret: true } : {}),
              ...(secret && client?.saved === true ? { saved: true } : {}),
            },
          }
        : {}),
    };
  }
  return null;
}

/** Accepts whatever was stored (or nothing) and keeps the servers it can read. */
export function normalizeMcpServers(raw: unknown): McpServer[] {
  if (!Array.isArray(raw)) return [];
  return raw.flatMap((item) => {
    if (!item || typeof item !== 'object') return [];
    const r = item as Record<string, unknown>;
    const transport = normalizeTransport(r.transport);
    if (!isString(r.id) || !r.id || !isString(r.name) || !transport) return [];
    return [
      {
        id: r.id,
        name: r.name,
        enabled: r.enabled !== false,
        transport,
        disabledTools: Array.isArray(r.disabledTools) ? r.disabledTools.filter(isString) : [],
      },
    ];
  });
}

/** Why `transport` can't be started, or null. */
export function transportProblem(t: McpTransport): string | null {
  if (t.kind === 'command' && !t.command.trim()) return 'Give the command that starts the server.';
  if (t.kind === 'url' && !/^https?:\/\/\S+$/i.test(t.url.trim())) {
    return 'The URL starts with http:// or https://.';
  }
  const names = valuesOf(t).map((v) => v.name.trim());
  if (names.some((n) => !n)) return 'Each value needs a name.';
  const bad = names.find((n) => /[\s=/:]/.test(n));
  if (bad) return `${bad} isn’t a name: it has no spaces, “=”, “/” or “:”.`;
  if (new Set(names.map((n) => n.toLowerCase())).size !== names.length) {
    return `Two ${t.kind === 'command' ? 'variables' : 'headers'} have the same name.`;
  }
  return null;
}

/** Why `server` can't be saved among `all` (the others with it, or not), or null. */
export function serverProblem(server: McpServer, all: McpServer[]): string | null {
  const name = server.name.trim();
  if (!name) return 'Give the server a name.';
  if (!isValidName(name)) {
    return `A name is lower-case letters, digits, - and _, starting with a letter, at most ${MAX_NAME} characters.`;
  }
  if (all.some((s) => s.id !== server.id && s.name === name)) {
    return `Another server is named ${name}.`;
  }
  return transportProblem(server.transport);
}

/** Whether `server` has a value saved in the keychain under `key`. */
export function hasSavedSecret(server: McpServer | undefined, key: string): boolean {
  return (
    !!server &&
    valuesOf(server.transport).some(
      (v) => v.secret && v.saved && secretKey(server.transport, v.name) === key
    )
  );
}

/** What saving a server keeps, and what it changes in the keychain. */
export interface Saving {
  /** The server as it's kept: no secret's value in it. */
  server: McpServer;
  /** By key (`env/NAME`): a value to save, or empty to forget the saved one. */
  secrets: Record<string, string>;
}

/**
 * Saving `draft` over `saved` (none: a new server): each secret's value
 * typed is taken out, to save, and the secret marked saved, as is one
 * typed before under the same key; each secret `saved` had that `draft`
 * hasn't (removed, renamed, made plain, of the other transport) is
 * forgotten. Names, the command and the URL are trimmed, and empty
 * argument lines dropped.
 */
export function savingServer(draft: McpServer, saved: McpServer | undefined): Saving {
  const secrets: Record<string, string> = {};
  const t = draft.transport;
  const values = valuesOf(t).map((v): McpValue => {
    const name = v.name.trim();
    if (!v.secret) return { name, value: v.value, secret: false };
    const key = secretKey(t, name);
    if (v.value) secrets[key] = v.value;
    const kept = !!v.value || hasSavedSecret(saved, key);
    return { name, value: '', secret: true, ...(kept ? { saved: true } : {}) };
  });
  if (saved) {
    const keys = new Set(values.filter((v) => v.secret).map((v) => secretKey(t, v.name)));
    for (const v of valuesOf(saved.transport)) {
      const key = secretKey(saved.transport, v.name);
      if (v.secret && !keys.has(key)) secrets[key] = '';
    }
  }
  // The OAuth client: its secret as typed goes to the keychain, as one
  // saved before stays; a client taken out forgets its secret (Q14).
  const client = t.kind === 'url' ? t.oauth : undefined;
  const clientId = client?.clientId.trim() ?? '';
  const typedSecret = clientId ? (client?.value ?? '') : '';
  const before = saved?.transport.kind === 'url' ? saved.transport.oauth : undefined;
  const secretKept = !!typedSecret || (!!clientId && before?.saved === true);
  if (typedSecret) secrets[CLIENT_SECRET_KEY] = typedSecret;
  if (before?.saved && !secretKept) secrets[CLIENT_SECRET_KEY] = '';
  const oauth: McpOAuthClient | undefined = clientId
    ? { clientId, ...(secretKept ? { secret: true, saved: true } : {}) }
    : undefined;
  let trimmed: McpTransport;
  if (t.kind === 'command') {
    trimmed = { ...t, command: t.command.trim(), args: t.args.filter((a) => a !== '') };
  } else {
    const { oauth: _typed, ...rest } = t;
    trimmed = { ...rest, url: t.url.trim(), ...(oauth ? { oauth } : {}) };
  }
  return {
    server: { ...draft, name: draft.name.trim(), transport: withValues(trimmed, values) },
    secrets,
  };
}

/** What removing `server` forgets in the keychain: each of its secrets, by key. */
export function forgettingServer(server: McpServer): Record<string, string> {
  const t = server.transport;
  return Object.fromEntries([
    ...valuesOf(t)
      .filter((v) => v.secret)
      .map((v) => [secretKey(t, v.name), '']),
    ...(t.kind === 'url' && t.oauth?.secret ? [[CLIENT_SECRET_KEY, '']] : []),
  ]);
}

/**
 * Whether `server` may want a sign-in (Q14): reached at a URL, with no
 * `Authorization` header of its settings, which a key or token would be.
 */
export function signsIn(server: Pick<McpServer, 'transport'>): boolean {
  const t = server.transport;
  return (
    t.kind === 'url' && !t.headers.some((h) => h.name.trim().toLowerCase() === 'authorization')
  );
}

/** The names of `server`'s secrets that have no value saved. */
export function unsavedSecrets(server: McpServer): string[] {
  return valuesOf(server.transport)
    .filter((v) => v.secret && !v.saved)
    .map((v) => v.name);
}

/** How it's reached, in one line: its command line, or its URL (and its old transport). */
export function serverSummary(server: Pick<McpServer, 'transport'>): string {
  const t = server.transport;
  if (t.kind === 'url') return t.sse ? `${t.url} (HTTP+SSE)` : t.url;
  const shown = t.args.map((a) => (!a || /\s|"/.test(a) ? JSON.stringify(a) : a));
  return [t.command, ...shown].join(' ');
}

/** One line for the Settings menu. */
export function mcpServersSummary(servers: McpServer[]): string {
  if (servers.length === 0) return 'None yet: add one for its tools';
  const on = servers.filter((s) => s.enabled);
  const names = on.map((s) => s.name).join(', ');
  if (on.length === servers.length) return names;
  return on.length === 0 ? 'All turned off' : `${names} (${servers.length - on.length} off)`;
}

/**
 * Servers from JSON pasted from another client's setup instructions, as the
 * app reads it (mcp/import.rs): an `mcpServers` object (Claude Code, Claude
 * Desktop, Cursor), VS Code's `servers`, one server's entry by its name, or
 * the entry alone, named apart from `existing`'s. A secret's value is typed
 * in, as a form has it, for `savingServer`. What's wrong with the JSON, in
 * words, when it has no server to add.
 */
export async function importServers(
  text: string,
  existing: McpServer[]
): Promise<{ servers: McpServer[] } | { error: string }> {
  try {
    const taken = existing.map((s) => s.name);
    return { servers: await invoke<McpServer[]>('mcp_import', { text, taken }) };
  } catch (e) {
    return { error: String(e) };
  }
}

/** Where secret values go: the keychain, or the encrypted file in its place. */
export function mcpStore(): Promise<StoreKind> {
  return invoke<StoreKind>('mcp_store');
}

/**
 * Saves the secret values typed for a server, by key (`env/NAME`): a value
 * is saved in the keychain, an empty one forgets what was saved. The
 * server starts again with them at its next use.
 */
export function saveMcpSecrets(server: string, values: Record<string, string>): Promise<void> {
  return invoke<void>('mcp_save_secrets', { server, values });
}

/**
 * Starts `server` as it's set up on the page, with the secret values typed
 * there (by key) over the saved ones, lists its tools, and stops it; in
 * `root`'s folder for a command. With a `host` (its id), that host's
 * daemon does it there, with this computer's saved secrets.
 */
export function testMcpServer(
  server: McpServer,
  values: Record<string, string>,
  root: string | null,
  host: string | null = null
): Promise<McpTested> {
  return invoke<McpTested>('mcp_test', { server, values, root, host });
}
