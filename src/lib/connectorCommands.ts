// connectorCommands.ts
//
// A connector's slash commands (Claude Code's `/compact`, `/context`, the
// project's own `.claude/commands`, skills…): listed by the core
// (`connector_commands`, which asks the program itself, without a request
// to the model), and offered by the composer's `/` picker. A message is a
// command only when the user picked it there and still starts with it:
// typed by hand, `/compact` or `/etc/passwd has a typo` is a message like
// any other. A command goes to the connector as typed and alone
// (`meta.command`, connectors/mod.rs `prompt_for`), without the editor's
// context before it.
import { invoke } from '@tauri-apps/api/core';
import type { ModelProfile } from '../types';
import { connectorKindOf, type ConnectorArgs } from './connectors';

/** One command the connector offers (rovik_protocol::connectors::ConnectorCommand). */
export interface ConnectorCommand {
  name: string;
  description: string;
  /** What it takes after its name, as the program shows it: `<name>`, `[instructions]`. */
  argumentHint?: string | null;
  /** Other names it answers to: `/reset` for `/clear`. */
  aliases?: string[];
  /** Built into the program, rather than the project's or the user's. */
  builtin?: boolean;
}

/** Where commands are listed: the connector, the chat's project folder, its host. */
export interface CommandsKey {
  args: ConnectorArgs;
  /** The project folder on this computer; none for a chat on a cloud host. */
  cwd: string | null;
  /** A cloud host's id; none: this computer. */
  host: string | null;
}

/** The connector a profile runs on, as the core's calls take it; none for a provider's model. */
export function profileConnectorArgs(
  profile: ModelProfile | null | undefined
): ConnectorArgs | null {
  const kind = profile ? connectorKindOf(profile.baseUrl) : undefined;
  if (!profile || !kind) return null;
  return { kind, settings: profile.connector ?? {}, apiKey: profile.apiKey, request: false };
}

/**
 * How long a listing is reused. Listing starts the program (a second or
 * so, no tokens); a command added to the project shows within this.
 */
export const COMMANDS_FRESH_MS = 60_000;

type Fetch = (key: CommandsKey) => Promise<ConnectorCommand[]>;

const fetchCommands: Fetch = (key) =>
  invoke<ConnectorCommand[]>('connector_commands', {
    args: key.args,
    cwd: key.cwd,
    host: key.host,
  });

const cache = new Map<string, { at: number; list: Promise<ConnectorCommand[]> }>();

/** Tests: listings are fetched again. */
export function clearCommandsCache(): void {
  cache.clear();
}

/**
 * The commands at `key`: a listing from the last minute, or a new one. A
 * failed listing isn't kept, so the next ask tries again.
 */
export function connectorCommands(
  key: CommandsKey,
  fetch: Fetch = fetchCommands,
  now = Date.now()
): Promise<ConnectorCommand[]> {
  const id = JSON.stringify(key);
  const kept = cache.get(id);
  if (kept && now - kept.at < COMMANDS_FRESH_MS) return kept.list;
  const list = fetch(key);
  cache.set(id, { at: now, list });
  list.catch(() => {
    if (cache.get(id)?.list === list) cache.delete(id);
  });
  return list;
}

/**
 * Whether a message still runs the command picked for it: it starts with
 * `/name`, alone or before what the command takes. Edited into something
 * else (`/compactly`, `see /compact`), it's a message.
 */
export function startsWithCommand(text: string, name: string): boolean {
  const rest = text.trimStart();
  const head = `/${name}`;
  return rest.startsWith(head) && (rest.length === head.length || /\s/.test(rest[head.length]));
}

/**
 * The picker's query while the composer holds only a command being typed:
 * `com` of `/com` (the text up to the caret). None once a space follows
 * the name, or when the `/` doesn't start the message.
 */
export function commandQuery(beforeCaret: string): { query: string; start: number } | null {
  const m = beforeCaret.match(/^(\s*)\/([^\s/]*)$/);
  return m ? { query: m[2], start: m[1].length } : null;
}

/**
 * The commands for a query: those whose name starts with it first, then
 * those with an alias that does, then those whose name has it elsewhere;
 * the project's and the user's own before the built-in ones in each.
 */
export function matchCommands(query: string, commands: ConnectorCommand[]): ConnectorCommand[] {
  const q = query.toLowerCase();
  const rank = (c: ConnectorCommand): number => {
    const name = c.name.toLowerCase();
    if (name.startsWith(q)) return 0;
    if ((c.aliases ?? []).some((a) => a.toLowerCase().startsWith(q))) return 1;
    if (name.includes(q)) return 2;
    return -1;
  };
  return commands
    .map((c) => ({ c, r: rank(c) }))
    .filter((x) => x.r >= 0)
    .sort(
      (a, b) =>
        a.r - b.r ||
        Number(!!a.c.builtin) - Number(!!b.c.builtin) ||
        a.c.name.localeCompare(b.c.name)
    )
    .map((x) => x.c);
}
