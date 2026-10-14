// GitPanel.tsx
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
  type JSX,
} from 'solid-js';
import { listen } from '@tauri-apps/api/event';
import { createVirtualizer, Virtualizer } from '@tanstack/solid-virtual';
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
import { ConflictResolver } from './ConflictResolver';
import { PullStrategyDialog, type PullStrategyPick } from './PullStrategyDialog';
import { ChevronIcon, FileIcon, FolderIcon } from './FileIcons';

const GIT_ROW_HEIGHT = 22;

interface GitPanelProps {
  /** Absolute path to the currently open project/workspace. */
  projectPath: string;
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
}

// ---------------------------------------------------------------------------
// Icons (inline SVG, sized via currentColor so they inherit button/text color)
// ---------------------------------------------------------------------------

const iconProps = {
  width: 14,
  height: 14,
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  'stroke-width': 2,
  'stroke-linecap': 'round' as const,
  'stroke-linejoin': 'round' as const,
};

function IconBranch() {
  return (
    <svg {...iconProps}>
      <line x1="6" y1="3" x2="6" y2="15" />
      <circle cx="18" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M18 9a9 9 0 0 1-9 9" />
    </svg>
  );
}

function IconChevronDown() {
  return (
    <svg {...iconProps} width={10} height={10}>
      <polyline points="6 9 12 15 18 9" />
    </svg>
  );
}

function IconRefresh() {
  return (
    <svg {...iconProps}>
      <polyline points="23 4 23 10 17 10" />
      <polyline points="1 20 1 14 7 14" />
      <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
    </svg>
  );
}

function IconArrowDown() {
  return (
    <svg {...iconProps}>
      <line x1="12" y1="5" x2="12" y2="19" />
      <polyline points="19 12 12 19 5 12" />
    </svg>
  );
}

function IconArrowUp() {
  return (
    <svg {...iconProps}>
      <line x1="12" y1="19" x2="12" y2="5" />
      <polyline points="5 12 12 5 19 12" />
    </svg>
  );
}

function IconCheck() {
  return (
    <svg {...iconProps}>
      <polyline points="20 6 9 17 4 12" />
    </svg>
  );
}

function IconAlert() {
  return (
    <svg {...iconProps}>
      <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
      <line x1="12" y1="9" x2="12" y2="13" />
      <line x1="12" y1="17" x2="12.01" y2="17" />
    </svg>
  );
}

function IconFolderGit() {
  return (
    <svg {...iconProps} width={28} height={28}>
      <path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9L9.34 3.9A2 2 0 0 0 7.68 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z" />
      <circle cx="12" cy="13" r="1.6" />
    </svg>
  );
}

function IconSpinner(props: { size?: number }) {
  return (
    <svg {...iconProps} width={props.size ?? 14} height={props.size ?? 14} class="git-spin">
      <path d="M21 12a9 9 0 1 1-9-9" />
    </svg>
  );
}

function IconDots() {
  return (
    <svg {...iconProps}>
      <circle cx="12" cy="5" r="1" />
      <circle cx="12" cy="12" r="1" />
      <circle cx="12" cy="19" r="1" />
    </svg>
  );
}

function IconBack() {
  return (
    <svg {...iconProps} width={12} height={12}>
      <polyline points="15 18 9 12 15 6" />
    </svg>
  );
}

function IconArchive() {
  return (
    <svg {...iconProps}>
      <rect x="2" y="3" width="20" height="5" rx="1" />
      <path d="M4 8v11a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V8" />
      <path d="M10 12h4" />
    </svg>
  );
}

function IconHistory() {
  return (
    <svg {...iconProps}>
      <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
      <path d="M3 3v5h5" />
      <path d="M12 7v5l3 2" />
    </svg>
  );
}

function IconTrash() {
  return (
    <svg {...iconProps}>
      <path d="M3 6h18" />
      <path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2" />
      <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
    </svg>
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

/** `+3 −3` line counts, green for additions and red for deletions. */
function DiffStats(props: { additions?: number; deletions?: number }) {
  const additions = props.additions ?? 0;
  const deletions = props.deletions ?? 0;
  if (additions <= 0 && deletions <= 0) return null;
  const title =
    additions > 0 && deletions > 0
      ? `${additions} added line${additions === 1 ? '' : 's'}, ${deletions} removed`
      : additions > 0
        ? `${additions} added line${additions === 1 ? '' : 's'}`
        : `${deletions} removed line${deletions === 1 ? '' : 's'}`;
  return (
    <span class="git-diff-stats" title={title}>
      {additions > 0 && <span class="git-diff-add">+{additions}</span>}
      {deletions > 0 && <span class="git-diff-del">−{deletions}</span>}
    </span>
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

export function GitPanel(props: GitPanelProps) {
  //let gitFilesScrollEl: HTMLDivElement | undefined;
  const [gitFilesScrollEl, setGitFilesScrollEl] = createSignal<HTMLDivElement | null>(null);
  const [gitFilesVirtualizer, setGitFilesVirtualizer] = createSignal<Virtualizer<
    HTMLDivElement,
    Element
  > | null>(null);

  const [phase, setPhase] = createSignal<GitPanelPhase>('uninitialized');
  const [status, setStatus] = createSignal<GitStatus | null>(null);
  const [errorMsg, setErrorMsg] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const [commitMessage, setCommitMessage] = createSignal('');
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
      const s = await invoke<GitStatus>('git_status', { path: props.projectPath });
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
    const root = status()?.workdir || props.projectPath;
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
      await invoke('git_revert_file', { path: props.projectPath, file: file.path });
      props.onFileReverted?.(absPathOf(file.path));
      await refreshStatus();
    });
  };

  const initRepo = () =>
    withBusy(async () => {
      await invoke('git_init', { path: props.projectPath });
      await refreshStatus();
    });

  const fetchGit = () =>
    withBusy(async () => {
      await invoke('git_fetch', { path: props.projectPath });
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
    let out = await invoke<GitPullOutcome>('git_pull', {
      path: props.projectPath,
      strategy: pick?.strategy,
      remember: pick?.remember ?? false,
    });
    if (out.result === 'needs-strategy') {
      const asked = await askStrategy(
        'Your branch and its upstream have diverged, and git config (pull.rebase) doesn’t say how to combine them.'
      );
      if (!asked) return 'cancelled';
      out = await invoke<GitPullOutcome>('git_pull', {
        path: props.projectPath,
        strategy: asked.strategy,
        remember: asked.remember,
      });
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

  /** Pushes; a push the remote turns down (it has new commits) offers to
   *  pull them in first, the way git config says, then push again. */
  const push = async () => {
    const out = await invoke<GitPushOutcome>('git_push', { path: props.projectPath });
    if (out.result === 'pushed') {
      setPendingPush(false);
      return;
    }
    const cfg = await invoke<GitPullConfig>('git_pull_config', { path: props.projectPath });
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
    const again = await invoke<GitPushOutcome>('git_push', { path: props.projectPath });
    if (again.result === 'rejected') {
      throw new Error('The remote moved again while pulling. Push once more.');
    }
  };

  /** Like withBusy, but the error goes back to the caller (the resolver shows it). */
  const busyOrThrow = async (fn: () => Promise<void>) => {
    setBusy(true);
    setErrorMsg(null);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  };

  const continueOperation = (message?: string) =>
    busyOrThrow(async () => {
      const out = await invoke<GitPullOutcome>('git_operation_continue', {
        path: props.projectPath,
        message,
      });
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
      await invoke('git_operation_abort', { path: props.projectPath });
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

  const commitAndMaybePush = () =>
    withBusy(async () => {
      const st = status();
      if (!st) return;
      const msg = commitMessage().trim();
      const hasChanges = st.files.length > 0;

      // Retry / push-only: no message but commits waiting to be pushed
      // (e.g. a previous push failed with an SSH/network error). There is
      // nothing to commit, so just push.
      if (!msg || !hasChanges) {
        if (st.ahead <= 0) return;
        try {
          await push();
        } finally {
          await refreshStatus();
          refreshHistoryIfOpen();
        }
        return;
      }

      await invoke('git_commit', { path: props.projectPath, message: msg });
      setCommitMessage('');

      const updated = await refreshStatus();
      try {
        if (updated && updated.ahead > 0) await push();
      } finally {
        await refreshStatus();
        refreshHistoryIfOpen();
      }
    });

  const switchBranch = (branch: string) =>
    withBusy(async () => {
      await invoke('git_checkout', { path: props.projectPath, branch });
      setBranchOpen(false);
      await refreshStatus();
      refreshHistoryIfOpen();
    });

  const loadBranches = async () => {
    setBranchesLoading(true);
    try {
      const list = await invoke<GitBranch[]>('git_branches', { path: props.projectPath });
      setBranches(list);
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

  type MoreView = 'menu' | 'new-branch' | 'delete-branch' | 'stash-save' | 'stash-list';

  const [moreOpen, setMoreOpen] = createSignal(false);
  const [moreView, setMoreView] = createSignal<MoreView>('menu');
  const [newBranchName, setNewBranchName] = createSignal('');
  // Which branch a new branch starts from. Defaults to the current branch
  // when the view opens; only a name that's actually in the branch list is
  // sent (anything else — detached HEAD, still loading — means HEAD).
  const [newBranchFrom, setNewBranchFrom] = createSignal('');
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
    if (view === 'delete-branch') void loadBranches();
    if (view === 'stash-list') void loadStashes();
  };

  const loadStashes = async () => {
    setStashesLoading(true);
    try {
      const list = await invoke<GitStashEntry[]>('git_stash_list', { path: props.projectPath });
      setStashes(list);
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
        path: props.projectPath,
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
        path: props.projectPath,
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
        path: props.projectPath,
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

  /** The obvious mistakes, caught inline; the backend re-checks for real. */
  const branchNameProblem = (name: string): string | null => {
    const n = name.trim();
    if (!n) return null;
    if (
      n === 'HEAD' ||
      n === '@' ||
      n.startsWith('-') ||
      n.startsWith('/') ||
      n.endsWith('/') ||
      n.endsWith('.') ||
      n.includes('..') ||
      n.includes('//') ||
      n.includes('@{') ||
      // No path component may start with "." or end with ".lock".
      n.split('/').some((part) => part.startsWith('.') || part.endsWith('.lock')) ||
      /[\s~^:?*\[\\\x00-\x1f\x7f]/.test(n)
    ) {
      return `'${n}' isn't a valid branch name.`;
    }
    return null;
  };

  const createBranch = (checkout: boolean) =>
    withBusy(async () => {
      const name = newBranchName().trim();
      if (!name || branchNameProblem(name)) return;
      // Only a branch that's actually in the list is sent as the starting
      // point; anything else (detached HEAD, list still loading) means HEAD.
      const picked = newBranchFrom();
      const from = picked && branches().some((b) => b.name === picked) ? picked : null;
      await invoke('git_create_branch', { path: props.projectPath, name, checkout, from });
      setNewBranchName('');
      closeMore();
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
      await invoke('git_delete_branch', { path: props.projectPath, name });
      await loadBranches();
      await refreshStatus();
    });
  };

  const stashPush = () =>
    withBusy(async () => {
      if (filesCount() === 0) return;
      await invoke('git_stash_push', {
        path: props.projectPath,
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
      await invoke('git_stash_pop', { path: props.projectPath, index });
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
      await invoke('git_stash_drop', { path: props.projectPath, index });
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
      await invoke('git_discard_all', { path: props.projectPath });
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
    if (next) loadBranches();
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
      if (moreView() !== 'stash-list') closeMore();
    }
  };
  window.addEventListener('pointerdown', onOutsideHeader, true);
  onCleanup(() => window.removeEventListener('pointerdown', onOutsideHeader, true));

  // Derived, reactive values (memoized so they actually update on signal
  // changes — reading a signal into a plain `const` outside of a memo/JSX
  // only captures its value once).
  // Push-only retry must stay enabled with an empty message: after a commit
  // succeeds but its auto-push fails (e.g. SSH/network error), `ahead > 0`
  // with a clean tree and a cleared message is exactly the state we are in —
  // requiring a message here would leave the Push button disabled forever,
  // even after a restart.
  const canCommit = createMemo(() => {
    const s = status();
    if (!s || busy()) return false;
    // A merge / rebase in progress is finished from its banner, not here.
    if (s.operation) return false;
    if (s.ahead > 0) return true;
    return !!commitMessage().trim() && s.files.length > 0;
  });
  const commitButtonText = createMemo(() => {
    const s = status();
    return s && s.ahead > 0 ? 'Push' : 'Commit';
  });

  /*
   * Git's own activity (a commit, a checkout, also from a terminal) and
   * edits to the project's files: the watcher reports them once per burst
   * ("git-changed"), and the status is read again unless an operation of
   * this panel is running anyway. At most once per GIT_REFRESH_MIN_MS, the
   * last one after the changes stop, and never two at once: dev servers
   * rewriting files kept `git status` of a whole workspace running.
   */
  const GIT_REFRESH_MIN_MS = 2_000;
  onMount(() => {
    let stop: (() => void) | undefined;
    let disposed = false;
    let lastRefresh = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let running = false;
    let again = false;
    const requestRefresh = () => {
      if (timer || disposed) return; // one is coming
      const wait = Math.max(0, lastRefresh + GIT_REFRESH_MIN_MS - Date.now());
      timer = setTimeout(async () => {
        timer = undefined;
        if (running) {
          again = true;
          return;
        }
        if (busy() || phase() !== 'ready') return;
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
    void listen('git-changed', requestRefresh).then((unlisten) => {
      if (disposed) unlisten();
      else stop = unlisten;
    });
    onCleanup(() => {
      disposed = true;
      clearTimeout(timer);
      stop?.();
    });
  });

  // Refresh when a different project is opened while the Git tab stays
  // mounted (onMount alone never re-runs on project switch).
  createEffect(
    on(
      () => props.projectPath,
      () => {
        void withBusy(refreshStatus);
      },
      { defer: true }
    )
  );

  onMount(() => {
    void withBusy(refreshStatus);
  });

  // Recreate virtualizer whenever count or scroll element changes
  createEffect(() => {
    const scrollEl = gitFilesScrollEl();

    if (!scrollEl) {
      setGitFilesVirtualizer(null);
      return;
    }

    const count = treeRows().length;
    const v = createVirtualizer({
      count,
      getScrollElement: () => scrollEl,
      estimateSize: () => GIT_ROW_HEIGHT,
      overscan: 12,
    });

    setGitFilesVirtualizer(v);
  });

  return (
    <div class="git-panel">
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
          <button class="git-init-button primary icon-button" onClick={initRepo} disabled={busy()}>
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
          <button class="icon-button" onClick={() => withBusy(refreshStatus)} disabled={busy()}>
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
              disabled={busy()}
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
              <button class="icon-button" onClick={fetchGit} title="Fetch" disabled={busy()}>
                <Show when={busy()} fallback={<IconRefresh />}>
                  <IconSpinner />
                </Show>
              </button>
              <button
                class="icon-button"
                onClick={toggleHistory}
                title="Recent commits"
                aria-label="Recent commits"
                aria-haspopup="dialog"
                aria-expanded={historyOpen()}
                disabled={busy()}
              >
                <IconHistory />
              </button>
              <button class="icon-button" onClick={pullGit} title="Pull" disabled={busy()}>
                <IconArrowDown />
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
            disabled={busy()}
          >
            <IconDots />
          </button>

          <Show when={branchOpen()}>
            <div class="git-branch-dropdown context-menu">
              <Show when={branchesLoading()}>
                <div class="context-menu-item context-menu-loading">
                  <IconSpinner /> <span>Loading branches...</span>
                </div>
              </Show>
              <Show when={branches().filter((b) => !b.is_remote).length > 0}>
                <div class="git-menu-section-label">Local</div>
                <ul class="context-menu-list">
                  <For each={branches().filter((b) => !b.is_remote)}>
                    {(b) => (
                      <li
                        class="context-menu-item"
                        classList={{ 'git-branch-item-active': b.name === status()?.branch }}
                        onClick={() => switchBranch(b.name)}
                      >
                        <IconBranch />
                        <span>{b.name}</span>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
              <Show when={branches().filter((b) => b.is_remote).length > 0}>
                <div class="git-menu-section-label">Remote</div>
                <ul class="context-menu-list">
                  <For each={branches().filter((b) => b.is_remote)}>
                    {(b) => (
                      <li
                        class="context-menu-item"
                        title={`Check out as a local tracking branch`}
                        onClick={() => switchBranch(b.name)}
                      >
                        <IconBranch />
                        <span>{b.name}</span>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
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
                    disabled={busy()}
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
                      disabled={busy() || branchesLoading() || branches().length === 0}
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
                        busy() || !newBranchName().trim() || !!branchNameProblem(newBranchName())
                      }
                      onClick={() => void createBranch(false)}
                    >
                      Create
                    </button>
                    <button
                      class="chat-action-button primary"
                      disabled={
                        busy() || !newBranchName().trim() || !!branchNameProblem(newBranchName())
                      }
                      onClick={() => void createBranch(true)}
                    >
                      Create & switch
                    </button>
                  </div>
                </div>
              </Show>

              {/* Delete branch */}
              <Show when={moreView() === 'delete-branch'}>
                <button class="icon-button git-more-back" onClick={() => setMoreView('menu')}>
                  <IconBack />
                  <span>Delete branch</span>
                </button>
                <Show when={branchesLoading()}>
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
                          disabled={busy()}
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
                    disabled={busy()}
                  />
                  <div class="git-more-note">
                    Shelves tracked changes and new files; the tree comes back clean.
                  </div>
                  <div class="git-more-actions">
                    <button
                      class="icon-button primary"
                      disabled={busy() || filesCount() === 0}
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
                            disabled={busy()}
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
                            disabled={busy()}
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
                    disabled={busy()}
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
                  disabled={busy()}
                  onClick={() => setResolver({})}
                >
                  {op().conflicts.length > 0 ? 'Resolve…' : 'Review…'}
                </button>
                <Show when={op().conflicts.length === 0 && op().canContinue}>
                  <button
                    type="button"
                    class="primary"
                    disabled={busy()}
                    onClick={() => void withBusy(() => continueOperation())}
                  >
                    {op().kind === 'merge' ? 'Commit merge' : 'Continue'}
                  </button>
                </Show>
                <button type="button" disabled={busy()} onClick={() => void confirmAbort()}>
                  Abort
                </button>
              </div>
            </div>
          )}
        </Show>

        <Show when={errorMsg()}>
          <div class="git-inline-error">
            <IconAlert />
            <span>{errorMsg()}</span>
          </div>
        </Show>

        {/* Uncommitted files */}
        <Show when={filesCount() === 0}>
          <div class="empty-state">No uncommitted changes</div>
        </Show>

        <Show when={filesCount() > 0}>
          <div class="git-files file-tree" ref={setGitFilesScrollEl}>
            <div
              style={{
                height: `${gitFilesVirtualizer()?.getTotalSize() ?? 0}px`,
                position: 'relative',
              }}
            >
              <For each={gitFilesVirtualizer()?.getVirtualItems() ?? []}>
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
                                      disabled={busy()}
                                      onClick={(e) => {
                                        e.stopPropagation();
                                        void revertFile(file());
                                      }}
                                    >
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

        {/* Commit controls */}
        <div class="git-commit-bar">
          <input
            class="tree-row-input git-commit-input"
            placeholder={
              operation() ? `Finish or abort the ${operation()!.kind} first` : 'Commit message'
            }
            value={commitMessage()}
            onInput={(e) => setCommitMessage(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                commitAndMaybePush();
              }
            }}
            disabled={busy() || !!operation()}
          />
          <button
            class="git-commit-button primary icon-button"
            disabled={!canCommit()}
            onClick={commitAndMaybePush}
          >
            <Show when={!busy()} fallback={<IconSpinner />}>
              <Show when={commitButtonText() === 'Push'} fallback={<IconCheck />}>
                <IconArrowUp />
              </Show>
            </Show>
            <span>{commitButtonText()}</span>
          </button>
        </div>
      </Show>

      <Show when={resolver() && operation()}>
        <ConflictResolver
          projectPath={props.projectPath}
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
