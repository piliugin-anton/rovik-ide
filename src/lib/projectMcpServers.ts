// projectMcpServers.ts
//
// The MCP servers a project's own files set up: .mcp.json (Claude Code),
// .cursor/mcp.json (Cursor), .vscode/mcp.json (VS Code); mcp-plan-2.md
// Q12. A run reads them, and those the user hasn't allowed, or not as
// they are now, wait: the run goes without them and tells the chat
// (`mcpServersAwaiting`), which asks above the input (ProjectMcpCard).
// Allowing keeps each server's definition, hashed, in the project's
// settings, which go with the project to its hosts, and the values the
// user gives its variables in the keychain, by the project; a server
// whose definition changes asks again. Settings → MCP servers lists each
// project's allowed ones, with Revoke, and Sign in for one that may ask
// for it (Q14).
import { invoke } from '@tauri-apps/api/core';
import { createStore, produce } from 'solid-js/store';
import { chatApi, type ProjectSummary } from './chatApi';
import { signsIn, type McpTransport } from './mcpServers';
import { projectServerId, signOut } from './mcpSignIn';
import { updateProjectSettings } from './projectProfile';

/** A variable a project's server names: `${GITHUB_TOKEN}`, `${input:db}`. */
export interface ProjectVariable {
  /** `GITHUB_TOKEN`, or `input:db` for VS Code's input. */
  name: string;
  /** What the file says of it (an input's description). */
  description?: string;
  /** What it is with no value given: `${VAR:-default}`'s, an input's. */
  default?: string;
  password?: boolean;
}

/** A server a project's file sets up, its values as the file writes them. */
export interface ProjectServer {
  name: string;
  /** The file, from the project's root: `.mcp.json`. */
  file: string;
  /** Its definition's hash: what an approval keeps. */
  hash: string;
  transport: McpTransport;
  variables?: ProjectVariable[];
}

/** What a run of a chat found waiting in its project (`mcpServersAwaiting`). */
export interface AwaitingServers {
  projectId: string;
  servers: ProjectServer[];
  /** What the files have that isn't used, and why. */
  leftOut?: string[];
}

/** A project server the user allowed, as the project's settings keep it. */
export interface McpApproval {
  hash: string;
  name: string;
  file: string;
  /** The variables it takes, whose values a host's starts bring. */
  variables?: string[];
  /** Whether it may ask the user to sign in (Q14): reached at a URL, with no Authorization header. */
  signsIn?: boolean;
}

const [state, setState] = createStore<{ awaiting: Record<string, AwaitingServers> }>({
  awaiting: {},
});

/**
 * What "Not now" put off, until the app is closed: a project's server by
 * its hash, or a note of what isn't used.
 */
const putOff = new Set<string>();
const offKey = (projectId: string, what: string) => `${projectId}\n${what}`;

/** A run of `dialogId` found these waiting: the chat asks, but for what was put off. */
export function noteMcpServersAwaiting(dialogId: string, awaiting: AwaitingServers) {
  const { projectId } = awaiting;
  const servers = awaiting.servers.filter((s) => !putOff.has(offKey(projectId, s.hash)));
  const leftOut = (awaiting.leftOut ?? []).filter((l) => !putOff.has(offKey(projectId, l)));
  setState(
    produce((s) => {
      if (servers.length === 0 && leftOut.length === 0) delete s.awaiting[dialogId];
      else s.awaiting[dialogId] = { projectId, servers, leftOut };
    })
  );
}

/** What waits for the user in the chat's project, as its last run found it. */
export function awaitingServersIn(dialogId: string): AwaitingServers | undefined {
  return state.awaiting[dialogId];
}

/**
 * The chat's card is answered: its servers allowed, or put off by "Not
 * now" (`putOffServers`); its notes of what isn't used were seen either
 * way. No chat of the project asks of them again.
 */
export function settleAwaitingServers(dialogId: string, putOffServers: boolean) {
  const awaiting = state.awaiting[dialogId];
  if (!awaiting) return;
  const { projectId } = awaiting;
  const hashes = new Set(awaiting.servers.map((s) => s.hash));
  const notes = new Set(awaiting.leftOut ?? []);
  if (putOffServers) for (const hash of hashes) putOff.add(offKey(projectId, hash));
  for (const note of notes) putOff.add(offKey(projectId, note));
  setState(
    produce((s) => {
      for (const [id, other] of Object.entries(s.awaiting)) {
        if (other.projectId !== projectId) continue;
        other.servers = other.servers.filter((server) => !hashes.has(server.hash));
        other.leftOut = (other.leftOut ?? []).filter((note) => !notes.has(note));
        if (other.servers.length === 0 && other.leftOut.length === 0) delete s.awaiting[id];
      }
    })
  );
}

/** The variables `servers` take, each once, as they come. */
export function variablesOf(servers: ProjectServer[]): ProjectVariable[] {
  const seen = new Map<string, ProjectVariable>();
  for (const v of servers.flatMap((s) => s.variables ?? []))
    if (!seen.has(v.name)) seen.set(v.name, v);
  return [...seen.values()];
}

/** The approvals a project's settings keep. */
export function approvalsIn(settings: unknown): McpApproval[] {
  const list = (settings as { mcpApprovals?: unknown } | null)?.mcpApprovals;
  if (!Array.isArray(list)) return [];
  return list.filter(
    (a): a is McpApproval =>
      !!a && typeof a.hash === 'string' && typeof a.name === 'string' && typeof a.file === 'string'
  );
}

/** `approvals` with `servers` among them, each once by its file and name: a newer one replaces it. */
export function withApproved(approvals: McpApproval[], servers: ProjectServer[]): McpApproval[] {
  const now = servers.map((s): McpApproval => ({
    hash: s.hash,
    name: s.name,
    file: s.file,
    variables: (s.variables ?? []).map((v) => v.name),
    ...(signsIn(s) && { signsIn: true }),
  }));
  const same = (a: McpApproval, b: McpApproval) => a.file === b.file && a.name === b.name;
  return [...approvals.filter((a) => !now.some((n) => same(a, n))), ...now];
}

/** What the fields of a project's variables start with. */
export interface Prefill {
  /** This computer's environment's values, by variable. */
  values: Record<string, string>;
  /** The variables the keychain has a value for already. */
  saved: string[];
}

/** The fields of `projectId`'s variables `names`: the environment's values, and which are saved. */
export function prefillValues(projectId: string, names: string[]): Promise<Prefill> {
  return invoke<Prefill>('mcp_project_prefill', { project: projectId, names });
}

/**
 * Allows the project's `servers` (the project at `root`, `projectId`): the
 * values typed for their variables go to the keychain, then the servers
 * to the project's settings. Their next run has them. One that replaces
 * the approval of its entry as it was has that one's sign-in forgotten
 * (Q14): it was made for the server as it was.
 */
export async function allowServers(
  root: string,
  projectId: string,
  servers: ProjectServer[],
  values: Record<string, string>
): Promise<void> {
  const given = Object.fromEntries(Object.entries(values).filter(([, v]) => v !== ''));
  if (Object.keys(given).length > 0) {
    await invoke('mcp_save_project_values', { project: projectId, values: given });
  }
  let replaced: McpApproval[] = [];
  await updateProjectSettings(root, (saved) => {
    const before = approvalsIn(saved);
    const after = withApproved(before, servers);
    replaced = before.filter((a) => a.signsIn && !after.some((b) => b.hash === a.hash));
    return { mcpApprovals: after };
  });
  for (const old of replaced) {
    await signOut(projectServerId(projectId, old.hash)).catch((e) =>
      console.error(`${old.name}'s sign-in from before its entry changed wasn't forgotten:`, e)
    );
  }
}

/** A project's allowed servers, for Settings → MCP servers. */
export interface ProjectApprovals {
  project: ProjectSummary;
  approvals: McpApproval[];
}

/** The projects that have servers allowed, each with them, as they're listed. */
export async function projectApprovals(): Promise<ProjectApprovals[]> {
  const projects = await chatApi.listProjects();
  const all = await Promise.all(
    projects.map(async (project) => {
      const settings = await invoke<unknown>('load_project_settings', { root: project.path });
      return { project, approvals: approvalsIn(settings) };
    })
  );
  return all.filter((p) => p.approvals.length > 0);
}

/**
 * Revokes the approval of the server `hash` in the project at `root`
 * (`projectId`), forgetting the values of its variables that no server
 * still allowed takes. The project's servers that run stop: the others
 * start again at their next use, this one no more.
 */
export async function revokeServer(root: string, projectId: string, hash: string): Promise<void> {
  let forgotten: string[] = [];
  await updateProjectSettings(root, (saved) => {
    const approvals = approvalsIn(saved);
    const kept = approvals.filter((a) => a.hash !== hash);
    const used = new Set(kept.flatMap((a) => a.variables ?? []));
    const gone = approvals.find((a) => a.hash === hash)?.variables ?? [];
    forgotten = gone.filter((v) => !used.has(v));
    return { mcpApprovals: kept.length > 0 ? kept : undefined };
  });
  const values = Object.fromEntries(forgotten.map((v) => [v, '']));
  await invoke('mcp_save_project_values', { project: projectId, values });
}
