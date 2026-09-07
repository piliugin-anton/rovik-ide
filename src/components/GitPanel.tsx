// GitPanel.tsx
import { createSignal, createEffect, onCleanup } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';

type GitStatus = {
  branch: string;
  behind: number;
  ahead: number;
  files: {
    path: string;
    status: 'M' | 'A' | 'D' | 'R' | 'U' | '?'; // modified, added, deleted, renamed, unmerged, untracked
  }[];
};

export function GitPanel() {
  const [status, setStatus] = createSignal<GitStatus | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [commitMessage, setCommitMessage] = createSignal('');
  const [branchOpen, setBranchOpen] = createSignal(false);
  const [branches, setBranches] = createSignal<string[]>([]);

  // Load initial status
  const refreshStatus = async () => {
    setLoading(true);
    try {
      // Replace with your Tauri command, e.g. invoke("git_status")
      const s = await invoke<GitStatus>('git_status');
      setStatus(s);
    } catch (e) {
      console.error('git_status error', e);
    } finally {
      setLoading(false);
    }
  };

  const fetchGit = async () => {
    try {
      await invoke('git_fetch');
      await refreshStatus();
    } catch (e) {
      console.error('git_fetch error', e);
    }
  };

  const pullGit = async () => {
    try {
      await invoke('git_pull');
      await refreshStatus();
    } catch (e) {
      console.error('git_pull error', e);
    }
  };

  const commitAndMaybePush = async () => {
    const msg = commitMessage().trim();
    if (!msg || !status()) return;
    try {
      await invoke('git_commit', { message: msg });
      setCommitMessage('');
      await refreshStatus();

      // If there are commits ahead, push
      const s = status(); // might be stale immediately; depends on your backend
      if (s && s.ahead > 0) {
        await invoke('git_push');
        await refreshStatus();
      }
    } catch (e) {
      console.error('git_commit/push error', e);
    }
  };

  const switchBranch = async (branch: string) => {
    try {
      await invoke('git_checkout', { branch });
      setBranchOpen(false);
      await refreshStatus();
    } catch (e) {
      console.error('git_checkout error', e);
    }
  };

  const loadBranches = async () => {
    try {
      const list = await invoke<string[]>('git_branches');
      setBranches(list);
    } catch (e) {
      console.error('git_branches error', e);
    }
  };

  createEffect(() => {
    if (branchOpen()) {
      loadBranches();
    }
  });

  onCleanup(() => {
    // optional: abort in-flight requests if needed
  });

  // Initial load
  refreshStatus();

  const s = status();
  const canCommit = !!commitMessage().trim() && !!s;
  const commitButtonText = s && s.ahead! > 0 ? 'Push' : 'Commit';

  return (
    <div class="git-panel">
      {/* Header */}
      <div class="git-panel-header">
        <div class="git-branch-row">
          <span class="git-branch-label">Branch:</span>
          <button
            class="git-branch-button icon-button"
            onClick={() => setBranchOpen((v) => !v)}
            title="Switch branch"
          >
            {s?.branch ?? '...'}
            <span class="git-branch-arrow">▼</span>
          </button>
        </div>

        {branchOpen() && (
          <div class="git-branch-dropdown context-menu">
            <ul class="context-menu-list">
              {branches().map((b) => (
                <li
                  class="context-menu-item"
                  onClick={() => switchBranch(b)}
                  classList={{ 'git-branch-item-active': b === s?.branch }}
                >
                  {b}
                </li>
              ))}
            </ul>
          </div>
        )}

        <div class="git-fetch-pull-row">
          <button class="icon-button" onClick={fetchGit} title="Fetch" disabled={loading()}>
            Fetch
          </button>
          <button class="icon-button" onClick={pullGit} title="Pull" disabled={loading()}>
            Pull
          </button>
        </div>
      </div>

      {/* Uncommitted files */}
      <div class="git-files file-tree">
        {loading() && !s && <div class="empty-state">Loading Git status...</div>}
        {!s && !loading() && <div class="empty-state">No Git repository</div>}
        {s && s.files.length === 0 && <div class="empty-state">No uncommitted changes</div>}
        {s && s.files.length > 0 && (
          <div>
            {s.files.map((f) => (
              <div class="tree-row" title={f.path}>
                <span class="tree-row-icon">{f.status === '?' ? 'U' : f.status}</span>
                <span class="tree-row-name">{f.path}</span>
              </div>
            ))}
          </div>
        )}
      </div>

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
        />
        <button
          class="git-commit-button primary"
          disabled={!canCommit || loading()}
          onClick={commitAndMaybePush}
        >
          {commitButtonText}
        </button>
      </div>
    </div>
  );
}
