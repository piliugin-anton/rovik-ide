// GitRepoSection.tsx
//
// One repository of the window's Git panel (GitPanel.tsx): its status, its
// header (the branch with its menu, Fetch, Pull, History and the ⋯ menu),
// a merge's or a rebase's banner, its errors, its changes, and the conflict
// resolver. A project's panel is one of these with no title of its own; a
// workspace's has one per repository its folders are in (D10), each named,
// with its box for the commit the panel makes across them, and one for each
// folder in no repository, which offers to make one. The commit message and
// its button are the panel's.
import {
  createSignal,
  createMemo,
  onMount,
  onCleanup,
  For,
  Show,
  Switch,
  Match,
  createEffect,
  on,
  type Accessor,
  type JSX,
} from 'solid-js';
import { listen } from '@tauri-apps/api/event';
import { createVirtualizer } from '@tanstack/solid-virtual';
import { invoke } from '@tauri-apps/api/core';
import {
  GitFileStatusCode,
  GitPanelPhase,
  GitStatus,
  GitStashEntry,
  GitStashFile,
  GitCommitEntry,
  GitLogPage,
  GitPullConfig,
  GitPullOutcome,
  GitPushOutcome,
} from '../types';
import { confirmDialog } from './ConfirmDialog';
import { SKIP_HINTS, skippableHook } from '../lib/gitHooks';
import { branchNameProblem } from '../lib/branchNames';
import { samePath } from '../lib/fileTools';
import { hasWork, includable, includeBlocked } from '../lib/gitPublish';
import { ConflictResolver } from './ConflictResolver';
import { GitHooksView } from './GitHooksView';
import { PullStrategyDialog, type PullStrategyPick } from './PullStrategyDialog';
import { ChevronIcon, FileIcon, FolderIcon } from './FileIcons';
import {
  IconAlert,
  IconArchive,
  IconArrowDown,
  IconBack,
  IconBranch,
  IconChevronDown,
  IconDots,
  IconFolderGit,
  IconHistory,
  IconHook,
  IconRebase,
  IconRefresh,
  IconSpinner,
  IconTrash,
} from './GitIcons';

const GIT_ROW_HEIGHT = 22;

/** What the app does for the Git panel: opening files, reloading tabs. */
export interface GitPanelEvents {
  /** A changed file was clicked: open it with its changes marked. */
  onOpenFile?: (absPath: string, file: string) => void;
  /** A stashed file was clicked: open its stashed content read-only. */
  onOpenStashFile?: (absPath: string, file: string, index: number, message?: string) => void;
  /** A committed file was clicked: open its content at the commit read-only. */
  onOpenCommitFile?: (absPath: string, file: string, oid: string, summary?: string) => void;
  /** A file was reverted on disk (its tab should show the restored text). */
  onFileReverted?: (absPath: string) => void;
  /** Git's state may have changed (commit, revert, checkout, refresh). */
  onChanged?: () => void;
  /** Stash list changed (push/pop/drop): preview tabs are now stale. */
  onStashChanged?: () => void;
  /** Files may have changed on disk (pull, merge, rebase): reload open tabs. */
  onFilesChangedOnDisk?: () => void;
  /** A file to edit that isn't a change (a git hook): open it as it is. */
  onOpenPath?: (absPath: string) => void;
}

/** Where a section of the Git panel asks git from (git_repos). */
export interface GitSectionSource {
  /** The repository's git folder, or `folder:<path>` for a folder in none. */
  key: string;
  /** The folder its commands are asked from: the first of the window's folders in it. */
  path: string;
  /** The window's folders in the repository, relative to its working folder;
   *  none for a folder in no repository. */
  scopes?: string[];
  /** Its name in its title: the folder's stored name when the repository is that folder (D8). */
  name: string;
  /** The window's folders it shows: a change in any of them refreshes it. */
  folders: string[];
}

/** A git hook stopped the action `error` says: the button that tries it once
 *  more without the hooks that check (git's --no-verify). */
export interface SkipOffer {
  error: string;
  label: string;
  hint: string;
  run: () => Promise<void>;
}

/** What the panel's commit does with a section. */
export interface GitRepoHandle {
  /** Its status: null until it's read, and for a folder in no repository. */
  status: Accessor<GitStatus | null>;
  /** One of its own actions is running. */
  busy: Accessor<boolean>;
  /** Runs `fn` as one of its actions: its controls wait, and what `fn`
   *  throws shows in it. */
  run: (fn: () => Promise<void>) => Promise<void>;
  /** Reads its status again. */
  refreshStatus: () => Promise<GitStatus | null>;
  /** Reads its history again, when it's open. */
  refreshHistory: () => void;
  /** Pushes its branch; a push the remote turns down offers to pull first. */
  push: (noVerify?: boolean) => Promise<void>;
  /** With the error `e` says, offers `run` once more without the hooks that check. */
  offerSkip: (e: unknown, label: string, hint: string, run: () => Promise<void>) => void;
}

interface GitRepoSectionProps extends GitPanelEvents {
  source: GitSectionSource;
  /** One of several: it shows its name, and its box for the commit. */
  several: boolean;
  /** In the panel's next commit (with several). */
  included: boolean;
  onInclude: (included: boolean) => void;
  /** The panel's commit across repositories is running: the section waits. */
  locked: boolean;
  /** A repository was made here (Initialize): the panel reads its list again. */
  onRepositoriesChanged: () => void;
  /** Its handle, as it starts, and null as it goes. */
  handle: (handle: GitRepoHandle | null) => void;
}

/** What failed, under a section's header or above the commit bar, with the
 *  skip offer when a hook stopped it. */
export function GitInlineError(props: {
  error: string;
  offer: SkipOffer | null;
  disabled: boolean;
}) {
  return (
    <div class="git-inline-error">
      <IconAlert />
      <div class="git-inline-error-body">
        <span class="git-inline-error-text">{props.error}</span>
        <Show when={props.offer?.error === props.error ? props.offer : null}>
          {(offer) => (
            <button
              type="button"
              class="git-skip-hooks"
              disabled={props.disabled}
              title={offer().hint}
              onClick={() => void offer().run()}
            >
              {offer().label}
            </button>
          )}
        </Show>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// File status -> label/color mapping
// ---------------------------------------------------------------------------

function statusMeta(status: GitFileStatusCode): {
  label: string;
  className: string;
  title: string;
} {
  switch (status) {
    case 'M':
      return { label: 'M', className: 'git-status-modified', title: 'Modified' };
    case 'A':
      return { label: 'A', className: 'git-status-added', title: 'Added' };
    case 'D':
      return { label: 'D', className: 'git-status-deleted', title: 'Deleted' };
    case 'R':
      return { label: 'R', className: 'git-status-renamed', title: 'Renamed' };
    case 'U':
      return { label: 'U', className: 'git-status-conflicted', title: 'Conflicted' };
    case '?':
    default:
      return { label: 'U', className: 'git-status-untracked', title: 'Untracked' };
  }
}

// ---------------------------------------------------------------------------
// Changed files -> folder tree
// ---------------------------------------------------------------------------

type GitFile = GitStatus['files'][number];

/** What reverting a folder did (src-tauri/src/git.rs, git_revert_folder). */
interface FolderRevertOutcome {
  reverted: string[];
  /** Conflicted or renamed: left as they are. */
  skipped: string[];
  failed: { path: string; reason: string }[];
}

/** The revert buttons' arrow, on file and folder rows. */
function RevertIcon() {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M3.5 6.5h6.25a3.25 3.25 0 0 1 0 6.5H6" />
      <path d="M6 3.5 3 6.5l3 3" />
    </svg>
  );
}

/**
 * `+3 −3` line counts, green for additions and red for deletions. Read as
 * they're drawn: a changes row stays while the status refreshes, so counts
 * read once kept a file's first save and hid every save after it.
 */
function DiffStats(props: { additions?: number; deletions?: number }) {
  const additions = () => props.additions ?? 0;
  const deletions = () => props.deletions ?? 0;
  const title = () =>
    additions() > 0 && deletions() > 0
      ? `${additions()} added line${additions() === 1 ? '' : 's'}, ${deletions()} removed`
      : additions() > 0
        ? `${additions()} added line${additions() === 1 ? '' : 's'}`
        : `${deletions()} removed line${deletions() === 1 ? '' : 's'}`;
  return (
    <Show when={additions() > 0 || deletions() > 0}>
      <span class="git-diff-stats" title={title()}>
        {additions() > 0 && <span class="git-diff-add">+{additions()}</span>}
        {deletions() > 0 && <span class="git-diff-del">−{deletions()}</span>}
      </span>
    </Show>
  );
}

interface GitDirNode {
  name: string;
  /** Relative to `workdir`, `/`-separated; '' for the root. */
  path: string;
  dirs: Map<string, GitDirNode>;
  files: GitFile[];
  /** Changed files anywhere below this folder. */
  count: number;
  /** Summed added / removed lines of everything below this folder. */
  additions: number;
  deletions: number;
}

type GitTreeRow =
  | { kind: 'dir'; depth: number; name: string; node: GitDirNode }
  | { kind: 'file'; depth: number; name: string; file: GitFile };

const asDirRow = (r: GitTreeRow) => (r.kind === 'dir' ? r : undefined);
const asFileRow = (r: GitTreeRow) => (r.kind === 'file' ? r : undefined);

const byName = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' }).compare;

function buildGitTree(files: GitFile[]): GitDirNode {
  const root: GitDirNode = {
    name: '',
    path: '',
    dirs: new Map(),
    files: [],
    count: 0,
    additions: 0,
    deletions: 0,
  };
  for (const file of files) {
    const parts = file.path.split('/').filter(Boolean);
    let node = root;
    node.count++;
    node.additions += file.additions ?? 0;
    node.deletions += file.deletions ?? 0;
    for (const part of parts.slice(0, -1)) {
      let next = node.dirs.get(part);
      if (!next) {
        next = {
          name: part,
          path: node.path ? `${node.path}/${part}` : part,
          dirs: new Map(),
          files: [],
          count: 0,
          additions: 0,
          deletions: 0,
        };
        node.dirs.set(part, next);
      }
      next.count++;
      next.additions += file.additions ?? 0;
      next.deletions += file.deletions ?? 0;
      node = next;
    }
    node.files.push(file);
  }
  return root;
}

/** The visible rows: folders first, then files, each sorted by name. */
function flattenGitTree(root: GitDirNode, collapsed: Set<string>): GitTreeRow[] {
  const rows: GitTreeRow[] = [];
  const walk = (node: GitDirNode, depth: number) => {
    for (const top of [...node.dirs.values()].sort((a, b) => byName(a.name, b.name))) {
      // A chain of folders that each hold only one subfolder (and no files)
      // shows as one row, `src/lib/lsp`, standing for its deepest folder.
      let dir = top;
      let name = top.name;
      while (dir.files.length === 0 && dir.dirs.size === 1) {
        dir = dir.dirs.values().next().value!;
        name += `/${dir.name}`;
      }
      rows.push({ kind: 'dir', depth, name, node: dir });
      if (!collapsed.has(dir.path)) walk(dir, depth + 1);
    }
    const files = node.files.map((file) => ({ file, name: file.path.split('/').pop()! }));
    for (const { file, name } of files.sort((a, b) => byName(a.name, b.name))) {
      rows.push({ kind: 'file', depth, name, file });
    }
  };
  walk(root, 0);
  return rows;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function GitRepoSection(props: GitRepoSectionProps) {
  //let gitFilesScrollEl: HTMLDivElement | undefined;
  const [gitFilesScrollEl, setGitFilesScrollEl] = createSignal<HTMLDivElement | null>(null);

  const [phase, setPhase] = createSignal<GitPanelPhase>('uninitialized');
  const [status, setStatus] = createSignal<GitStatus | null>(null);
  const [errorMsg, setErrorMsg] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  /** Its controls wait: one of its actions is running, or the panel's commit. */
  const waiting = () => busy() || props.locked;
  /** The skip offer for a hook that stopped an action; shown only with that
   *  action's error. */
  const [skipOffer, setSkipOffer] = createSignal<SkipOffer | null>(null);
  /** Offers `run` as `label` when `e` is a check hook that stopped the action. */
  const offerSkip = (e: unknown, label: string, hint: string, run: () => Promise<void>) => {
    const error = e instanceof Error ? e.message : String(e);
    if (skippableHook(error)) setSkipOffer({ error, label, hint, run });
  };

  const [branchOpen, setBranchOpen] = createSignal(false);
  interface GitBranch {
    name: string;
    is_remote: boolean;
  }
  const [branches, setBranches] = createSignal<GitBranch[]>([]);
  const [branchesLoading, setBranchesLoading] = createSignal(false);

  const filesCount = createMemo(() => status()?.files.length ?? 0);

  // Folders start expanded; only the ones the user closed are remembered,
  // so a folder that appears with a new change shows its files.
  const [collapsed, setCollapsed] = createSignal<Set<string>>(new Set());
  const gitTree = createMemo(() => buildGitTree(status()?.files ?? []));
  const treeRows = createMemo(() => flattenGitTree(gitTree(), collapsed()));
  const toggleDir = (path: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (!next.delete(path)) next.add(path);
      return next;
    });

  // Runs `fn`, tracking a shared busy flag and surfacing any thrown error.
  // Every git operation below goes through this so loading/disabled state
  // and error reporting stay consistent in one place.
  const withBusy = async <T,>(fn: () => Promise<T>) => {
    setBusy(true);
    setErrorMsg(null);
    setSkipOffer(null);
    try {
      await fn();
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const refreshStatus = async (): Promise<GitStatus | null> => {
    try {
      const s = await invoke<GitStatus>('git_status', {
        path: props.source.path,
        scopes: props.source.scopes,
      });
      setStatus(s);
      setPhase('ready');
      props.onChanged?.();
      return s;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setStatus(null);
      if (/not a git repository/i.test(msg)) {
        setPhase('no-repo');
      } else {
        setPhase('error');
        setErrorMsg(msg);
      }
      return null;
    }
  };

  /** A row's file on disk. */
  const absPathOf = (file: string) => {
    const root = status()?.workdir || props.source.path;
    // Git names files with `/`; on Windows the tree and tabs use `\`, and a
    // mixed path would open a second tab of the same file.
    const sep = root.includes('\\') && !root.includes('/') ? '\\' : '/';
    return `${root.replace(/[\\/]+$/, '')}${sep}${file.replace(/[\\/]/g, sep)}`;
  };

  /**
   * Puts a file back as it was: the staged version, which in this app is
   * the last commit. The current version goes to the trash first; a new
   * file goes to the trash (src-tauri/src/git.rs, git_revert_file).
   */
  const revertFile = async (file: GitStatus['files'][number]) => {
    const name = file.path.split('/').pop() ?? file.path;
    const isNew = file.status === '?' || file.status === 'A';
    const ok = await confirmDialog({
      title: isNew ? `Remove the new file ${name}?` : `Revert ${name}?`,
      message: isNew
        ? 'It isn’t in git yet, so there’s no earlier version: it’s moved to the trash, where you can restore it.'
        : file.status === 'D'
          ? 'The deleted file is restored from git.'
          : `Your changes are undone. Your current version is moved to the trash as “${name.replace(/(\.[^.]+)?$/, ' (before revert)$1')}”, so you can get it back.`,
      confirmLabel: isNew ? 'Move to Trash' : 'Revert',
      danger: true,
    });
    if (!ok) return;
    await withBusy(async () => {
      await invoke('git_revert_file', { path: props.source.path, file: file.path });
      props.onFileReverted?.(absPathOf(file.path));
      await refreshStatus();
    });
  };

  /** The files below a folder of the Git tree, at any depth. */
  const filesBelow = (node: GitDirNode): GitFile[] => [
    ...node.files,
    ...[...node.dirs.values()].flatMap(filesBelow),
  ];
  /** What a folder's revert takes: conflicted and renamed files are left (their rows offer no revert either). */
  const revertable = (node: GitDirNode) =>
    filesBelow(node).filter((f) => f.status !== 'U' && f.status !== 'R');

  /**
   * Reverts every file below a folder as revertFile does each, in one go
   * (src-tauri/src/git.rs, git_revert_folder): changed files go back to the
   * last committed version with the current versions in the trash, and new
   * files go to the trash.
   */
  const revertFolder = async (dir: GitDirNode) => {
    const files = revertable(dir);
    if (files.length === 0) return;
    const fresh = files.filter((f) => f.status === '?' || f.status === 'A').length;
    const changed = files.length - fresh;
    const left = dir.count - files.length;
    const one = (n: number, single: string, more: string) => (n === 1 ? single : more);
    const message = [
      changed > 0 &&
        `${changed} changed ${one(changed, 'file goes', 'files go')} back to the last committed version; your current ${one(changed, 'version is', 'versions are')} moved to the trash as “… (before revert)”, so you can get ${one(changed, 'it', 'them')} back.`,
      fresh > 0 &&
        `${fresh} new ${one(fresh, 'file isn’t', 'files aren’t')} in git yet, so there’s no earlier version: ${one(fresh, 'it’s', 'they’re')} moved to the trash, where you can restore ${one(fresh, 'it', 'them')}.`,
      left > 0 &&
        `${left} conflicted or renamed ${one(left, 'file is', 'files are')} left as ${one(left, 'it is', 'they are')}.`,
    ]
      .filter(Boolean)
      .join(' ');
    const ok = await confirmDialog({
      title:
        changed === 0
          ? `Remove the ${one(fresh, 'new file', `${fresh} new files`)} in ${dir.path}?`
          : `Revert ${one(files.length, 'the file', `${files.length} files`)} in ${dir.path}?`,
      message,
      confirmLabel: changed === 0 ? 'Move to Trash' : 'Revert',
      danger: true,
    });
    if (!ok) return;
    await withBusy(async () => {
      const outcome = await invoke<FolderRevertOutcome>('git_revert_folder', {
        path: props.source.path,
        folder: dir.path,
      });
      for (const file of outcome.reverted) props.onFileReverted?.(absPathOf(file));
      await refreshStatus();
      if (outcome.failed.length > 0) {
        const list = outcome.failed.map((f) => `${f.path} (${f.reason})`).join('; ');
        throw new Error(
          `${outcome.failed.length} ${one(outcome.failed.length, 'file wasn’t', 'files weren’t')} reverted: ${list}`
        );
      }
    });
  };

  const initRepo = () =>
    withBusy(async () => {
      await invoke('git_init', { path: props.source.path });
      await refreshStatus();
      props.onRepositoriesChanged();
    });

  const fetchGit = () =>
    withBusy(async () => {
      await invoke('git_fetch', { path: props.source.path });
      await refreshStatus();
    });

  // -------------------------------------------------------------------------
  // Pull / push with the remote's commits merged or rebased in, as git
  // config says (src-tauri/src/git_sync.rs), and the conflicts that leaves
  // resolved in ConflictResolver.
  // -------------------------------------------------------------------------

  /** The resolver is open (on this file first, if a string). */
  const [resolver, setResolver] = createSignal<{ file?: string } | null>(null);
  /** A push the remote turned down waits for the pull's conflicts. */
  const [pendingPush, setPendingPush] = createSignal(false);
  /** The merge-or-rebase question, while it's asked. */
  const [strategyAsk, setStrategyAsk] = createSignal<{
    reason: string;
    resolve: (pick: PullStrategyPick | null) => void;
  } | null>(null);
  const askStrategy = (reason: string) =>
    new Promise<PullStrategyPick | null>((resolve) => setStrategyAsk({ reason, resolve }));
  const answerStrategy = (pick: PullStrategyPick | null) => {
    strategyAsk()?.resolve(pick);
    setStrategyAsk(null);
  };

  const operation = () => status()?.operation ?? null;

  /** Pulls; asks how when the config doesn't say. 'conflicts' opens the resolver. */
  const runPull = async (pick?: PullStrategyPick): Promise<'done' | 'conflicts' | 'cancelled'> => {
    let out: GitPullOutcome;
    try {
      out = await invoke<GitPullOutcome>('git_pull', {
        path: props.source.path,
        strategy: pick?.strategy,
        remember: pick?.remember ?? false,
      });
      if (out.result === 'needs-strategy') {
        const asked = await askStrategy(
          'Your branch and its upstream have diverged, and git config (pull.rebase) doesn’t say how to combine them.'
        );
        if (!asked) return 'cancelled';
        out = await invoke<GitPullOutcome>('git_pull', {
          path: props.source.path,
          strategy: asked.strategy,
          remember: asked.remember,
        });
      }
    } catch (e) {
      // A merge a git hook refused stays in progress, to continue or abort.
      await refreshStatus();
      props.onFilesChangedOnDisk?.();
      offerSkip(e, 'Commit merge without hooks', SKIP_HINTS.merge, () =>
        withBusy(() => continueOperation(undefined, true))
      );
      throw e;
    }
    await refreshStatus();
    props.onFilesChangedOnDisk?.();
    refreshHistoryIfOpen();
    if (out.result === 'conflicts') {
      setResolver({ file: out.conflicts[0] });
      return 'conflicts';
    }
    return 'done';
  };

  const pullGit = () =>
    withBusy(async () => {
      await runPull();
    });

  const strategyWords = (cfg: GitPullConfig) =>
    cfg.strategy === 'rebase'
      ? `rebasing your commits onto them (${cfg.source})`
      : `merging them into your branch (${cfg.source})`;

  /** One push. A pre-push hook that stops it offers to push without it. */
  const pushOnce = async (noVerify: boolean) => {
    try {
      return await invoke<GitPushOutcome>('git_push', { path: props.source.path, noVerify });
    } catch (e) {
      offerSkip(e, 'Push without hooks', SKIP_HINTS.push, () =>
        withBusy(async () => {
          try {
            await push(true);
          } finally {
            await refreshStatus();
            refreshHistoryIfOpen();
          }
        })
      );
      throw e;
    }
  };

  /** Pushes; a push the remote turns down (it has new commits) offers to
   *  pull them in first, the way git config says, then push again. */
  const push = async (noVerify = false) => {
    const out = await pushOnce(noVerify);
    if (out.result === 'pushed') {
      setPendingPush(false);
      return;
    }
    const cfg = await invoke<GitPullConfig>('git_pull_config', { path: props.source.path });
    let pick: PullStrategyPick | undefined;
    if (cfg.strategy === 'merge' || cfg.strategy === 'rebase') {
      const ok = await confirmDialog({
        title: 'The remote has new commits',
        message: `The remote branch has commits yours doesn’t, so the push was rejected.\n\nPull them in by ${strategyWords(cfg)}, then push again? Conflicts, if any, open for you to resolve first.`,
        confirmLabel: cfg.strategy === 'rebase' ? 'Rebase & push' : 'Merge & push',
      });
      if (!ok) return;
    } else {
      const asked = await askStrategy(
        cfg.strategy === 'ff-only'
          ? 'The push was rejected: the remote branch has commits yours doesn’t. pull.ff is “only”, which can’t combine diverged branches, so choose how this time.'
          : 'The push was rejected: the remote branch has commits yours doesn’t. Pull them in first, then push again.'
      );
      if (!asked) return;
      pick = asked;
    }
    setPendingPush(true);
    const pulled = await runPull(pick);
    if (pulled !== 'done') {
      // Conflicts: the push waits for the resolver to finish.
      if (pulled === 'cancelled') setPendingPush(false);
      return;
    }
    setPendingPush(false);
    const again = await pushOnce(noVerify);
    if (again.result === 'rejected') {
      throw new Error('The remote moved again while pulling. Push once more.');
    }
  };

  /** Like withBusy, but the error goes back to the caller (the resolver shows it). */
  const busyOrThrow = async (fn: () => Promise<void>) => {
    setBusy(true);
    setErrorMsg(null);
    setSkipOffer(null);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  };

  const continueOperation = (message?: string, noVerify = false) =>
    busyOrThrow(async () => {
      let out: GitPullOutcome;
      try {
        out = await invoke<GitPullOutcome>('git_operation_continue', {
          path: props.source.path,
          message,
          noVerify,
        });
      } catch (e) {
        offerSkip(e, 'Commit merge without hooks', SKIP_HINTS.merge, () =>
          withBusy(() => continueOperation(message, true))
        );
        throw e;
      }
      await refreshStatus();
      props.onFilesChangedOnDisk?.();
      refreshHistoryIfOpen();
      // A rebase stopped again at a later commit: the resolver stays.
      if (out.result === 'conflicts') return;
      setResolver(null);
      if (pendingPush()) {
        setPendingPush(false);
        await push();
        await refreshStatus();
        refreshHistoryIfOpen();
      }
    });

  const abortOperation = () =>
    busyOrThrow(async () => {
      await invoke('git_operation_abort', { path: props.source.path });
      setPendingPush(false);
      setResolver(null);
      await refreshStatus();
      props.onFilesChangedOnDisk?.();
      refreshHistoryIfOpen();
    });

  const confirmAbort = async () => {
    const op = operation();
    if (!op) return;
    const ok = await confirmDialog({
      title: `Abort the ${op.kind}?`,
      message:
        'Your branch goes back to where it was before it started. Resolutions made so far are lost.',
      confirmLabel: `Abort ${op.kind}`,
      danger: true,
    });
    if (ok) await withBusy(abortOperation);
  };

  const switchBranch = (branch: string) =>
    withBusy(async () => {
      await invoke('git_checkout', { path: props.source.path, branch });
      setBranchOpen(false);
      await refreshStatus();
      refreshHistoryIfOpen();
    });

  const loadBranches = async () => {
    setBranchesLoading(true);
    try {
      const list = await invoke<GitBranch[]>('git_branches', { path: props.source.path });
      // The branches already listed keep their rows, whatever else came or
      // went: a click on one as the list came in was lost.
      setBranches((old) => {
        const key = (b: GitBranch) => `${b.is_remote ? 'remote' : 'local'}:${b.name}`;
        const listed = new Map(old.map((b) => [key(b), b]));
        const next = list.map((b) => listed.get(key(b)) ?? b);
        return next.length === old.length && next.every((b, i) => b === old[i]) ? old : next;
      });
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBranchesLoading(false);
    }
  };

  // -------------------------------------------------------------------------
  // "More actions" menu: the git operations the panel has no dedicated
  // control for (create/delete branch, stash, discard-all). One dropdown
  // with small inline sub-views, so no new components are needed. What the
  // panel already does — commit, push, fetch, pull, history, switching
  // branches, reverting one file — stays where it is.
  // -------------------------------------------------------------------------

  type MoreView =
    'menu' | 'new-branch' | 'rebase' | 'delete-branch' | 'stash-save' | 'stash-list' | 'hooks';

  const [moreOpen, setMoreOpen] = createSignal(false);
  const [moreView, setMoreView] = createSignal<MoreView>('menu');
  const [newBranchName, setNewBranchName] = createSignal('');
  // Which branch a new branch starts from. Defaults to the current branch
  // when the view opens; only a name that's actually in the branch list is
  // sent (anything else — detached HEAD, still loading — means HEAD).
  const [newBranchFrom, setNewBranchFrom] = createSignal('');
  // Rebase: what a pick said when it had nothing to do (its search over the
  // branches to rebase onto is `rebaseSearch`, below).
  const [rebaseNote, setRebaseNote] = createSignal<string | null>(null);
  const [stashMessage, setStashMessage] = createSignal('');
  const [stashes, setStashes] = createSignal<GitStashEntry[]>([]);
  const [stashesLoading, setStashesLoading] = createSignal(false);
  // Stash preview: which entry is expanded and the files each expanded
  // entry touched. Clicking a file opens its stashed content in a
  // read-only editor tab (deleted files have nothing to open). Cleared
  // whenever the stash list changes (pop, drop, push shift indices).
  const [expandedStash, setExpandedStash] = createSignal<number | null>(null);
  const [stashFiles, setStashFiles] = createSignal<Record<number, GitStashFile[]>>({});
  const [stashFilesLoading, setStashFilesLoading] = createSignal<number | null>(null);
  const clearStashPreview = () => {
    setExpandedStash(null);
    setStashFiles({});
  };
  // History ("Recent commits"): a dropdown of its own, opened by the clock
  // button left of Pull.
  const [historyOpen, setHistoryOpen] = createSignal(false);
  const [commits, setCommits] = createSignal<GitCommitEntry[]>([]);
  const [commitsLoading, setCommitsLoading] = createSignal(false);
  // Smart history search: query text, a seq guard so stale backend replies
  // can't overwrite newer results, and debounce delay for typing.
  // 400ms: each keystroke can scan up to 400 commits with diffs, so a
  // shorter delay fires overlapping backend searches while typing.
  const HISTORY_SEARCH_DEBOUNCE_MS = 400;
  const HISTORY_PAGE_LIMIT = 20;
  const [historyQuery, setHistoryQuery] = createSignal('');
  // The query the shown list belongs to. While typing, the input runs
  // ahead of the results for the debounce (and the search): badges,
  // per-file match flags and the list's notes must describe the results
  // on screen, not the text being typed.
  const [resultsQuery, setResultsQuery] = createSignal('');
  // Pagination cursor: raw commits already scanned (`next_offset` from the
  // backend). `hasMoreHistory` means older commits remain beyond this
  // page's scan budget — keep it separate from `commitsLoading` so the
  // first page and "load more" have their own spinners.
  const [historyOffset, setHistoryOffset] = createSignal(0);
  // The commit the paging session walks from (HEAD when it started):
  // offsets count from it, so commits made meanwhile (a terminal) can't
  // make "load more" repeat or skip one. A reset starts a new session.
  let historyTip: string | null = null;
  const [hasMoreHistory, setHasMoreHistory] = createSignal(false);
  const [historyLoadingMore, setHistoryLoadingMore] = createSignal(false);
  let historySeq = 0;
  // History preview: which commit is expanded and the files each expanded
  // commit touched (`git_commit_show`). Clicking a file opens its content
  // at the commit in a read-only editor tab, like stashed files.
  const [expandedCommit, setExpandedCommit] = createSignal<string | null>(null);
  const [commitFiles, setCommitFiles] = createSignal<Record<string, GitStashFile[]>>({});
  const [commitFilesLoading, setCommitFilesLoading] = createSignal<string | null>(null);
  // Full hash copied via the clickable short hash; shows a "Copied" tick.
  const [copiedHash, setCopiedHash] = createSignal<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  const copyHash = async (oid: string) => {
    try {
      await navigator.clipboard.writeText(oid);
      setCopiedHash(oid);
      if (copiedTimer) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => setCopiedHash((cur) => (cur === oid ? null : cur)), 1200);
    } catch (err) {
      console.error('Copy failed:', err);
    }
  };
  onCleanup(() => {
    if (copiedTimer) clearTimeout(copiedTimer);
  });

  const closeMore = () => {
    setMoreOpen(false);
    setMoreView('menu');
  };

  const toggleMore = () => {
    const next = !moreOpen();
    setMoreOpen(next);
    setBranchOpen(false);
    setHistoryOpen(false);
    if (next) {
      setMoreView('menu');
      void loadStashes();
    }
  };

  /** Opens or closes the history; it opens on the newest commits, with no search. */
  const toggleHistory = () => {
    const next = !historyOpen();
    setHistoryOpen(next);
    setBranchOpen(false);
    closeMore();
    if (!next) return;
    if (historyDebounce) clearTimeout(historyDebounce);
    historyDebounce = undefined;
    setHistoryQuery('');
    setResultsQuery('');
    setExpandedCommit(null);
    setHistoryOffset(0);
    setHasMoreHistory(false);
    void loadLog('', 'reset');
  };

  const openMoreView = (view: MoreView) => {
    setMoreView(view);
    if (view === 'new-branch') {
      setNewBranchFrom(status()?.branch ?? '');
      void loadBranches();
    }
    if (view === 'rebase') {
      rebaseSearch.reset();
      setRebaseNote(null);
      void loadBranches();
    }
    if (view === 'delete-branch') void loadBranches();
    if (view === 'stash-list') void loadStashes();
  };

  /** Why the current branch can't be rebased now; null when it can. */
  const rebaseBlocked = () => {
    const op = operation();
    if (op) return `A ${op.kind} is in progress: finish or abort it first`;
    const branch = status()?.branch ?? '';
    if (branch === 'unborn') return 'There are no commits to rebase yet';
    if (!branch || branch.startsWith('detached@')) return 'Check out a branch to rebase it';
    return null;
  };

  /** Rebases the current branch onto `b`, asking first when that rewrites
   *  commits already pushed (Rovik doesn't force a push). Conflicts open
   *  the resolver, as a pull's do. */
  const rebaseOnto = (b: GitBranch) => {
    if (waiting()) return;
    return withBusy(async () => {
      const branch = status()?.branch ?? 'This branch';
      const args = { path: props.source.path, onto: b.name, remote: b.is_remote };
      let out = await invoke<GitPullOutcome>('git_rebase', args);
      if (out.result === 'rewrites-pushed') {
        const upstream = status()?.upstream ?? 'its remote branch';
        const ok = await confirmDialog({
          title: `Rebase ${branch} onto ${b.name}?`,
          message: `Some of ${branch}’s commits are on ${upstream} already. The rebase rewrites them, and the remote then turns down a push of the rewritten ones unless it’s forced (git push --force-with-lease, in a terminal): Rovik doesn’t force a push.`,
          confirmLabel: 'Rebase anyway',
        });
        if (!ok) return;
        out = await invoke<GitPullOutcome>('git_rebase', { ...args, rewritePushed: true });
      }
      if (out.result === 'up-to-date') {
        setRebaseNote(`${branch} already has everything on ${b.name}: nothing to rebase.`);
        return;
      }
      closeMore();
      await refreshStatus();
      props.onFilesChangedOnDisk?.();
      refreshHistoryIfOpen();
      if (out.result === 'conflicts') setResolver({ file: out.conflicts[0] });
    });
  };

  /**
   * A search over branches, as the branch menu and Rebase… have it: every
   * word of it in a branch's name, local branches first. The arrow keys
   * move a cursor along both sections, Enter picks its row, Escape clears
   * the search (a second one closes the menu).
   */
  interface BranchSearch {
    query: () => string;
    /** Typed into the box: the cursor goes back to the first row. */
    edit: (value: string) => void;
    reset: () => void;
    /** What matches, local branches first, as the sections show them. */
    targets: () => GitBranch[];
    cursor: () => number;
    setCursor: (row: number) => void;
    pick: (b: GitBranch) => void;
    onKey: (e: KeyboardEvent) => void;
  }

  function branchSearch(candidates: () => GitBranch[], pick: (b: GitBranch) => void): BranchSearch {
    const [query, setQuery] = createSignal('');
    const [cursor, setCursor] = createSignal(0);
    const targets = createMemo(() => {
      const words = query().toLowerCase().split(/\s+/).filter(Boolean);
      const found = candidates().filter((b) =>
        words.every((w) => b.name.toLowerCase().includes(w))
      );
      return [...found.filter((b) => !b.is_remote), ...found.filter((b) => b.is_remote)];
    });
    const reset = () => {
      setQuery('');
      setCursor(0);
    };
    const onKey = (e: KeyboardEvent) => {
      const rows = targets();
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        const by = e.key === 'ArrowDown' ? 1 : -1;
        setCursor((i) => Math.max(0, Math.min(rows.length - 1, i + by)));
        const menu = (e.target as HTMLElement).closest('.git-branch-dropdown');
        queueMicrotask(() =>
          menu?.querySelector('.git-menu-cursor')?.scrollIntoView({ block: 'nearest' })
        );
      } else if (e.key === 'Enter') {
        e.preventDefault();
        const row = rows[Math.min(cursor(), rows.length - 1)];
        if (row) pick(row);
      } else if (e.key === 'Escape' && query() !== '') {
        e.stopPropagation();
        reset();
      }
    };
    return {
      query,
      edit: (value) => {
        setQuery(value);
        setCursor(0);
      },
      reset,
      targets,
      cursor,
      setCursor,
      pick,
      onKey,
    };
  }

  /** A branch search's box, focused as it shows; `onEdit` runs as it's typed in. */
  const branchSearchBox = (search: BranchSearch, label: string, onEdit?: () => void) => (
    <div class="git-history-search-row">
      <div class="git-history-search-wrap">
        <input
          ref={(el) => queueMicrotask(() => el.focus())}
          class="tree-row-input git-more-input git-history-search"
          placeholder="Search branches"
          value={search.query()}
          spellcheck={false}
          autocomplete="off"
          aria-label={label}
          onInput={(e) => {
            search.edit(e.currentTarget.value);
            onEdit?.();
          }}
          onKeyDown={search.onKey}
          disabled={waiting()}
        />
        <Show when={search.query() !== ''}>
          <button
            type="button"
            class="git-history-clear"
            title="Clear search"
            aria-label="Clear branch search"
            onClick={search.reset}
          >
            ✕
          </button>
        </Show>
      </div>
    </div>
  );

  /** A branch search's local or remote rows, under their label. */
  const branchSection = (
    search: BranchSearch,
    remote: boolean,
    row: { title: (b: GitBranch) => string | undefined; active?: (b: GitBranch) => boolean }
  ) => {
    const rows = () => search.targets().filter((b) => b.is_remote === remote);
    // Rows are counted across both sections, local ones first.
    const first = () => (remote ? search.targets().filter((b) => !b.is_remote).length : 0);
    return (
      <Show when={rows().length > 0}>
        <div class="git-menu-section-label">{remote ? 'Remote' : 'Local'}</div>
        <ul class="context-menu-list">
          <For each={rows()}>
            {(b, i) => (
              <li
                class="context-menu-item"
                classList={{
                  'git-menu-cursor': first() + i() === search.cursor(),
                  'git-branch-item-active': !!row.active?.(b),
                }}
                title={row.title(b)}
                onMouseMove={() => search.setCursor(first() + i())}
                onClick={() => search.pick(b)}
              >
                <IconBranch />
                <span>{b.name}</span>
              </li>
            )}
          </For>
        </ul>
      </Show>
    );
  };

  const rebaseTitle = (b: GitBranch) =>
    `Rebase ${status()?.branch ?? 'this branch'} onto ${b.name}`;

  // The branch menu's search: every branch, the current one marked; a pick
  // switches to it.
  const switchSearch = branchSearch(branches, (b) => void switchBranch(b.name));
  // Rebase…'s: the branches the current one can be rebased onto.
  const rebaseSearch = branchSearch(
    () => branches().filter((b) => b.is_remote || b.name !== status()?.branch),
    (b) => void rebaseOnto(b)
  );

  const sameStashes = (a: GitStashEntry[], b: GitStashEntry[]) =>
    a.length === b.length &&
    a.every((s, i) => s.index === b[i].index && s.message === b[i].message);

  const loadStashes = async () => {
    setStashesLoading(true);
    try {
      const list = await invoke<GitStashEntry[]>('git_stash_list', { path: props.source.path });
      // The same stashes keep their rows: opening the list loads them again
      // right after the menu did, and new rows replaced the ones a click
      // (Pop, a stash opened) was landing on.
      setStashes((old) => (sameStashes(old, list) ? old : list));
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setStashesLoading(false);
    }
  };

  /** Expand a stash to show the files it touched (`git_stash_show`). */
  const toggleStashExpanded = async (index: number) => {
    if (expandedStash() === index) {
      setExpandedStash(null);
      return;
    }
    setExpandedStash(index);
    if (stashFiles()[index]) return;
    setStashFilesLoading(index);
    try {
      const files = await invoke<GitStashFile[]>('git_stash_show', {
        path: props.source.path,
        index,
      });
      setStashFiles((prev) => ({ ...prev, [index]: files }));
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e));
      setExpandedStash(null);
    } finally {
      setStashFilesLoading(null);
    }
  };

  /** Expand a commit to show the files it changed (`git_commit_show`).
   * Passes the active history search so the backend flags per-file
   * matches; files are then filtered/highlighted in the UI. */
  const toggleCommitExpanded = async (oid: string) => {
    if (expandedCommit() === oid) {
      setExpandedCommit(null);
      return;
    }
    setExpandedCommit(oid);
    const q = resultsQuery();
    const key = `${oid}::${q}`;
    if (commitFiles()[key]) return;
    setCommitFilesLoading(oid);
    try {
      const files = await invoke<GitStashFile[]>('git_commit_show', {
        path: props.source.path,
        oid,
        query: q ? q : null,
      });
      setCommitFiles((prev) => ({ ...prev, [key]: files }));
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e));
      setExpandedCommit((cur) => (cur === oid ? null : cur));
    } finally {
      // Another commit may have been expanded meanwhile: its spinner stays.
      setCommitFilesLoading((cur) => (cur === oid ? null : cur));
    }
  };
  // When searching, show only matching files by default; user can reveal
  // the rest per expanded commit. Keyed by commit oid.
  const [showAllFiles, setShowAllFiles] = createSignal<Record<string, boolean>>({});

  /**
   * Load one page of history. `mode: 'reset'` (default) replaces the list
   * (open, new search, refresh after a commit); `mode: 'more'` appends the
   * next page at the stored cursor. The backend scans at most ~400 commits
   * per call — `hasMoreHistory` + `historyOffset` let the UI page further
   * back instead of raising that cap.
   */
  const loadLog = async (query?: string, mode: 'reset' | 'more' = 'reset') => {
    const q = (query ?? historyQuery()).trim();
    const run = ++historySeq;
    const offset = mode === 'more' ? historyOffset() : 0;
    const tip = mode === 'more' ? historyTip : null;
    if (mode === 'more') setHistoryLoadingMore(true);
    else setCommitsLoading(true);
    try {
      const page = await invoke<GitLogPage | GitCommitEntry[]>('git_log', {
        path: props.source.path,
        limit: HISTORY_PAGE_LIMIT,
        query: q ? q : null,
        offset,
        tip,
      });
      if (run !== historySeq) return;
      // The session's tip no longer exists (history rewritten and pruned):
      // the backend started over from HEAD, so this isn't the next page.
      if (mode === 'more' && !Array.isArray(page) && page.tip && tip && page.tip !== tip) {
        historyTip = page.tip;
        void loadLog(q, 'reset');
        return;
      }
      if (!Array.isArray(page)) historyTip = page.tip ?? null;
      // Accept the legacy `Vec` shape too (older backend): no cursor.
      const list = Array.isArray(page) ? page : (page.commits ?? []);
      const hasMore = Array.isArray(page) ? false : !!page.has_more;
      const nextOffset = Array.isArray(page) ? offset + list.length : page.next_offset;
      setCommits((prev) => {
        if (mode === 'more') {
          const seen = new Set(prev.map((c) => c.oid));
          return [...prev, ...list.filter((c) => !seen.has(c.oid))];
        }
        return list;
      });
      setHistoryOffset(nextOffset);
      setHasMoreHistory(hasMore);
      setResultsQuery(q);
      // Drop expansion for a commit that's no longer in the results.
      // On 'more' the list only grows, so keep the expansion.
      if (mode === 'reset') {
        const expanded = expandedCommit();
        if (expanded && !list.some((c) => c.oid === expanded)) setExpandedCommit(null);
      }
    } catch (e) {
      if (run !== historySeq) return;
      setErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      if (run === historySeq) {
        setCommitsLoading(false);
        setHistoryLoadingMore(false);
      }
    }
  };

  /** Append the next page (older commits / further search). */
  const loadMoreHistory = () => {
    if (commitsLoading() || historyLoadingMore() || !hasMoreHistory()) return;
    void loadLog(resultsQuery(), 'more');
  };

  /** Re-run the visible history after an operation that moves HEAD
   * (commit, pull, checkout, branch create/delete) so an open history
   * view never shows stale commits. No-op unless history is open. */
  const refreshHistoryIfOpen = (query?: string) => {
    if (historyOpen()) {
      setHistoryOffset(0);
      void loadLog(query ?? historyQuery(), 'reset');
    }
  };

  let historyDebounce: ReturnType<typeof setTimeout> | undefined;
  const onHistorySearchInput = (value: string) => {
    setHistoryQuery(value);
    setExpandedCommit(null);
    // Cached per-commit file lists carry the previous query's match flags.
    setCommitFiles({});
    setShowAllFiles({});
    setHistoryOffset(0);
    setHasMoreHistory(false);
    // A "load more" still in flight belongs to the old query: its reply is
    // dropped (historySeq), so its spinner must not linger.
    setHistoryLoadingMore(false);
    if (historyDebounce) clearTimeout(historyDebounce);
    // Empty query reloads immediately (cheap: newest 20, no scan);
    // typing waits for a pause, and Enter searches at once.
    historyDebounce =
      value.trim() === ''
        ? undefined
        : setTimeout(() => void loadLog(value, 'reset'), HISTORY_SEARCH_DEBOUNCE_MS);
    if (value.trim() === '') void loadLog('', 'reset');
  };
  onCleanup(() => {
    if (historyDebounce) clearTimeout(historyDebounce);
  });

  const createBranch = (checkout: boolean) =>
    withBusy(async () => {
      const name = newBranchName().trim();
      if (!name || branchNameProblem(name)) return;
      // Only a branch that's actually in the list is sent as the starting
      // point; anything else (detached HEAD, list still loading) means HEAD.
      const picked = newBranchFrom();
      const from = picked && branches().some((b) => b.name === picked) ? picked : null;
      await invoke('git_create_branch', { path: props.source.path, name, checkout, from });
      setNewBranchName('');
      closeMore();
      // The branch menu lists it from the start, not a moment after opening.
      await loadBranches();
      await refreshStatus();
      if (checkout) refreshHistoryIfOpen();
    });

  const deleteBranch = async (name: string) => {
    const ok = await confirmDialog({
      title: `Delete branch ${name}?`,
      message: 'The branch itself is gone right away; its commits linger until git cleans them up.',
      confirmLabel: 'Delete',
      danger: true,
    });
    if (!ok) return;
    await withBusy(async () => {
      await invoke('git_delete_branch', { path: props.source.path, name });
      await loadBranches();
      await refreshStatus();
    });
  };

  const stashPush = () =>
    withBusy(async () => {
      if (filesCount() === 0) return;
      await invoke('git_stash_push', {
        path: props.source.path,
        message: stashMessage().trim() || null,
      });
      setStashMessage('');
      clearStashPreview();
      props.onStashChanged?.();
      closeMore();
      await refreshStatus();
    });

  const stashPop = (index: number) =>
    withBusy(async () => {
      await invoke('git_stash_pop', { path: props.source.path, index });
      clearStashPreview();
      props.onStashChanged?.();
      await loadStashes();
      await refreshStatus();
    });

  const stashDrop = async (index: number) => {
    const ok = await confirmDialog({
      title: 'Forget this stash?',
      message: 'Its changes are thrown away without restoring them.',
      confirmLabel: 'Forget',
      danger: true,
    });
    if (!ok) return;
    await withBusy(async () => {
      await invoke('git_stash_drop', { path: props.source.path, index });
      clearStashPreview();
      props.onStashChanged?.();
      await loadStashes();
      await refreshStatus();
    });
  };

  const discardAll = async () => {
    const files = status()?.files ?? [];
    if (files.length === 0) return;
    const ok = await confirmDialog({
      title: `Throw away all ${files.length} changed file${files.length === 1 ? '' : 's'}?`,
      message:
        'Every tracked change goes back to the last commit. New files go to the trash, where you can still restore them.',
      confirmLabel: 'Discard all',
      danger: true,
    });
    if (!ok) return;
    // Tabs showing these files reload from disk afterwards.
    const absPaths = files.map((f) => absPathOf(f.path));
    await withBusy(async () => {
      await invoke('git_discard_all', { path: props.source.path, scopes: props.source.scopes });
      closeMore();
      await refreshStatus();
      for (const abs of absPaths) props.onFileReverted?.(abs);
    });
  };

  const toggleBranchDropdown = () => {
    const next = !branchOpen();
    setBranchOpen(next);
    setMoreOpen(false);
    setHistoryOpen(false);
    if (next) {
      switchSearch.reset();
      loadBranches();
    }
  };

  // Close header dropdowns on a click outside the header (same pattern
  // as the project switcher / effort menu). The triggers and menus all live
  // inside the header, so anything outside it dismisses them — except the
  // stash list and history: opening a file moves focus to the editor, and
  // they must stay open so more files can be opened.
  let headerRoot: HTMLDivElement | undefined;
  const onOutsideHeader = (e: PointerEvent) => {
    if (headerRoot && !headerRoot.contains(e.target as Node)) {
      setBranchOpen(false);
      if (moreView() !== 'stash-list' && moreView() !== 'hooks') closeMore();
    }
  };
  window.addEventListener('pointerdown', onOutsideHeader, true);
  onCleanup(() => window.removeEventListener('pointerdown', onOutsideHeader, true));

  /*
   * Git's own activity (a commit, a checkout, also from a terminal) and
   * edits to the files of its folders: the watcher reports them once per
   * burst and folder ("git-changed"), and the status is read again unless an
   * operation of this section, or the panel's commit, is running anyway. At
   * most once per GIT_REFRESH_MIN_MS, the last one after the changes stop,
   * and never two at once: dev servers rewriting files kept `git status` of
   * a whole workspace running. Another repository's folders are another
   * section's.
   */
  const GIT_REFRESH_MIN_MS = 2_000;
  onMount(() => {
    let stop: (() => void) | undefined;
    let disposed = false;
    let lastRefresh = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let running = false;
    let again = false;
    /** A change came while the panel's commit kept the section waiting. */
    let missed = false;
    const requestRefresh = () => {
      if (timer || disposed) return; // one is coming
      const wait = Math.max(0, lastRefresh + GIT_REFRESH_MIN_MS - Date.now());
      timer = setTimeout(async () => {
        timer = undefined;
        if (running) {
          again = true;
          return;
        }
        if (waiting() || phase() !== 'ready') {
          // Its own action reads the status again as it ends; the panel's
          // commit reads only the repositories it committed in.
          missed = props.locked;
          return;
        }
        running = true;
        lastRefresh = Date.now();
        try {
          await refreshStatus();
        } finally {
          running = false;
          if (again) {
            again = false;
            requestRefresh();
          }
        }
      }, wait);
    };
    // The panel's commit is over: a change it kept the section from reading
    // is read now.
    createEffect(
      on(
        () => props.locked,
        (locked) => {
          if (locked || !missed) return;
          missed = false;
          requestRefresh();
        },
        { defer: true }
      )
    );
    void listen<{ folder: string }>('git-changed', ({ payload }) => {
      if (props.source.folders.some((folder) => samePath(folder, payload.folder))) requestRefresh();
    }).then((unlisten) => {
      if (disposed) unlisten();
      else stop = unlisten;
    });
    onCleanup(() => {
      disposed = true;
      clearTimeout(timer);
      stop?.();
    });
  });

  // Read again when the window's folders in it change while the Git tab
  // stays mounted (onMount alone never re-runs).
  createEffect(
    on(
      () => [props.source.path, ...(props.source.scopes ?? [])].join('\0'),
      () => {
        void withBusy(refreshStatus);
      },
      { defer: true }
    )
  );

  onMount(() => {
    void withBusy(refreshStatus);
  });

  // Taken once: the handle goes back through the callback it came by.
  const handOver = props.handle;
  handOver({
    status,
    busy,
    run: withBusy,
    refreshStatus,
    refreshHistory: () => refreshHistoryIfOpen(),
    push,
    offerSkip,
  });
  onCleanup(() => handOver(null));

  /** Its box can't change: an action is running, a merge, a rebase or
   *  conflicts keep it out, or it has nothing to commit or push. */
  const includeOff = () => {
    const s = status();
    return waiting() || !s || !includable(s);
  };
  const includeTitle = () => {
    const s = status();
    if (!s) return undefined;
    const name = props.source.name;
    const why = includeBlocked(s, name);
    if (why) return why;
    if (!hasWork(s)) return `Nothing to commit or push in ${name}`;
    return props.included ? `${name} is in the commit` : `${name} is left out of the commit`;
  };

  // One virtualizer per scroll element, its row count following the
  // status. A new one per status refresh (any save, any agent write) or
  // folder toggle scrolled the list back to its top and drew every row
  // anew, so a click landing as it refreshed was lost.
  // (The list's element comes and goes with its <Show>: the virtualizer
  // reads it again whenever it changes.)
  const gitFilesVirtualizer = createVirtualizer({
    get count() {
      return treeRows().length;
    },
    getScrollElement: () => gitFilesScrollEl(),
    estimateSize: () => GIT_ROW_HEIGHT,
    overscan: 12,
  });

  return (
    <div class="git-repo-section" classList={{ 'git-repo-section-several': props.several }}>
      <Show when={props.several}>
        <div class="git-repo-title" title={status()?.workdir || props.source.path}>
          <Show when={status()} fallback={<span class="git-repo-name">{props.source.name}</span>}>
            <label class="git-repo-include" title={includeTitle()}>
              <input
                type="checkbox"
                checked={props.included}
                disabled={includeOff()}
                aria-label={`Include ${props.source.name} in the commit`}
                onChange={(e) => props.onInclude(e.currentTarget.checked)}
              />
              <span class="git-repo-name">{props.source.name}</span>
            </label>
          </Show>
        </div>
      </Show>
      <Show when={phase() === 'uninitialized' && busy()}>
        <div class="git-panel-loading empty-state">
          <IconSpinner size={16} />
          <span>Loading repository...</span>
        </div>
      </Show>

      <Show when={phase() === 'no-repo'}>
        <div class="git-panel-empty empty-state">
          <IconFolderGit />
          <p>This folder isn't a Git repository yet.</p>
          <button
            class="git-init-button primary icon-button"
            onClick={initRepo}
            disabled={waiting()}
          >
            <Show when={busy()} fallback={<IconBranch />}>
              <IconSpinner />
            </Show>
            <span>Initialize repository</span>
          </button>
        </div>
      </Show>

      <Show when={phase() === 'error'}>
        <div class="git-panel-empty empty-state git-panel-error">
          <IconAlert />
          <p>{errorMsg() ?? 'Something went wrong talking to Git.'}</p>
          <button class="icon-button" onClick={() => withBusy(refreshStatus)} disabled={waiting()}>
            <IconRefresh />
            <span>Retry</span>
          </button>
        </div>
      </Show>

      <Show when={phase() === 'ready'}>
        {/* Header */}
        <div
          class="git-panel-header"
          ref={headerRoot}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              setBranchOpen(false);
              setHistoryOpen(false);
              closeMore();
            }
          }}
        >
          <div class="git-branch-row">
            <button
              class="git-branch-button icon-button"
              onClick={toggleBranchDropdown}
              title="Switch branch"
              aria-haspopup="menu"
              aria-expanded={branchOpen()}
              disabled={waiting()}
            >
              <IconBranch />
              <span class="git-branch-name">{status()?.branch ?? '...'}</span>
              <Show when={(status()?.ahead ?? 0) > 0}>
                <span class="git-branch-badge git-badge-ahead" title={`${status()!.ahead} ahead`}>
                  ↑{status()!.ahead}
                </span>
              </Show>
              <Show when={(status()?.behind ?? 0) > 0}>
                <span
                  class="git-branch-badge git-badge-behind"
                  title={`${status()!.behind} behind`}
                >
                  ↓{status()!.behind}
                </span>
              </Show>
              <IconChevronDown />
            </button>

            <div class="git-fetch-pull-row">
              <button class="icon-button" onClick={fetchGit} title="Fetch" disabled={waiting()}>
                <Show when={busy()} fallback={<IconRefresh />}>
                  <IconSpinner />
                </Show>
              </button>
              <button class="icon-button" onClick={pullGit} title="Pull" disabled={waiting()}>
                <IconArrowDown />
              </button>
              <button
                class="icon-button"
                onClick={toggleHistory}
                title="Recent commits"
                aria-label="Recent commits"
                aria-haspopup="dialog"
                aria-expanded={historyOpen()}
                disabled={waiting()}
              >
                <IconHistory />
              </button>
            </div>
          </div>
          <button
            class="icon-button git-more-trigger"
            onClick={toggleMore}
            title="More actions"
            aria-label="More git actions"
            aria-haspopup="menu"
            aria-expanded={moreOpen()}
            disabled={waiting()}
          >
            <IconDots />
          </button>

          <Show when={branchOpen()}>
            <div class="git-branch-dropdown context-menu">
              {branchSearchBox(switchSearch, 'Search branches to switch to')}
              {/* Only while there's nothing to list yet: over the rows, it
                  moved them all down and back while they refreshed. */}
              <Show when={branchesLoading() && branches().length === 0}>
                <div class="context-menu-item context-menu-loading">
                  <IconSpinner /> <span>Loading branches...</span>
                </div>
              </Show>
              <Show
                when={switchSearch.query().trim() !== '' && switchSearch.targets().length === 0}
              >
                <div class="git-more-note">No branch matches.</div>
              </Show>
              {branchSection(switchSearch, false, {
                title: () => undefined,
                active: (b) => b.name === status()?.branch,
              })}
              {branchSection(switchSearch, true, {
                title: () => 'Check out as a local tracking branch',
              })}
            </div>
          </Show>

          <Show when={moreOpen()}>
            <div
              class="git-branch-dropdown git-more-dropdown context-menu"
              role="menu"
              aria-label="More git actions"
            >
              {/* Main menu */}
              <Show when={moreView() === 'menu'}>
                <div class="git-menu-section-label">Branch</div>
                <ul class="context-menu-list">
                  <li class="context-menu-item" onClick={() => openMoreView('new-branch')}>
                    <IconBranch />
                    <span>New branch…</span>
                  </li>
                  <li
                    class="context-menu-item"
                    classList={{ 'git-menu-disabled': rebaseBlocked() !== null }}
                    aria-disabled={rebaseBlocked() !== null}
                    title={
                      rebaseBlocked() ??
                      `Replay ${status()?.branch}’s commits on top of another branch`
                    }
                    onClick={() => {
                      if (rebaseBlocked() === null) openMoreView('rebase');
                    }}
                  >
                    <IconRebase />
                    <span>Rebase…</span>
                  </li>
                  <li class="context-menu-item" onClick={() => openMoreView('delete-branch')}>
                    <IconTrash />
                    <span>Delete branch…</span>
                  </li>
                </ul>
                <div class="git-menu-section-label">Stash</div>
                <ul class="context-menu-list">
                  <li
                    class="context-menu-item"
                    classList={{ 'git-menu-disabled': filesCount() === 0 }}
                    aria-disabled={filesCount() === 0}
                    title={
                      filesCount() === 0
                        ? 'Nothing to stash: there are no changes'
                        : 'Shelve the working tree on the stash'
                    }
                    onClick={() => {
                      if (filesCount() > 0) openMoreView('stash-save');
                    }}
                  >
                    <IconArchive />
                    <span>Stash changes…</span>
                  </li>
                  <li class="context-menu-item" onClick={() => openMoreView('stash-list')}>
                    <IconArchive />
                    <span>
                      Stashed changes{stashes().length > 0 ? ` (${stashes().length})` : ''}…
                    </span>
                  </li>
                  <Show when={stashes().length > 0}>
                    <li
                      class="context-menu-item"
                      title={stashes()[0]?.message || 'Restore the newest stash'}
                      onClick={() => {
                        closeMore();
                        void stashPop(0);
                      }}
                    >
                      <IconArchive />
                      <span>Pop latest stash</span>
                    </li>
                  </Show>
                </ul>
                <div class="git-menu-section-label">Changes</div>
                <ul class="context-menu-list">
                  <li
                    class="context-menu-item git-menu-danger"
                    classList={{ 'git-menu-disabled': filesCount() === 0 }}
                    aria-disabled={filesCount() === 0}
                    title={
                      filesCount() === 0
                        ? 'Nothing to discard: there are no changes'
                        : 'Throw away every change in the working tree'
                    }
                    onClick={() => void discardAll()}
                  >
                    <IconTrash />
                    <span>Discard all changes…</span>
                  </li>
                </ul>
                <div class="git-menu-section-label">Repository</div>
                <ul class="context-menu-list">
                  <li
                    class="context-menu-item"
                    title="The scripts git runs around a commit, a push, a pull…"
                    onClick={() => openMoreView('hooks')}
                  >
                    <IconHook />
                    <span>Git hooks…</span>
                  </li>
                </ul>
              </Show>

              {/* Git hooks */}
              <Show when={moreView() === 'hooks'}>
                <button class="icon-button git-more-back" onClick={() => setMoreView('menu')}>
                  <IconBack />
                  <span>Git hooks</span>
                </button>
                <GitHooksView
                  projectPath={props.source.path}
                  onOpen={(path) => props.onOpenPath?.(path)}
                />
              </Show>

              {/* New branch */}
              <Show when={moreView() === 'new-branch'}>
                <button class="icon-button git-more-back" onClick={() => setMoreView('menu')}>
                  <IconBack />
                  <span>New branch</span>
                </button>
                <div class="git-more-form">
                  <input
                    class="tree-row-input git-more-input"
                    placeholder="Branch name"
                    value={newBranchName()}
                    spellcheck={false}
                    autocomplete="off"
                    onInput={(e) => setNewBranchName(e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') void createBranch(true);
                    }}
                    disabled={waiting()}
                  />
                  <div class="git-base-field">
                    <div class="git-more-note">Create from</div>
                    {/* Each option carries its own `selected`, rather than
                        the select a `value`: the list loads after the view
                        opens, and a value set before its option exists is
                        lost, leaving the first branch shown while the
                        current one would be used. */}
                    <select
                      class="git-more-input git-base-select"
                      onChange={(e) => setNewBranchFrom(e.currentTarget.value)}
                      disabled={waiting() || branchesLoading() || branches().length === 0}
                      title="The branch the new branch starts from"
                    >
                      {/* Not on a branch in the list (detached HEAD, a rebase,
                          the list still loading): the current commit. */}
                      <Show when={!branches().some((b) => b.name === newBranchFrom())}>
                        <option value="" selected>
                          {status()?.branch || 'HEAD'}
                        </option>
                      </Show>
                      <optgroup label="Local">
                        <For each={branches().filter((b) => !b.is_remote)}>
                          {(b) => (
                            <option value={b.name} selected={b.name === newBranchFrom()}>
                              {b.name}
                            </option>
                          )}
                        </For>
                      </optgroup>
                      <Show when={branches().some((b) => b.is_remote)}>
                        <optgroup label="Remote">
                          <For each={branches().filter((b) => b.is_remote)}>
                            {(b) => (
                              <option value={b.name} selected={b.name === newBranchFrom()}>
                                {b.name}
                              </option>
                            )}
                          </For>
                        </optgroup>
                      </Show>
                    </select>
                  </div>
                  <Show when={branchNameProblem(newBranchName())}>
                    <div class="git-more-note error">{branchNameProblem(newBranchName())}</div>
                  </Show>
                  <div class="git-more-actions">
                    <button
                      class="chat-action-button"
                      disabled={
                        waiting() || !newBranchName().trim() || !!branchNameProblem(newBranchName())
                      }
                      onClick={() => void createBranch(false)}
                    >
                      Create
                    </button>
                    <button
                      class="chat-action-button primary"
                      disabled={
                        waiting() || !newBranchName().trim() || !!branchNameProblem(newBranchName())
                      }
                      onClick={() => void createBranch(true)}
                    >
                      Create & switch
                    </button>
                  </div>
                </div>
              </Show>

              {/* Rebase: onto which branch, found by searching */}
              <Show when={moreView() === 'rebase'}>
                <button class="icon-button git-more-back" onClick={() => setMoreView('menu')}>
                  <IconBack />
                  <span>Rebase {status()?.branch} onto</span>
                </button>
                {branchSearchBox(rebaseSearch, 'Search branches to rebase onto', () =>
                  setRebaseNote(null)
                )}
                <Show when={rebaseNote()}>
                  <div class="git-more-note git-rebase-note">{rebaseNote()}</div>
                </Show>
                <Show when={branchesLoading() && branches().length === 0}>
                  <div class="context-menu-item context-menu-loading">
                    <IconSpinner /> <span>Loading branches...</span>
                  </div>
                </Show>
                <Show
                  when={
                    !(branchesLoading() && branches().length === 0) &&
                    rebaseSearch.targets().length === 0
                  }
                >
                  <div class="git-more-note">
                    {rebaseSearch.query().trim()
                      ? 'No branch matches.'
                      : 'No other branches to rebase onto.'}
                  </div>
                </Show>
                {branchSection(rebaseSearch, false, { title: rebaseTitle })}
                {branchSection(rebaseSearch, true, { title: rebaseTitle })}
              </Show>

              {/* Delete branch */}
              <Show when={moreView() === 'delete-branch'}>
                <button class="icon-button git-more-back" onClick={() => setMoreView('menu')}>
                  <IconBack />
                  <span>Delete branch</span>
                </button>
                {/* As in the branch menu: over the rows it moved them, and a
                    click on one's delete button could land on the next. */}
                <Show when={branchesLoading() && branches().length === 0}>
                  <div class="context-menu-item context-menu-loading">
                    <IconSpinner /> <span>Loading branches...</span>
                  </div>
                </Show>
                <Show
                  when={
                    !branchesLoading() &&
                    branches().filter((b) => !b.is_remote && b.name !== status()?.branch).length ===
                      0
                  }
                >
                  <div class="git-more-note">No other branches to delete.</div>
                </Show>
                <ul class="context-menu-list">
                  <For each={branches().filter((b) => !b.is_remote && b.name !== status()?.branch)}>
                    {(b) => (
                      <li class="context-menu-item git-more-row">
                        <IconBranch />
                        <span class="git-more-row-label">{b.name}</span>
                        <button
                          type="button"
                          class="git-more-row-button"
                          title={`Delete branch ${b.name}`}
                          aria-label={`Delete branch ${b.name}`}
                          disabled={waiting()}
                          onClick={(e) => {
                            e.stopPropagation();
                            void deleteBranch(b.name);
                          }}
                        >
                          <IconTrash />
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>

              {/* Stash: save */}
              <Show when={moreView() === 'stash-save'}>
                <button class="icon-button git-more-back" onClick={() => setMoreView('menu')}>
                  <IconBack />
                  <span>Stash changes</span>
                </button>
                <div class="git-more-form">
                  <input
                    class="tree-row-input git-more-input"
                    placeholder="Message (optional)"
                    value={stashMessage()}
                    spellcheck={false}
                    autocomplete="off"
                    onInput={(e) => setStashMessage(e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') void stashPush();
                    }}
                    disabled={waiting()}
                  />
                  <div class="git-more-note">
                    Shelves tracked changes and new files; the tree comes back clean.
                  </div>
                  <div class="git-more-actions">
                    <button
                      class="icon-button primary"
                      disabled={waiting() || filesCount() === 0}
                      onClick={() => void stashPush()}
                    >
                      Stash changes
                    </button>
                  </div>
                </div>
              </Show>

              {/* Stash: list */}
              <Show when={moreView() === 'stash-list'}>
                <button class="icon-button git-more-back" onClick={() => setMoreView('menu')}>
                  <IconBack />
                  <span>Stashed changes</span>
                </button>
                <Show when={stashesLoading()}>
                  <div class="context-menu-item context-menu-loading">
                    <IconSpinner /> <span>Loading stashes...</span>
                  </div>
                </Show>
                <Show when={!stashesLoading() && stashes().length === 0}>
                  <div class="git-more-note">No stashes yet.</div>
                </Show>
                <ul class="context-menu-list">
                  <For each={stashes()}>
                    {(s) => (
                      <li class="git-stash-entry">
                        <div
                          class="context-menu-item git-more-row"
                          title={`${s.message}\nClick to see what this stash contains`}
                          onClick={() => void toggleStashExpanded(s.index)}
                        >
                          <span class="tree-row-chevron">
                            <ChevronIcon open={expandedStash() === s.index} />
                          </span>
                          <IconArchive />
                          <span class="git-more-row-label">{s.message}</span>
                          <button
                            type="button"
                            class="git-more-row-button"
                            title="Restore this stash and remove it from the list"
                            aria-label={`Restore stash ${s.message}`}
                            disabled={waiting()}
                            onClick={(e) => {
                              e.stopPropagation();
                              void stashPop(s.index);
                            }}
                          >
                            <span class="git-more-row-text">Pop</span>
                          </button>
                          <button
                            type="button"
                            class="git-more-row-button git-menu-danger"
                            title="Forget this stash without restoring it"
                            aria-label={`Forget stash ${s.message}`}
                            disabled={waiting()}
                            onClick={(e) => {
                              e.stopPropagation();
                              void stashDrop(s.index);
                            }}
                          >
                            <IconTrash />
                          </button>
                        </div>
                        <Show when={expandedStash() === s.index}>
                          <div class="git-stash-files">
                            <Show when={stashFilesLoading() === s.index}>
                              <div class="context-menu-item context-menu-loading">
                                <IconSpinner /> <span>Loading stash contents...</span>
                              </div>
                            </Show>
                            <Show
                              when={
                                stashFilesLoading() !== s.index &&
                                (stashFiles()[s.index] ?? []).length === 0
                              }
                            >
                              <div class="git-more-note">Empty stash.</div>
                            </Show>
                            <For each={stashFiles()[s.index] ?? []}>
                              {(f) => {
                                const meta = () => statusMeta(f.status);
                                const name = () => f.path.split('/').pop() ?? f.path;
                                const deleted = () => f.status === 'D';
                                return (
                                  <div
                                    class="context-menu-item git-stash-file-row"
                                    classList={{ clickable: !deleted() }}
                                    title={
                                      deleted()
                                        ? `${f.path} (deleted in this stash)`
                                        : `${f.path}: click to open the stashed version`
                                    }
                                    onClick={() => {
                                      // A deleted file has no content to open.
                                      if (!deleted())
                                        props.onOpenStashFile?.(
                                          absPathOf(f.path),
                                          f.path,
                                          s.index,
                                          s.message
                                        );
                                    }}
                                  >
                                    <span class="tree-row-icon">
                                      <FileIcon />
                                    </span>
                                    <span
                                      class="tree-row-name"
                                      classList={{ 'git-file-deleted': deleted() }}
                                      title={f.path}
                                    >
                                      {name()}
                                    </span>
                                    <DiffStats additions={f.additions} deletions={f.deletions} />
                                    <span
                                      class={`tree-row-badge git-file-badge ${meta().className}`}
                                      title={meta().title}
                                    >
                                      {meta().label}
                                    </span>
                                  </div>
                                );
                              }}
                            </For>
                          </div>
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </div>
          </Show>

          {/* History: the clock button left of Pull */}
          <Show when={historyOpen()}>
            <div
              class="git-branch-dropdown git-history-dropdown context-menu"
              role="dialog"
              aria-label="Recent commits"
            >
              <div class="git-menu-section-label">Recent commits</div>
              <div class="git-history-search-row">
                <div class="git-history-search-wrap">
                  <input
                    class="tree-row-input git-more-input git-history-search"
                    placeholder="Search history: message, file, symbol…"
                    value={historyQuery()}
                    spellcheck={false}
                    autocomplete="off"
                    aria-label="Search commit history"
                    onInput={(e) => onHistorySearchInput(e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Escape') {
                        e.stopPropagation();
                        onHistorySearchInput('');
                      } else if (e.key === 'Enter') {
                        // Search immediately instead of waiting for debounce.
                        e.preventDefault();
                        if (historyDebounce) clearTimeout(historyDebounce);
                        historyDebounce = undefined;
                        setHistoryOffset(0);
                        setHasMoreHistory(false);
                        void loadLog(e.currentTarget.value, 'reset');
                      }
                    }}
                    disabled={waiting()}
                  />
                  <Show when={historyQuery().trim() !== ''}>
                    <button
                      type="button"
                      class="git-history-clear"
                      title="Clear search"
                      aria-label="Clear history search"
                      onClick={() => onHistorySearchInput('')}
                    >
                      ✕
                    </button>
                  </Show>
                </div>
              </div>
              <Show when={commitsLoading()}>
                <div class="context-menu-item context-menu-loading">
                  <IconSpinner />{' '}
                  <span>
                    {historyQuery().trim() !== '' ? 'Searching history...' : 'Loading history...'}
                  </span>
                </div>
              </Show>
              <Show when={!commitsLoading() && commits().length === 0 && resultsQuery() === ''}>
                <div class="git-more-note">No commits yet.</div>
              </Show>
              <Show when={!commitsLoading() && commits().length === 0 && resultsQuery() !== ''}>
                <div class="git-more-note">
                  No matching commits for “{resultsQuery()}”. Try a file name, symbol, or word from
                  the message.
                </div>
              </Show>
              <Show when={!commitsLoading()}>
                <ul class="context-menu-list">
                  <For each={commits()}>
                    {(c) => {
                      const q = () => resultsQuery();
                      const searching = () => q() !== '';
                      const filesKey = () => `${c.oid}::${q()}`;
                      const allFiles = () => commitFiles()[filesKey()] ?? [];
                      const matchedCount = () =>
                        allFiles().filter((f) => f.matched !== false).length;
                      const showingAll = () => !!showAllFiles()[c.oid];
                      const visibleFiles = () => {
                        const all = allFiles();
                        if (!searching() || showingAll()) return all;
                        const matched = all.filter((f) => f.matched !== false);
                        // Message-only match (nothing flagged): show all,
                        // the header below explains why.
                        return matched.length > 0 ? matched : all;
                      };
                      const matchLabel = (kind?: string | null) =>
                        kind === 'both'
                          ? 'file + content'
                          : kind === 'filename'
                            ? 'filename'
                            : kind === 'content'
                              ? 'content'
                              : '';
                      return (
                        <li class="git-commit-entry">
                          <div
                            class="git-commit-row"
                            title={`${c.oid}\n${c.author} · ${new Date(c.time * 1000).toLocaleString()}\nClick to see the files this commit changed`}
                            onClick={() => void toggleCommitExpanded(c.oid)}
                          >
                            <span class="tree-row-chevron">
                              <ChevronIcon open={expandedCommit() === c.oid} />
                            </span>
                            <button
                              type="button"
                              class="git-commit-hash"
                              title={`${c.oid}\nClick to copy the full hash`}
                              aria-label={`Copy commit hash ${c.oid}`}
                              onClick={(e) => {
                                e.stopPropagation();
                                void copyHash(c.oid);
                              }}
                            >
                              {copiedHash() === c.oid ? 'Copied' : c.short}
                            </button>
                            <span class="git-commit-summary">{c.summary}</span>
                            <Show
                              when={
                                searching() &&
                                c.matched_file_count != null &&
                                (c.file_count ?? 0) > 0
                              }
                            >
                              <span
                                class="git-commit-match-badge"
                                title={
                                  (c.matched_file_count ?? 0) > 0
                                    ? `${c.matched_file_count} of ${c.file_count} files contain “${q()}”`
                                    : `Matched via message/author — no changed filename contains “${q()}”`
                                }
                              >
                                {(c.matched_file_count ?? 0) > 0
                                  ? `${c.matched_file_count}/${c.file_count} files`
                                  : 'message'}
                              </span>
                            </Show>
                          </div>
                          <Show when={expandedCommit() === c.oid}>
                            <div class="git-stash-files">
                              <Show when={commitFilesLoading() === c.oid}>
                                <div class="context-menu-item context-menu-loading">
                                  <IconSpinner /> <span>Loading commit contents...</span>
                                </div>
                              </Show>
                              <Show
                                when={commitFilesLoading() !== c.oid && allFiles().length === 0}
                              >
                                <div class="git-more-note">Empty commit.</div>
                              </Show>
                              <Show
                                when={
                                  commitFilesLoading() !== c.oid &&
                                  searching() &&
                                  allFiles().length > 0
                                }
                              >
                                <div class="git-match-summary-row">
                                  <Show
                                    when={matchedCount() > 0 && matchedCount() < allFiles().length}
                                  >
                                    <span class="git-more-note">
                                      {matchedCount()} of {allFiles().length} files match “{q()}”
                                      {showingAll() ? ' — showing all' : ''}.
                                    </span>
                                    <button
                                      type="button"
                                      class="link-button git-match-toggle"
                                      onClick={(e) => {
                                        e.stopPropagation();
                                        setShowAllFiles((prev) => ({
                                          ...prev,
                                          [c.oid]: !prev[c.oid],
                                        }));
                                      }}
                                    >
                                      {showingAll() ? 'Show matches only' : 'Show all'}
                                    </button>
                                  </Show>
                                  <Show when={matchedCount() === allFiles().length}>
                                    <span class="git-more-note">
                                      All {allFiles().length} files match “{q()}”.
                                    </span>
                                  </Show>
                                  <Show when={matchedCount() === 0}>
                                    <span class="git-more-note">
                                      Matched via message — no changed file contains “{q()}”.
                                      Showing all {allFiles().length} files.
                                    </span>
                                  </Show>
                                </div>
                              </Show>
                              <For each={visibleFiles()}>
                                {(f) => {
                                  const meta = () => statusMeta(f.status);
                                  const name = () => f.path.split('/').pop() ?? f.path;
                                  const deleted = () => f.status === 'D';
                                  const isMatch = () => f.matched !== false;
                                  return (
                                    <div
                                      class="context-menu-item git-stash-file-row"
                                      classList={{
                                        clickable: !deleted(),
                                        'git-file-unmatched':
                                          searching() && !isMatch() && showingAll(),
                                        'git-file-matched': searching() && isMatch(),
                                      }}
                                      title={
                                        deleted()
                                          ? `${f.path} (deleted in this commit)`
                                          : searching() && isMatch() && f.match_kind
                                            ? `${f.path} (matches via ${matchLabel(f.match_kind)}): click to open the version at this commit`
                                            : `${f.path}: click to open the version at this commit`
                                      }
                                      onClick={() => {
                                        // A deleted file has no content to open.
                                        if (!deleted())
                                          props.onOpenCommitFile?.(
                                            absPathOf(f.path),
                                            f.path,
                                            c.oid,
                                            c.summary
                                          );
                                      }}
                                    >
                                      <span class="tree-row-icon">
                                        <FileIcon />
                                      </span>
                                      <span
                                        class="tree-row-name"
                                        classList={{ 'git-file-deleted': deleted() }}
                                        title={f.path}
                                      >
                                        {name()}
                                      </span>
                                      <Show when={searching() && isMatch() && f.match_kind}>
                                        <span
                                          class="git-match-kind"
                                          title={`Matches via ${matchLabel(f.match_kind)}`}
                                        >
                                          {f.match_kind === 'both' ? 'file+content' : f.match_kind}
                                        </span>
                                      </Show>
                                      <DiffStats additions={f.additions} deletions={f.deletions} />
                                      <span
                                        class={`tree-row-badge git-file-badge ${meta().className}`}
                                        title={meta().title}
                                      >
                                        {meta().label}
                                      </span>
                                    </div>
                                  );
                                }}
                              </For>
                            </div>
                          </Show>
                        </li>
                      );
                    }}
                  </For>
                </ul>
              </Show>
              {/* Pagination: the backend scans at most ~400 commits per
                  call, so older history needs explicit paging. Without a
                  query this loads older commits; with a query it keeps
                  searching further back. */}
              <Show when={!commitsLoading() && commits().length > 0 && hasMoreHistory()}>
                <button
                  type="button"
                  class="link-button git-match-toggle"
                  style={{ margin: '4px 8px 8px' }}
                  title={
                    resultsQuery() !== ''
                      ? 'Keep searching older commits (next ~400)'
                      : 'Load older commits'
                  }
                  disabled={historyLoadingMore()}
                  onClick={loadMoreHistory}
                >
                  {historyLoadingMore()
                    ? 'Loading…'
                    : resultsQuery() !== ''
                      ? 'Search further back…'
                      : 'Load more…'}
                </button>
              </Show>
              <Show when={!commitsLoading() && commits().length > 0 && !hasMoreHistory()}>
                <div class="git-more-note">
                  {resultsQuery() !== ''
                    ? 'Searched the full history.'
                    : 'Reached the oldest commit.'}
                </div>
              </Show>
            </div>
          </Show>
        </div>

        <Show when={operation()}>
          {(op) => (
            <div class="git-operation-banner" role="status">
              <IconAlert />
              <div class="git-operation-text">
                <strong>
                  {op().kind === 'rebase'
                    ? `Rebasing${op().step && op().total ? ` ${op().step}/${op().total}` : ''}${op().incoming ? ` onto ${op().incoming}` : ''}`
                    : op().kind === 'merge'
                      ? `Merging${op().incoming ? ` ${op().incoming}` : ''}`
                      : `${op().kind[0].toUpperCase()}${op().kind.slice(1)} in progress`}
                </strong>
                <span>
                  {op().conflicts.length > 0
                    ? `${op().conflicts.length} conflicted file${op().conflicts.length === 1 ? '' : 's'}`
                    : op().canContinue
                      ? 'Conflicts resolved: ready to finish'
                      : 'Finish it from a terminal, or abort'}
                  {pendingPush() ? ' · push follows' : ''}
                </span>
              </div>
              <div class="git-operation-actions">
                <button
                  type="button"
                  classList={{ primary: op().conflicts.length > 0 }}
                  disabled={waiting()}
                  onClick={() => setResolver({})}
                >
                  {op().conflicts.length > 0 ? 'Resolve…' : 'Review…'}
                </button>
                <Show when={op().conflicts.length === 0 && op().canContinue}>
                  <button
                    type="button"
                    class="primary"
                    disabled={waiting()}
                    onClick={() => void withBusy(() => continueOperation())}
                  >
                    {op().kind === 'merge' ? 'Commit merge' : 'Continue'}
                  </button>
                </Show>
                <button type="button" disabled={waiting()} onClick={() => void confirmAbort()}>
                  Abort
                </button>
              </div>
            </div>
          )}
        </Show>

        <Show when={errorMsg()}>
          {(error) => <GitInlineError error={error()} offer={skipOffer()} disabled={waiting()} />}
        </Show>

        {/* Uncommitted files */}
        <Show when={filesCount() === 0}>
          <div class="empty-state">No uncommitted changes</div>
        </Show>

        <Show when={filesCount() > 0}>
          <div
            class="git-files file-tree"
            // Once it's in the page: a ref comes as the element is made,
            // and measured then it has no size (and no rows were drawn).
            ref={(el) => queueMicrotask(() => setGitFilesScrollEl(el))}
          >
            <div
              style={{
                height: `${gitFilesVirtualizer.getTotalSize()}px`,
                position: 'relative',
              }}
            >
              <For each={gitFilesVirtualizer.getVirtualItems()}>
                {(item) => {
                  const rowStyle = (depth: number): JSX.CSSProperties => ({
                    position: 'absolute',
                    top: 0,
                    left: 0,
                    width: '100%',
                    height: `${GIT_ROW_HEIGHT}px`,
                    transform: `translateY(${item.start}px)`,
                    'padding-left': `${8 + depth * 14}px`,
                    'user-select': 'none',
                    '-webkit-user-select': 'none',
                  });

                  // The rows outlive a status refresh by a tick: when the list
                  // shrinks (a revert, a commit) the virtualizer is recreated
                  // only after the rows have re-read status, so an index can
                  // briefly point past the end. Render nothing for it then.
                  const row = () => treeRows()[item.index];

                  return (
                    <Show when={row()}>
                      {(r) => (
                        <Switch>
                          <Match when={asDirRow(r())}>
                            {(dirRow) => {
                              const dir = () => dirRow().node;
                              const open = () => !collapsed().has(dir().path);
                              return (
                                <div
                                  data-index={item.index}
                                  class="tree-row dir git-dir-row clickable"
                                  title={dir().path}
                                  onClick={() => toggleDir(dir().path)}
                                  style={rowStyle(dirRow().depth)}
                                >
                                  <span class="tree-row-chevron">
                                    <ChevronIcon open={open()} />
                                  </span>
                                  <span class="tree-row-icon">
                                    <FolderIcon open={open()} />
                                  </span>
                                  <span class="tree-row-name">{dirRow().name}</span>
                                  <Show when={revertable(dir()).length > 0}>
                                    <button
                                      type="button"
                                      class="git-revert-button"
                                      title="Revert every changed file in this folder"
                                      aria-label={`Revert ${dir().path}`}
                                      disabled={waiting()}
                                      onClick={(e) => {
                                        e.stopPropagation();
                                        void revertFolder(dir());
                                      }}
                                    >
                                      <RevertIcon />
                                    </button>
                                  </Show>
                                  <DiffStats
                                    additions={dir().additions}
                                    deletions={dir().deletions}
                                  />
                                  <span
                                    class="git-dir-count"
                                    title={`${dir().count} changed file${dir().count === 1 ? '' : 's'}`}
                                  >
                                    {dir().count}
                                  </span>
                                </div>
                              );
                            }}
                          </Match>
                          <Match when={asFileRow(r())}>
                            {(fileRow) => {
                              const file = () => fileRow().file;
                              const meta = () => statusMeta(file().status);

                              return (
                                <div
                                  data-index={item.index}
                                  class="tree-row git-file-row"
                                  classList={{ clickable: file().status !== 'D' }}
                                  title={
                                    file().status === 'U'
                                      ? `${file().path}: conflicted, click to resolve`
                                      : file().status === 'D'
                                        ? `${file().path} (deleted)`
                                        : `${file().path}: click to open with its changes marked`
                                  }
                                  onClick={() => {
                                    if (file().status === 'U') {
                                      if (operation()) setResolver({ file: file().path });
                                      else props.onOpenFile?.(absPathOf(file().path), file().path);
                                      return;
                                    }
                                    // A deleted file has nothing to open; revert brings it back.
                                    if (file().status !== 'D')
                                      props.onOpenFile?.(absPathOf(file().path), file().path);
                                  }}
                                  style={rowStyle(fileRow().depth)}
                                >
                                  <span class="tree-row-chevron" />
                                  <span class="tree-row-icon">
                                    <FileIcon />
                                  </span>
                                  <span
                                    class={`tree-row-name ${meta().className}`}
                                    classList={{ 'git-file-deleted': file().status === 'D' }}
                                  >
                                    {fileRow().name}
                                  </span>
                                  <Show when={file().status !== 'U' && file().status !== 'R'}>
                                    <button
                                      type="button"
                                      class="git-revert-button"
                                      title={
                                        file().status === '?' || file().status === 'A'
                                          ? 'Remove this new file (to the trash)'
                                          : 'Revert to the last committed version'
                                      }
                                      aria-label={`Revert ${file().path}`}
                                      disabled={waiting()}
                                      onClick={(e) => {
                                        e.stopPropagation();
                                        void revertFile(file());
                                      }}
                                    >
                                      <RevertIcon />
                                    </button>
                                  </Show>
                                  <DiffStats
                                    additions={file().additions}
                                    deletions={file().deletions}
                                  />
                                  <span
                                    class={`tree-row-badge git-file-badge ${meta().className}`}
                                    title={meta().title}
                                  >
                                    {meta().label}
                                  </span>
                                </div>
                              );
                            }}
                          </Match>
                        </Switch>
                      )}
                    </Show>
                  );
                }}
              </For>
            </div>
          </div>
        </Show>
      </Show>

      <Show when={resolver() && operation()}>
        <ConflictResolver
          projectPath={props.source.path}
          operation={operation()!}
          initialFile={resolver()?.file}
          onFileWritten={(file) => {
            props.onFileReverted?.(absPathOf(file));
            void refreshStatus();
          }}
          onContinue={continueOperation}
          onAbort={abortOperation}
          onClose={() => setResolver(null)}
        />
      </Show>
      <Show when={strategyAsk()}>
        {(ask) => (
          <PullStrategyDialog
            reason={ask().reason}
            onPick={(pick) => answerStrategy(pick)}
            onCancel={() => answerStrategy(null)}
          />
        )}
      </Show>
    </div>
  );
}
