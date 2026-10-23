// GitHooksView.tsx
//
// The Git panel's "Git hooks" view (in its ⋯ menu): the hooks the panel
// runs (src-tauri/src/git/hooks.rs), each on, not executable (skipped),
// only git's example, or not there. One that's there opens in the editor;
// one that isn't is made from a template, executable, and opened; one that
// isn't executable is made so; one can go to the trash. The file tree
// doesn't show .git, so this is where its hooks are found
// (src-tauri/src/git/hook_files.rs).
import { createSignal, For, onMount, Show } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import { confirmDialog } from './ConfirmDialog';
import {
  HOOK_STATE_LABELS,
  hookTitle,
  hooksPlace,
  type GitHook,
  type GitHooks,
} from '../lib/gitHooks';

function IconTrash() {
  return (
    <svg
      width={14}
      height={14}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width={2}
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      <polyline points="3 6 5 6 21 6" />
      <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
      <path d="M10 11v6M14 11v6" />
      <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
    </svg>
  );
}

export function GitHooksView(props: {
  projectPath: string;
  /** Opens a hook's file in the editor. */
  onOpen: (path: string) => void;
}) {
  const [hooks, setHooks] = createSignal<GitHooks | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [working, setWorking] = createSignal(false);

  const load = async () => {
    try {
      setHooks(await invoke<GitHooks>('git_hooks', { path: props.projectPath }));
    } catch (e) {
      setError(String(e));
    }
  };
  onMount(() => void load());

  /** Runs `fn`, then reads the hooks again; its error shows below them. */
  const act = async (fn: () => Promise<void>) => {
    setWorking(true);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      await load();
      setWorking(false);
    }
  };

  const create = (hook: GitHook) =>
    act(async () => {
      const path = await invoke<string>('git_hook_create', {
        path: props.projectPath,
        name: hook.name,
      });
      props.onOpen(path);
    });

  const enable = (hook: GitHook) =>
    act(() => invoke('git_hook_make_executable', { path: props.projectPath, name: hook.name }));

  const remove = async (hook: GitHook) => {
    const ok = await confirmDialog({
      title: `Delete the ${hook.name} hook?`,
      message:
        'It goes to the trash, from where it can be put back. Until then git doesn’t run it.',
      confirmLabel: 'Delete',
      danger: true,
    });
    if (ok)
      await act(() => invoke('git_hook_delete', { path: props.projectPath, name: hook.name }));
  };

  return (
    <div class="git-hooks">
      <Show when={hooks()}>{(h) => <div class="git-more-note">{hooksPlace(h())}</div>}</Show>
      <ul class="context-menu-list">
        <For each={hooks()?.hooks ?? []}>
          {(hook) => (
            <li
              class="context-menu-item git-more-row git-hook-row"
              classList={{ 'git-hook-absent': !hook.path }}
              title={hookTitle(hook)}
              onClick={() => {
                if (hook.path) props.onOpen(hook.path);
              }}
            >
              <span class="git-more-row-label">
                {hook.name}
                <Show when={HOOK_STATE_LABELS[hook.state]}>
                  {(label) => (
                    <span class={`git-hook-state git-hook-${hook.state}`}> · {label()}</span>
                  )}
                </Show>
              </span>
              <Show when={hook.state === 'not-executable'}>
                <button
                  type="button"
                  class="git-more-row-button"
                  title="Make it executable (chmod +x), so git runs it"
                  disabled={working()}
                  onClick={(e) => {
                    e.stopPropagation();
                    void enable(hook);
                  }}
                >
                  <span class="git-more-row-text">Enable</span>
                </button>
              </Show>
              <Show
                when={hook.path}
                fallback={
                  <button
                    type="button"
                    class="git-more-row-button"
                    title={`Make a ${hook.name} hook from a template and open it`}
                    disabled={working()}
                    onClick={(e) => {
                      e.stopPropagation();
                      void create(hook);
                    }}
                  >
                    <span class="git-more-row-text">Create</span>
                  </button>
                }
              >
                <button
                  type="button"
                  class="git-more-row-button git-menu-danger"
                  title="Move it to the trash"
                  aria-label={`Delete the ${hook.name} hook`}
                  disabled={working()}
                  onClick={(e) => {
                    e.stopPropagation();
                    void remove(hook);
                  }}
                >
                  <IconTrash />
                </button>
              </Show>
            </li>
          )}
        </For>
      </ul>
      <Show when={error()}>{(e) => <div class="git-more-note error">{e()}</div>}</Show>
    </div>
  );
}
