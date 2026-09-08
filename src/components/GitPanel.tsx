// GitPanel.tsx
import { createSignal, createMemo, onMount, For, Show, createEffect } from 'solid-js';
import { createVirtualizer, Virtualizer } from '@tanstack/solid-virtual';
import { invoke } from '@tauri-apps/api/core';
import { GitFileStatusCode, GitPanelPhase, GitStatus } from '../types';

const GIT_ROW_HEIGHT = 22;

interface GitPanelProps {
  /** Absolute path to the currently open project/workspace. */
  projectPath: string;
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

  onMount(() => {
    withBusy(refreshStatus);
  });

  // Recreate virtualizer whenever count or scroll element changes
  createEffect(() => {
    const scrollEl = gitFilesScrollEl();

    if (!scrollEl) {
      setGitFilesVirtualizer(null);
      return;
    }

    const count = filesCount();
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
                  const file = () => status()!.files[item.index];
                  const meta = () => statusMeta(file().status);

                  return (
                    <div
                      data-index={item.index}
                      class="tree-row"
                      title={file().path}
                      style={{
                        position: 'absolute',
                        top: 0,
                        left: 0,
                        width: '100%',
                        height: `${GIT_ROW_HEIGHT}px`,
                        transform: `translateY(${item.start}px)`,
                        'padding-left': '8px', // match your git-files .tree-row padding
                        'user-select': 'none',
                        '-webkit-user-select': 'none',
                      }}
                    >
                      <span class={`tree-row-badge ${meta().className}`} title={meta().title}>
                        {meta().label}
                      </span>
                      <span class="tree-row-name">{file().path}</span>
                    </div>
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
