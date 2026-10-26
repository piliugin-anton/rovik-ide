// instances.ts
//
// Rovik runs as several windows, each a process of its own with a project
// of its own: a project, or a copy of it sharing its chats, is open in one
// window at a time (src-tauri/src/instances.rs), so no two windows change
// the same chats unaware of each other. A workspace is one window's with
// every one of its folders, all or none
// (docs/design/workspaces-plan.md, D7). Opening one that another window has
// brings that window to the front instead. The chats without a project are
// one window's too, and a project a window left while its chats were at
// work stays that window's until they're done.
import { invoke } from '@tauri-apps/api/core';
import { notify } from './notifications';

/**
 * What a window asks to claim: a folder, a workspace (whose folders the
 * backend reads), or nothing — the chats without a project.
 */
export type ClaimTarget = { kind: 'folder'; path: string } | { kind: 'workspace'; id: string };

/** What claiming a project for this window came to (instances::Claim). */
export interface ProjectClaim {
  claimed: boolean;
  openElsewhere: boolean;
  /** The folder open in the other window (none for a workspace). */
  openAs: string | null;
  /** The workspace it's open in there, when it's one. */
  workspace?: string | null;
  /** That's another folder: a copy of this one, sharing its chats. */
  copy: boolean;
  /** The other window left it for another project, its chats still at work. */
  atWork: boolean;
  /** The other window was brought to the front. */
  focused: boolean;
}

const folderName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

interface ClaimOptions {
  /** Ask the window that has it to come to the front. */
  focusOther?: boolean;
  /** Try again for a moment first, for a window that's still closing (a
   *  project restored right after Rovik was quit). */
  retries?: number;
}

/**
 * Claims `target` for this window as it opens (null: the chats without a
 * project). False when another window has it open: that one is asked to
 * come to the front, and this one says why it didn't open. What this window
 * had open before is let go.
 */
export const claimProject = (target: ClaimTarget | null, options: ClaimOptions = {}) =>
  ask('project_claim', target, options);

/**
 * Whether `target` can open in this window, as claimProject finds, without
 * claiming it: this window keeps its project (the switch may yet be
 * cancelled).
 */
export const checkProject = (target: ClaimTarget | null, options: ClaimOptions = {}) =>
  ask('project_check', target, options);

async function ask(
  command: 'project_claim' | 'project_check',
  target: ClaimTarget | null,
  { focusOther = true, retries = 0 }: ClaimOptions
): Promise<boolean> {
  let claim: ProjectClaim;
  try {
    claim = await invoke<ProjectClaim>(command, { target, focusOther });
    for (let i = 0; i < retries && claim.openElsewhere; i++) {
      await new Promise((resolve) => setTimeout(resolve, 500));
      claim = await invoke<ProjectClaim>(command, { target, focusOther });
    }
  } catch (err) {
    // Better a project open in two windows than one that won't open.
    console.error('Claiming the project failed:', err);
    return true;
  }
  if (!claim.openElsewhere) return true;
  notify({
    id: 'open-elsewhere',
    kind: 'info',
    ...openElsewhereNotice(target, claim),
    timeout: 10_000,
  });
  return false;
}

/** What to tell the user about a project open in another window. */
export function openElsewhereNotice(
  target: ClaimTarget | null,
  claim: ProjectClaim
): { title: string; message: string } {
  const folder = target?.kind === 'folder' ? folderName(target.path) : null;
  // A workspace's name is the one the other window wrote: the same one.
  const name = folder ?? claim.workspace ?? 'These chats';
  const where = folder && claim.workspace ? `, in ${claim.workspace}` : '';
  if (claim.atWork) {
    return {
      title: `${name}’s chats are at work in another window`,
      message: `That window went on to another project while they work. Go back to ${name} there, or open it here once they’re done.`,
    };
  }
  if (claim.copy) {
    return {
      title: `${name}’s chats are open in another window`,
      message: `${claim.openAs ?? 'A copy of it'} shares its chats, and it’s open there. Each window has a project of its own.`,
    };
  }
  return {
    title: `${name} ${target ? 'is' : 'are'} open in another window${where}`,
    message: claim.focused
      ? 'That window is in front now. Each window has a project of its own.'
      : 'Each window has a project of its own: open another one here.',
  };
}

/**
 * Claims the chats without a project for this window, which has no folder
 * open: false when another window has them (nothing is said: this window
 * shows why in its chat panel).
 */
export async function claimNoProject(): Promise<boolean> {
  try {
    const claim = await invoke<ProjectClaim>('project_claim', {
      target: null,
      focusOther: false,
    });
    return !claim.openElsewhere;
  } catch (err) {
    console.error('Claiming the chats without a project failed:', err);
    return true;
  }
}

/** Brings the window that has the chats without a project to the front. */
export async function showNoProjectWindow(): Promise<void> {
  await invoke('project_check', { target: null, focusOther: true }).catch(() => undefined);
}

/**
 * The chats among `dialogIds` another window is at work on (a run, an
 * Autopilot task): what they wait on is that window's to judge.
 */
export async function chatsAtWorkElsewhere(dialogIds: string[]): Promise<Set<string>> {
  if (dialogIds.length === 0) return new Set();
  try {
    return new Set(await invoke<string[]>('chats_at_work_elsewhere', { dialogIds }));
  } catch (err) {
    console.error('Asking which chats are at work elsewhere failed:', err);
    return new Set();
  }
}

/** Starts another Rovik, a window for another project. */
export async function openNewWindow(): Promise<void> {
  try {
    await invoke('new_window');
  } catch (err) {
    notify({ kind: 'error', title: 'Couldn’t open a new window', message: String(err) });
  }
}
