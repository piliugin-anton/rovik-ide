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
import { GitFileStatusCode, GitPanelPhase, GitStatus } from '../types';
import { confirmDialog } from './ConfirmDialog';
import { ChevronIcon, FileIcon, FolderIcon } from './FileIcons';

const GIT_ROW_HEIGHT = 22;

interface GitPanelProps {
  /** Absolute path to the currently open project/workspace. */
  projectPath: string;
  /** A changed file was clicked: open it with its changes marked. */
  onOpenFile?: (absPath: string, file: string) => void;
  /** A file was reverted on disk (its tab should show the restored text). */
  onFileReverted?: (absPath: string) => void;
  /** Git's state may have changed (commit, revert, checkout, refresh). */
  onChanged?: () => void;
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

interface GitDirNode {
  name: string;
  /** Relative to `workdir`, `/`-separated; '' for the root. */
  path: string;
  dirs: Map<string, GitDirNode>;
  files: GitFile[];
  /** Changed files anywhere below this folder. */
  count: number;
}

type GitTreeRow =
  | { kind: 'dir'; depth: number; name: string; node: GitDirNode }
  | { kind: 'file'; depth: number; name: string; file: GitFile };

const asDirRow = (r: GitTreeRow) => (r.kind === 'dir' ? r : undefined);
const asFileRow = (r: GitTreeRow) => (r.kind === 'file' ? r : undefined);

const byName = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' }).compare;

function buildGitTree(files: GitFile[]): GitDirNode {
  const root: GitDirNode = { name: '', path: '', dirs: new Map(), files: [], count: 0 };
  for (const file of files) {
    const parts = file.path.split('/').filter(Boolean);
    let node = root;
    node.count++;
    for (const part of parts.slice(0, -1)) {
      let next = node.dirs.get(part);
      if (!next) {
        next = {
          name: part,
          path: node.path ? `${node.path}/${part}` : part,
          dirs: new Map(),
          files: [],
          count: 0,
        };
        node.dirs.set(part, next);
      }
      next.count++;
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
      const msg = commitMessage().trim();
      if (!msg || !status()) return;

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

  const toggleBranchDropdown = () => {
    const next = !branchOpen();
    setBranchOpen(next);
    if (next) loadBranches();
  };

  // Derived, reactive values (memoized so they actually update on signal
  // changes — reading a signal into a plain `const` outside of a memo/JSX
  // only captures its value once).
  const canCommit = createMemo(() => !!commitMessage().trim() && !!status() && !busy());
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
      { defer: true },
    ),
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
        <div class="git-panel-header">
          <div class="git-branch-row">
            <button
              class="git-branch-button icon-button"
              onClick={toggleBranchDropdown}
              title="Switch branch"
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
