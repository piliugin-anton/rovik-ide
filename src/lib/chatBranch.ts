// chatBranch.ts
//
// A branch of its own for a new chat's work. When a new chat's first
// message is sent while the project is on its main branch, its run waits
// while the user is asked whether to create a branch and switch to it
// first (components/NewBranchDialog.tsx), under a name made up for it
// (lib/branchNames.ts). Switching keeps the files as they are: the branch
// starts at the commit the project is on.
//
// A chat across a workspace's folders (docs/design/workspaces-plan.md, D29)
// is asked once about every repository of its folders on its main branch,
// with one name for all, and told which of them stay on another branch.
// Yes makes the branch in each in turn; one that fails says which have it,
// and Yes again makes it in the rest. A folder that joins the chat later
// is asked about at its next send: offered the branch the chat's other
// repositories are on, when they agree on one that isn't their main.
//
// Whether to ask is a setting of the user's on this computer, shared by
// the windows (lib/sharedState.ts): on, until the dialog's "Don't ask
// again" turns it off; Settings → Agent runs turns it on again.
import { createSignal } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import type { ChatMessage, GitHead, GitRepos } from '../types';
import { suggestBranchName } from './branchNames';
import { sharedMap, type Entries } from './sharedState';

const saved = sharedMap('chat-branch', { key: 'rovik.chatBranch' });
/** On unless turned off: the default is saved as nothing. */
const asks = (all: Entries) => all.offer !== false;
const [offers, setOffers] = createSignal(asks(saved.all()));
// Another window's change.
saved.subscribe((all) => setOffers(asks(all)));

/** Whether a new chat on the main branch is offered a branch of its own (reactive). */
export function offersChatBranch(): boolean {
  return offers();
}

/** Turns the offer on or off, in every window. */
export function setOffersChatBranch(offer: boolean): void {
  setOffers(offer);
  void saved.set({ offer: offer ? null : false });
}

/** A folder of a chat's, as the window has it. */
export interface BranchFolder {
  path: string;
  name: string;
}

/** A repository and the branch it's on. */
export interface RepoBranch {
  name: string;
  branch: string;
}

/** What a chat is offered. */
export interface BranchOffer {
  /**
   * The repositories the branch is made in, each on its main branch: a
   * folder chat's one, the project.
   */
  repos: RepoBranch[];
  /** The chat's repositories on another branch, which they keep. */
  staying: RepoBranch[];
  /**
   * The name offered: the chat's branch when its other repositories are on
   * one, else one made up for it (lib/branchNames.ts).
   */
  suggested: string;
}

/**
 * Asks the user (components/NewBranchDialog.tsx): Yes creates the branch
 * with `create`, and the dialog stays open, saying why, when that fails.
 * Resolves once it's closed, with whether to ask again ("Don't ask again"
 * unticked).
 */
export type AskForBranch = (
  offer: BranchOffer,
  create: (name: string) => Promise<void>
) => Promise<boolean>;

/** A repository of a chat's folders. */
interface Repo {
  name: string;
  /** The folder git is asked from. */
  from: string;
  /** It holds a folder new to the chat: the offer asks about it. */
  fresh: boolean;
}

/** Where HEAD is in the repository at `path`; null when git can't say. */
async function headOf(path: string): Promise<GitHead | null> {
  try {
    return await invoke<GitHead>('git_head', { path });
  } catch (err) {
    // A folder that isn't in a repository is the usual case, not a problem.
    if (!/not a git repository/i.test(String(err))) console.warn('No branch offered:', err);
    return null;
  }
}

/**
 * The repositories of `folders`: one folder's is its own; several are
 * grouped by repository (`git_repos`), two folders of one asked about once.
 */
async function reposOf(
  folders: readonly BranchFolder[],
  fresh: (folder: BranchFolder) => boolean
): Promise<Repo[]> {
  if (folders.length === 1) {
    const [only] = folders;
    return [{ name: only.name, from: only.path, fresh: fresh(only) }];
  }
  const { repos } = await invoke<GitRepos>('git_repos', {
    folders: folders.map(({ path, name }) => ({ path, name })),
  });
  return repos.map((repo) => ({
    name: repo.name,
    from: repo.folders[0],
    fresh: folders.some((folder) => repo.folders.includes(folder.path) && fresh(folder)),
  }));
}

/**
 * The offer for the repositories of a chat's `folders` new to it, and those
 * the branch is made in; null when there's none to make: it's turned off,
 * or none of them is on its main branch (another branch, a detached HEAD or
 * a rebase, no commit yet, no repository). Git that can't be read makes no
 * offer: it never holds a chat up.
 */
async function offerFor(
  folders: readonly BranchFolder[],
  fresh: (folder: BranchFolder) => boolean
): Promise<{ offer: BranchOffer; making: Repo[] } | null> {
  if (!offers()) return null;
  let repos: Repo[];
  try {
    repos = await reposOf(folders, fresh);
  } catch (err) {
    console.warn('No branch offered:', err);
    return null;
  }
  const heads = await Promise.all(repos.map((repo) => headOf(repo.from)));
  const making: Repo[] = [];
  const offer: BranchOffer = { repos: [], staying: [], suggested: '' };
  /** The branches the chat's other repositories are on, off their main ones. */
  const chats = new Set<string>();
  repos.forEach((repo, at) => {
    const head = heads[at];
    if (!head?.branch) return;
    if (head.branch !== head.main_branch) {
      offer.staying.push({ name: repo.name, branch: head.branch });
      if (!repo.fresh) chats.add(head.branch);
    } else if (repo.fresh) {
      making.push(repo);
      offer.repos.push({ name: repo.name, branch: head.branch });
    }
  });
  if (making.length === 0) return null;
  offer.suggested = chats.size === 1 ? [...chats][0] : suggestBranchName();
  return { offer, making };
}

/** "api", "api and web", "api, web and docs". */
function listed(names: readonly string[]): string {
  return names.length < 2
    ? names.join('')
    : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

/**
 * Before a chat's run: the repositories of its `folders` on their main
 * branch are asked about once, together, whether to create a branch for the
 * chat's work in each and switch to it (D29); resolves once answered. Only
 * those of folders new to the chat are asked about: all of a new chat's, or
 * those not among `since`, a workspace chat's folders at its last run. Yes
 * makes the branch in each in turn; one that fails says which have it, and
 * Yes again makes it in the rest. Never rejects.
 */
export async function offerChatBranch(
  folders: readonly BranchFolder[],
  ask: AskForBranch,
  since: readonly string[] | null = null
): Promise<void> {
  const found = await offerFor(folders, (folder) => !since?.includes(folder.name));
  if (!found) return;
  const { offer, making } = found;
  const made = new Set<Repo>();
  const askAgain = await ask(offer, async (name) => {
    for (const repo of making) {
      if (made.has(repo)) continue;
      try {
        await invoke('git_create_branch', { path: repo.from, name, checkout: true, from: null });
      } catch (err) {
        if (making.length === 1) throw err;
        const have = making.filter((r) => made.has(r)).map((r) => r.name);
        const which = have.length
          ? ` ${listed(have)} ${have.length === 1 ? 'has' : 'have'} it: Yes makes it in the rest.`
          : '';
        throw `${repo.name}: ${String(err)}${which}`;
      }
      made.add(repo);
    }
  });
  if (!askAgain) setOffersChatBranch(false);
}

/**
 * A workspace chat's folders at its last run, as its run recorded them on
 * its message (the latest one that has them); null before its first run.
 */
export function foldersAtLastRun(messages: readonly ChatMessage[]): string[] | null {
  for (let at = messages.length - 1; at >= 0; at--) {
    const folders = messages[at].meta?.folders;
    if (messages[at].role === 'user' && Array.isArray(folders)) return folders;
  }
  return null;
}
