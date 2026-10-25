// projectProfile.ts
//
// What the agent is told about the open project, and which commands check
// its changes. Detection is in the backend
// (src-tauri/crates/rovik-core/src/project_profile.rs); a detected check is
// off until the user turns it on. The user turns checks on and corrects
// them per project (ProjectChecksSettings), which is stored separately and
// wins once saved. The backend builds the agent's prompt section from them
// (src-tauri/crates/rovik-core/src/agent_run/project.rs).
import { invoke } from '@tauri-apps/api/core';
import { plural } from './textUtils';
import type { RestrictedFile } from './restrictedFiles';

export type CheckKind = 'typecheck' | 'lint' | 'test' | 'build';

export const CHECK_KINDS: CheckKind[] = ['typecheck', 'lint', 'test', 'build'];

export const CHECK_LABEL: Record<CheckKind, string> = {
  typecheck: 'Type check',
  lint: 'Lint',
  test: 'Tests',
  build: 'Build',
};

/** A check found in the project's files: off until the user turns it on. */
export interface DetectedCheck {
  kind: CheckKind;
  command: string;
  /** Where it was found, e.g. "package.json scripts.test". */
  source: string;
}

export interface WorkspaceMember {
  path: string;
  name?: string;
  kind: 'node' | 'cargo' | string;
  scripts: string[];
}

export interface ProjectProfile {
  stack: string[];
  checks: DetectedCheck[];
  instructions: { file: string; text: string; truncated: boolean } | null;
  /** Workspace member packages (capped); empty/missing when not a workspace. */
  workspaces?: WorkspaceMember[];
  /** Node package manager owning workspace commands, when there's a package.json. */
  packageManager?: 'npm' | 'pnpm' | 'yarn' | 'yarn-berry' | 'bun' | string;
}

export interface ProjectCheck {
  kind: CheckKind;
  command: string;
  enabled: boolean;
  /** Where a detected check came from; absent once the user edits the command. */
  source?: string;
}

/**
 * Per-project settings. `checks` replaces the detected list once the user
 * edits it; `restrictedFiles` is Settings → Restricted files' list
 * (restrictedFiles.ts), which the backend reads.
 */
export interface ProjectSettings {
  checks?: ProjectCheck[];
  restrictedFiles?: RestrictedFile[];
}

export function detectProject(root: string): Promise<ProjectProfile> {
  return invoke<ProjectProfile>('project_profile', { root });
}

export async function loadProjectSettings(root: string): Promise<ProjectSettings> {
  try {
    const raw = await invoke<unknown>('load_project_settings', { root });
    return normalizeSettings(raw);
  } catch (err) {
    console.error('Failed to load project settings:', err);
    return {};
  }
}

/** The last save of project settings: the next waits for it. */
let saving: Promise<void> = Promise.resolve();

/** Some of a project's settings: a key given `undefined` is removed. */
export type ProjectSettingsPatch = {
  [K in keyof ProjectSettings]: ProjectSettings[K] | undefined;
};

/**
 * Saves some of a project's settings, keeping the rest: the pages that set
 * them each save their own. A key given `undefined` is removed (back to
 * what's detected), and settings left empty are removed altogether. Saves
 * are made one at a time, in order, so a later one is never overwritten.
 * `patch` may be made from the settings as saved (`saved => patch`): a
 * change to a list then keeps the change saved just before it.
 */
export function updateProjectSettings(
  root: string,
  patch: ProjectSettingsPatch | ((saved: Record<string, unknown>) => ProjectSettingsPatch)
): Promise<void> {
  const next = saving.then(async () => {
    const saved = await invoke<unknown>('load_project_settings', { root });
    const settings: Record<string, unknown> =
      saved && typeof saved === 'object' && !Array.isArray(saved)
        ? { ...(saved as Record<string, unknown>) }
        : {};
    const changes = typeof patch === 'function' ? patch({ ...settings }) : patch;
    for (const [key, value] of Object.entries(changes)) {
      if (value === undefined) delete settings[key];
      else settings[key] = value;
    }
    await invoke('save_project_settings', {
      root,
      settings: Object.keys(settings).length > 0 ? settings : null,
    });
  });
  saving = next.catch(() => undefined);
  return next;
}

function normalizeSettings(raw: unknown): ProjectSettings {
  if (!raw || typeof raw !== 'object') return {};
  const checks = (raw as { checks?: unknown }).checks;
  if (!Array.isArray(checks)) return {};
  return {
    checks: checks
      .filter(
        (c): c is ProjectCheck =>
          !!c &&
          typeof c === 'object' &&
          CHECK_KINDS.includes((c as ProjectCheck).kind) &&
          typeof (c as ProjectCheck).command === 'string'
      )
      .map((c) => ({
        kind: c.kind,
        command: c.command,
        enabled: c.enabled !== false,
        ...(typeof c.source === 'string' && c.source ? { source: c.source } : {}),
      })),
  };
}

/**
 * The checks in effect: the user's list when they saved one, else what's
 * detected, each off. First open of a project has no saved settings, so
 * detection — including workspace-wide commands — pre-fills the list, and
 * nothing runs until the user turns a check on; from the first edit on,
 * the user's list wins. The backend reads them the same way
 * (agent_run/project.rs, `effective_checks`).
 */
export function effectiveChecks(
  profile: ProjectProfile | null,
  settings: ProjectSettings
): ProjectCheck[] {
  if (settings.checks) return settings.checks;
  return (profile?.checks ?? []).map((c) => ({
    kind: c.kind,
    command: c.command,
    enabled: false,
    source: c.source,
  }));
}

/** Of `checks`, those that run after the agent's changes: on, with a command. */
export function checksThatRun(checks: ProjectCheck[]): ProjectCheck[] {
  return checks.filter((c) => c.enabled && c.command.trim() !== '');
}

/** How many of the project in `root`'s checks run after the agent's changes. */
export async function countChecksThatRun(root: string): Promise<number> {
  const [detected, settings] = await Promise.all([detectProject(root), loadProjectSettings(root)]);
  return checksThatRun(effectiveChecks(detected, settings)).length;
}

/**
 * The Settings menu's line for Project checks: whether checking is on
 * (Agent runs), and how many of the project's checks run (`running`;
 * undefined or null while that isn't known).
 */
export function projectChecksSummary(
  verifyChanges: boolean,
  running: number | null | undefined
): string {
  if (!verifyChanges) return 'Not run: checking is off in Agent runs';
  if (running === 0) return 'Nothing to run: no check is turned on';
  if (typeof running === 'number') {
    const verb = running === 1 ? 'runs' : 'run';
    return `${plural(running, 'check')} ${verb} when the agent finishes a change`;
  }
  return 'Run when the agent finishes a change';
}

/** One workspace-wide command running `script` in every package that has it. */
export function workspaceScriptCommand(
  packageManager: ProjectProfile['packageManager'],
  script: string
): string {
  switch (packageManager) {
    case 'pnpm':
      return `pnpm -r --if-present run ${script}`;
    case 'yarn-berry':
      return `yarn workspaces foreach -A run ${script}`;
    case 'yarn':
      return `yarn workspaces run ${script}`;
    case 'bun':
      return `bun run --filter '*' ${script}`;
    default:
      return script === 'test'
        ? 'npm test --workspaces --if-present'
        : `npm run ${script} --workspaces --if-present`;
  }
}

/** Scoped command running `script` in one workspace member only. */
export function workspaceMemberCommand(
  packageManager: ProjectProfile['packageManager'],
  member: WorkspaceMember,
  script: string
): { command: string; kind: CheckKind; source: string } {
  if (member.kind === 'cargo') {
    // Here `script` is a check kind (picker offers kinds for cargo members).
    const kind = (['typecheck', 'lint', 'test', 'build'] as CheckKind[]).includes(
      script as CheckKind
    )
      ? (script as CheckKind)
      : 'test';
    const bin =
      kind === 'typecheck'
        ? 'check'
        : kind === 'lint'
          ? 'clippy'
          : kind === 'test'
            ? 'test'
            : 'build';
    const target = member.name ? `-p ${member.name}` : `--manifest-path ${member.path}/Cargo.toml`;
    return {
      command: `cargo ${bin} ${target}`,
      kind,
      source: `${member.path}/Cargo.toml [${member.name ?? 'member'}]`,
    };
  }
  const kind = scriptKind(script);
  const ref = member.name ?? `./${member.path}`;
  let command: string;
  switch (packageManager) {
    case 'pnpm':
      command = `pnpm --filter ${ref} run ${script}`;
      break;
    case 'yarn-berry':
      command = member.name
        ? `yarn workspace ${ref} run ${script}`
        : `yarn --cwd ${member.path} run ${script}`;
      break;
    case 'yarn':
      command = member.name
        ? `yarn workspace ${ref} run ${script}`
        : `yarn --cwd ${member.path} ${script}`;
      break;
    case 'bun':
      command = `bun run --filter '${ref}' ${script}`;
      break;
    default:
      command = `npm run ${script} --workspace=${ref}`;
      break;
  }
  return { command, kind, source: `${member.path}/package.json scripts.${script}` };
}

/**
 * The package.json scripts that run each kind of check, as the backend
 * looks for them (CHECK_SCRIPTS in project_profile/node.rs): keep the two
 * the same.
 */
const CHECK_SCRIPTS: [CheckKind, string[]][] = [
  ['typecheck', ['typecheck', 'type-check', 'check-types', 'types', 'tsc']],
  ['lint', ['lint', 'lint:check', 'check:lint', 'lint:ci']],
  ['test', ['test']],
  ['build', ['build']],
];

/** Script name → check kind for "add script" suggestions; any other is a test. */
export function scriptKind(script: string): CheckKind {
  return CHECK_SCRIPTS.find(([, names]) => names.includes(script))?.[0] ?? 'test';
}

export interface WorkspaceScriptSuggestion {
  script: string;
  kind: CheckKind;
  command: string;
  packages: number;
}

/**
 * Distinct node workspace scripts not already covered by a check, with the
 * workspace-wide command that would run each. Powers one-click "add" chips
 * in Project checks; empty for non-workspaces.
 */
export function workspaceScriptSuggestions(
  profile: ProjectProfile | null,
  checks: ProjectCheck[]
): WorkspaceScriptSuggestion[] {
  const members = (profile?.workspaces ?? []).filter((w) => w.kind === 'node');
  if (!members.length) return [];
  const have = new Set(checks.map((c) => c.command.trim()));
  const counts = new Map<string, number>();
  for (const m of members) {
    for (const s of new Set(m.scripts)) counts.set(s, (counts.get(s) ?? 0) + 1);
  }
  return [...counts.entries()]
    .map(([script, packages]) => ({
      script,
      kind: scriptKind(script),
      command: workspaceScriptCommand(profile?.packageManager, script),
      packages,
    }))
    .filter((s) => !have.has(s.command))
    .sort((a, b) => b.packages - a.packages || a.script.localeCompare(b.script))
    .slice(0, 8);
}

/** Checks in the order they run: cheap and broad first. */
export function orderedChecks(checks: ProjectCheck[]): ProjectCheck[] {
  return [...checks].sort((a, b) => CHECK_KINDS.indexOf(a.kind) - CHECK_KINDS.indexOf(b.kind));
}
