// projectProfile.ts
//
// What the agent is told about the open project, and which commands check
// its changes. Detection is in the backend (src-tauri/src/project_profile.rs);
// the user can correct the detected checks per project (ProjectChecksSettings),
// which is stored separately and wins once saved. The backend builds the
// agent's prompt section from them (src-tauri/src/agent_run/project.rs).
import { invoke } from '@tauri-apps/api/core';

export type CheckKind = 'typecheck' | 'lint' | 'test' | 'build';

export const CHECK_KINDS: CheckKind[] = ['typecheck', 'lint', 'test', 'build'];

export const CHECK_LABEL: Record<CheckKind, string> = {
  typecheck: 'Type check',
  lint: 'Lint',
  test: 'Tests',
  build: 'Build',
};

export interface DetectedCheck {
  kind: CheckKind;
  command: string;
  /** Where it was found, e.g. "package.json scripts.test". */
  source: string;
  defaultEnabled: boolean;
}

export interface ProjectProfile {
  stack: string[];
  checks: DetectedCheck[];
  instructions: { file: string; text: string; truncated: boolean } | null;
}

export interface ProjectCheck {
  kind: CheckKind;
  command: string;
  enabled: boolean;
}

/** Per-project settings. `checks` replaces the detected list once the user edits it. */
export interface ProjectSettings {
  checks?: ProjectCheck[];
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

/** null removes the project's settings (back to what's detected). */
export function saveProjectSettings(root: string, settings: ProjectSettings | null): Promise<void> {
  return invoke('save_project_settings', { root, settings });
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
      .map((c) => ({ kind: c.kind, command: c.command, enabled: c.enabled !== false })),
  };
}

/** The checks in effect: the user's list when they saved one, else what's detected. */
export function effectiveChecks(
  profile: ProjectProfile | null,
  settings: ProjectSettings
): ProjectCheck[] {
  if (settings.checks) return settings.checks;
  return (profile?.checks ?? []).map((c) => ({
    kind: c.kind,
    command: c.command,
    enabled: c.defaultEnabled,
  }));
}

/** Checks in the order they run: cheap and broad first. */
export function orderedChecks(checks: ProjectCheck[]): ProjectCheck[] {
  return [...checks].sort((a, b) => CHECK_KINDS.indexOf(a.kind) - CHECK_KINDS.indexOf(b.kind));
}
