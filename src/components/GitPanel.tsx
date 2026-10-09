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
} from '../types';
import { confirmDialog } from './ConfirmDialog';
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
  const [branches, setBranches] = createSignal<string[]>([]);
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

  const pullGit = () =>
    withBusy(async () => {
      await invoke('git_pull', { path: props.projectPath });
      await refreshStatus();
    });

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
        await invoke('git_push', { path: props.projectPath });
        await refreshStatus();
        return;
      }

      await invoke('git_commit', { path: props.projectPath, message: msg });
      setCommitMessage('');

      const updated = await refreshStatus();
      if (updated && updated.ahead > 0) {
        await invoke('git_push', { path: props.projectPath });
        await refreshStatus();
      }
    });

  const switchBranch = (branch: string) =>
    withBusy(async () => {
      await invoke('git_checkout', { path: props.projectPath, branch });
      setBranchOpen(false);
      await refreshStatus();
    });

  const loadBranches = async () => {
    setBranchesLoading(true);
    try {
      const list = await invoke<string[]>('git_branches', { path: props.projectPath });
      setBranches(list);
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBranchesLoading(false);
    }
  };

  // -------------------------------------------------------------------------
  // "More actions" menu: the git operations the panel has no dedicated
  // control for (create/delete branch, stash, discard-all, history).
  // One dropdown with small inline sub-views, so no new components are
  // needed. What the panel already does — commit, push, fetch, pull,
  // switching branches, reverting one file — stays where it is.
  // -------------------------------------------------------------------------

  type MoreView = 'menu' | 'new-branch' | 'delete-branch' | 'stash-save' | 'stash-list' | 'history';

  const [moreOpen, setMoreOpen] = createSignal(false);
  const [moreView, setMoreView] = createSignal<MoreView>('menu');
  const [newBranchName, setNewBranchName] = createSignal('');
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
  const [commits, setCommits] = createSignal<GitCommitEntry[]>([]);
  const [commitsLoading, setCommitsLoading] = createSignal(false);
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
    if (next) {
      setMoreView('menu');
      void loadStashes();
    }
  };

  const openMoreView = (view: MoreView) => {
    setMoreView(view);
    if (view === 'delete-branch') void loadBranches();
    if (view === 'stash-list') void loadStashes();
    if (view === 'history') void loadLog();
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

  /** Expand a commit to show the files it changed (`git_commit_show`). */
  const toggleCommitExpanded = async (oid: string) => {
    if (expandedCommit() === oid) {
      setExpandedCommit(null);
      return;
    }
    setExpandedCommit(oid);
    if (commitFiles()[oid]) return;
    setCommitFilesLoading(oid);
    try {
      const files = await invoke<GitStashFile[]>('git_commit_show', {
        path: props.projectPath,
        oid,
      });
      setCommitFiles((prev) => ({ ...prev, [oid]: files }));
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e));
      setExpandedCommit(null);
    } finally {
      setCommitFilesLoading(null);
    }
  };

  const loadLog = async () => {
    setCommitsLoading(true);
    try {
      const list = await invoke<GitCommitEntry[]>('git_log', {
        path: props.projectPath,
        limit: 20,
      });
      setCommits(list);
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setCommitsLoading(false);
    }
  };

  /** The obvious mistakes, caught inline; the backend re-checks for real. */
  const branchNameProblem = (name: string): string | null => {
    const n = name.trim();
    if (!n) return null;
    if (
      n === 'HEAD' ||
      n.startsWith('-') ||
      n.startsWith('/') ||
      n.endsWith('/') ||
      n.endsWith('.') ||
      n.endsWith('.lock') ||
      n.includes('..') ||
      n.includes('//') ||
      n.includes('@{') ||
      /[\s~^:?*\[\\]/.test(n)
    ) {
      return `'${n}' isn't a valid branch name.`;
    }
    return null;
  };

  const createBranch = (checkout: boolean) =>
    withBusy(async () => {
      const name = newBranchName().trim();
      if (!name || branchNameProblem(name)) return;
      await invoke('git_create_branch', { path: props.projectPath, name, checkout });
      setNewBranchName('');
      closeMore();
      await refreshStatus();
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
    if (next) loadBranches();
  };

  // Close header dropdowns on a click outside the header (same pattern
  // as the project switcher / effort menu). The triggers and menus all live
  // inside the header, so anything outside it dismisses them — except the
  // stash list and history: opening a file moves focus to the editor, and
  // the menu must stay open so more files can be opened.
  let headerRoot: HTMLDivElement | undefined;
  const onOutsideHeader = (e: PointerEvent) => {
    if (headerRoot && !headerRoot.contains(e.target as Node)) {
      setBranchOpen(false);
      if (moreView() !== 'stash-list' && moreView() !== 'history') closeMore();
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
              <ul class="context-menu-list">
                <For each={branches()}>
                  {(b) => (
                    <li
                      class="context-menu-item"
                      classList={{ 'git-branch-item-active': b === status()?.branch }}
                      onClick={() => switchBranch(b)}
                    >
                      <IconBranch />
                      <span>{b}</span>
                    </li>
                  )}
                </For>
              </ul>
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
                <div class="git-menu-section-label">History</div>
                <ul class="context-menu-list">
                  <li class="context-menu-item" onClick={() => openMoreView('history')}>
                    <IconHistory />
                    <span>Recent commits…</span>
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
                  <Show when={branchNameProblem(newBranchName())}>
                    <div class="git-more-note error">{branchNameProblem(newBranchName())}</div>
                  </Show>
                  <div class="git-more-actions">
                    <button
                      class="icon-button"
                      disabled={
                        busy() || !newBranchName().trim() || !!branchNameProblem(newBranchName())
                      }
                      onClick={() => void createBranch(false)}
                    >
                      Create
                    </button>
                    <button
                      class="icon-button primary"
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
                    branches().filter((b) => b !== status()?.branch).length === 0
                  }
                >
                  <div class="git-more-note">No other branches to delete.</div>
                </Show>
                <ul class="context-menu-list">
                  <For each={branches().filter((b) => b !== status()?.branch)}>
                    {(b) => (
                      <li class="context-menu-item git-more-row">
                        <IconBranch />
                        <span class="git-more-row-label">{b}</span>
                        <button
                          type="button"
                          class="git-more-row-button"
                          title={`Delete branch ${b}`}
                          aria-label={`Delete branch ${b}`}
                          disabled={busy()}
                          onClick={(e) => {
                            e.stopPropagation();
                            void deleteBranch(b);
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

              {/* History */}
              <Show when={moreView() === 'history'}>
                <button class="icon-button git-more-back" onClick={() => setMoreView('menu')}>
                  <IconBack />
                  <span>Recent commits</span>
                </button>
                <Show when={commitsLoading()}>
                  <div class="context-menu-item context-menu-loading">
                    <IconSpinner /> <span>Loading history...</span>
                  </div>
                </Show>
                <Show when={!commitsLoading() && commits().length === 0}>
                  <div class="git-more-note">No commits yet.</div>
                </Show>
                <ul class="context-menu-list">
                  <For each={commits()}>
                    {(c) => (
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
                        </div>
                        <Show when={expandedCommit() === c.oid}>
                          <div class="git-stash-files">
                            <Show when={commitFilesLoading() === c.oid}>
                              <div class="context-menu-item context-menu-loading">
                                <IconSpinner /> <span>Loading commit contents...</span>
                              </div>
                            </Show>
                            <Show
                              when={
                                commitFilesLoading() !== c.oid &&
                                (commitFiles()[c.oid] ?? []).length === 0
                              }
                            >
                              <div class="git-more-note">Empty commit.</div>
                            </Show>
                            <For each={commitFiles()[c.oid] ?? []}>
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
                                        ? `${f.path} (deleted in this commit)`
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
        </div>

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
                                    file().status === 'D'
                                      ? `${file().path} (deleted)`
                                      : `${file().path}: click to open with its changes marked`
                                  }
                                  onClick={() => {
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
            placeholder="Commit message"
            value={commitMessage()}
            onInput={(e) => setCommitMessage(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                commitAndMaybePush();
              }
            }}
            disabled={busy()}
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
    </div>
  );
}
