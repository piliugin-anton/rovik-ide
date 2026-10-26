// chatBranch.ts
//
// A branch of its own for a new chat's work. When a new chat's first
// message is sent while the project is on its main branch, its run waits
// while the user is asked whether to create a branch and switch to it
// first (components/NewBranchDialog.tsx), under a name made up for it
// (lib/branchNames.ts). Switching keeps the files as they are: the branch
// starts at the commit the project is on.
//
// Whether to ask is a setting of the user's on this computer, shared by
// the windows (lib/sharedState.ts): on, until the dialog's "Don't ask
// again" turns it off; Settings → Agent runs turns it on again.
import { createSignal } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import type { GitHead } from '../types';
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

/** What a new chat is offered. */
export interface BranchOffer {
  /** The branch the project is on: its main one. */
  current: string;
  /** A name made up for the new branch (lib/branchNames.ts). */
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

/**
 * The offer for a new chat of the project at `root`, or null when there's
 * none to make: it's turned off, or the project isn't on its main branch
 * (another branch, a detached HEAD or a rebase, no commit yet, no
 * repository). Git that can't be read makes no offer: it never holds a
 * chat up.
 */
async function offerFor(root: string): Promise<BranchOffer | null> {
  if (!offers()) return null;
  try {
    const head = await invoke<GitHead>('git_head', { path: root });
    if (!head.branch || head.branch !== head.main_branch) return null;
    return { current: head.branch, suggested: suggestBranchName() };
  } catch (err) {
    // A project that isn't in a repository is the usual case, not a problem.
    if (!/not a git repository/i.test(String(err))) console.warn('No branch offered:', err);
    return null;
  }
}

/**
 * Before the first run of a new chat of the project at `root`: when the
 * project is on its main branch, asks whether to create a branch for the
 * chat's work and switch to it, and resolves once answered. Never rejects.
 */
export async function offerChatBranch(root: string, ask: AskForBranch): Promise<void> {
  const offer = await offerFor(root);
  if (!offer) return;
  const askAgain = await ask(offer, async (name) => {
    await invoke('git_create_branch', { path: root, name, checkout: true, from: null });
  });
  if (!askAgain) setOffersChatBranch(false);
}
