// mcpSignIn.ts
//
// Signing in to MCP servers that ask for it (docs/design/mcp-plan-2.md,
// Q14): Rovik runs the OAuth sign-in in the browser, for this computer and
// for each host apart, since refresh tokens rotate and two places sharing
// one would sign each other out. A host's sign-in is made here and kept,
// and refreshed, by its daemon. Settings → MCP servers shows where each
// server is signed in, with Sign in and Sign out; a place whose sign-in the
// server refused lists it signed out, and one it asked for more
// permissions (a 403) wants a new sign-in, which asks for them.
import { invoke } from '@tauri-apps/api/core';
import { createMemo, createResource, createSignal } from 'solid-js';
import type { HostRow } from './hosts';
import type { McpServer } from './mcpServers';

/** A place a server is signed in: this computer (null), or a host by its id. */
export type Place = string | null;

/** A server's sign-in at a place, and the scopes the server asked for that it lacks. */
export interface SignInAt {
  place: Place;
  wants?: string;
}

/** Where the servers are signed in: by server id. */
export type SignedIn = Record<string, SignInAt[]>;

/** A server signed in at a place, as the app answers. */
interface Answered {
  server: string;
  wants?: string;
}

/** The hosts a sign-in can be made for: connected, their daemon keeping sign-ins. */
export function signInHosts(hosts: HostRow[]): HostRow[] {
  return hosts.filter((h) => h.state === 'ready' && h.serves.includes('mcp-sign-in'));
}

/** Where each of `servers` (ids) is signed in: here, and on each of `hosts` that answers. */
export async function signedIn(servers: string[], hosts: string[]): Promise<SignedIn> {
  const answer = await invoke<{ here: Answered[]; hosts: Record<string, Answered[]> }>(
    'mcp_signed_in',
    { servers, hosts }
  );
  const places: SignedIn = {};
  const add = (place: Place, { server, wants }: Answered) =>
    (places[server] ??= []).push({ place, ...(wants && { wants }) });
  for (const signed of answer.here) add(null, signed);
  for (const host of hosts) for (const signed of answer.hosts[host] ?? []) add(host, signed);
  return places;
}

/** Signs in to the user's `server` for `place`, in the browser. */
export function signInServer(server: McpServer, place: Place): Promise<void> {
  return invoke('mcp_sign_in', { server, values: {}, host: place });
}

/** Signs in to a project's own server (by its hash) for `place`, as its files set it up. */
export function signInProjectServer(
  root: string,
  projectId: string,
  hash: string,
  place: Place
): Promise<void> {
  return invoke('mcp_project_sign_in', { root, project: projectId, hash, host: place });
}

/** Signs `server` (its id) out of every place: here, and on each of `hosts`. */
export function signOut(server: string, hosts: string[]): Promise<void> {
  return invoke('mcp_sign_out', { server, hosts });
}

/** The id a project's own server is signed in under: `project/<project>/<hash>`. */
export function projectServerId(projectId: string, hash: string): string {
  return `project/${projectId}/${hash}`;
}

/** A page's sign-ins: where its servers are signed in, and signing them in and out. */
export interface SignIns {
  /** The places server `id` is signed in. */
  at(id: string): Place[];
  /** The places where the server asked for more permissions than its sign-in has. */
  wanting(id: string): Place[];
  /**
   * The places it wants a sign-in: this computer, then each host that keeps
   * sign-ins, where it has none or one that wants more.
   */
  missing(id: string): Place[];
  /** The places as a sentence: "This computer, WSL, build-server". */
  said(places: Place[]): string;
  /** What the sign-in under way says, while one is. */
  progress(): string | null;
  /**
   * Signs server `id` (called `label`) in where it wants a sign-in, one
   * browser sign-in after another by `one`; stops at the first that
   * fails, and says why.
   */
  signIn(id: string, label: string, one: (place: Place) => Promise<void>): Promise<string | null>;
  /** Signs server `id` out of every place; what failed, if something did. */
  signOut(id: string): Promise<string | null>;
}

/** Whether two lists of ids are the same, in order. */
const sameIds = (a: string[] | undefined, b: string[] | undefined) =>
  a === b || (!!a && !!b && a.length === b.length && a.every((id, i) => id === b[i]));

/**
 * The sign-ins of `servers()` (ids), here and on each of `hosts()` that
 * keeps sign-ins: read once the hosts are (`hosts()` is undefined until
 * then), and again only when the servers or those hosts change.
 */
export function createSignIns(
  servers: () => string[],
  hosts: () => HostRow[] | undefined
): SignIns {
  const ids = createMemo(servers, [], { equals: sameIds });
  const on = createMemo(
    () => {
      const all = hosts();
      return all && signInHosts(all).map((h) => h.id);
    },
    undefined,
    { equals: sameIds }
  );
  const name = (place: Place) =>
    place === null ? 'This computer' : (hosts()?.find((h) => h.id === place)?.name ?? place);
  const [signed, { refetch }] = createResource(
    () => {
      const places = on();
      return places && { ids: ids(), on: places };
    },
    ({ ids, on }): Promise<SignedIn> =>
      ids.length === 0
        ? Promise.resolve({})
        : signedIn(ids, on).catch((e): SignedIn => {
            console.error('Where the MCP servers are signed in wasn’t read:', e);
            return {};
          })
  );
  const [progress, setProgress] = createSignal<string | null>(null);
  const of = (id: string) => signed()?.[id] ?? [];
  const at = (id: string) => of(id).map((s) => s.place);
  const wanting = (id: string) =>
    of(id)
      .filter((s) => s.wants)
      .map((s) => s.place);
  const missing = (id: string): Place[] =>
    [null, ...(on() ?? [])].filter((p) => !at(id).includes(p) || wanting(id).includes(p));
  return {
    at,
    wanting,
    missing,
    said: (list) => list.map(name).join(', '),
    progress,
    async signIn(id, label, one) {
      let failed: string | null = null;
      for (const place of missing(id)) {
        setProgress(`Signing in to ${label} for ${name(place)}: finish in your browser…`);
        try {
          await one(place);
        } catch (e) {
          failed = `${label} wasn’t signed in for ${name(place)}: ${String(e)}`;
          break;
        }
      }
      setProgress(null);
      void refetch();
      return failed;
    },
    async signOut(id) {
      let failed: string | null = null;
      try {
        await signOut(id, on() ?? []);
      } catch (e) {
        failed = String(e);
      }
      void refetch();
      return failed;
    },
  };
}
