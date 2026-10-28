// NewWorkspaceDialog.tsx: Create a workspace… (docs/design/workspaces-plan.md,
// 3.4, D4): its name and its folders, the open folder listed first. Add
// folder… opens the picker, several folders at once, and the field beside it
// takes a folder's full path, typed or pasted, as New project's location field
// does. Each folder is checked as it's listed, against those listed before it
// (D5) and against the other windows' (D7): why one can't join shows on its
// row, and Create stays off while one does. Create makes the workspace; the
// window then opens it (App).
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import { Portal } from 'solid-js/web';
import { chatApi, folderProblemMessage, type Workspace } from '../lib/chatApi';
import { pickFolders } from '../lib/commands';
import { isAbsolutePath } from '../lib/fileTools';
import { openElsewhereReason } from '../lib/instances';
import { fileName } from '../lib/toolGroups';

/** The longest name a workspace takes (chat_db/workspaces.rs). */
const NAME_MAX = 80;

/** A workspace's name until the user types one: its folders' names ("api + web"). */
export function defaultWorkspaceName(paths: readonly string[]): string {
  const name = paths.map(fileName).join(' + ');
  return name.length > NAME_MAX ? `${name.slice(0, NAME_MAX - 1)}…` : name;
}

/** A path as typed or pasted: trimmed, and out of the quotes a copied path comes in. */
function typedPath(text: string): string {
  const trimmed = text.trim();
  return /^".*"$/.test(trimmed) ? trimmed.slice(1, -1).trim() : trimmed;
}

/**
 * Why the folder at `path` can't join a workspace beside `before`, the folders
 * listed before it; null when it can.
 */
async function whyNot(path: string, before: string[]): Promise<string | null> {
  if (!isAbsolutePath(path)) return 'Type the folder’s full path.';
  try {
    const problem = await chatApi.checkWorkspaceFolder(null, path, before);
    if (problem) return folderProblemMessage(problem);
  } catch (err) {
    return String(err);
  }
  return openElsewhereReason(path);
}

export default function NewWorkspaceDialog(props: {
  /** The folder the window has open: listed first (D4). None in a workspace, or with nothing open. */
  openFolder?: string | null;
  /** The workspace was made: the window opens it. */
  onCreated: (workspace: Workspace) => void;
  onCancel: () => void;
}) {
  const [paths, setPaths] = createSignal<string[]>(props.openFolder ? [props.openFolder] : []);
  /** Each listed folder's reason it can't join, by its place in the list; undefined while it's checked. */
  const [problems, setProblems] = createSignal<(string | null | undefined)[]>([]);
  /** The name the user typed; null until they type one. */
  const [typedName, setTypedName] = createSignal<string | null>(null);
  const [typed, setTyped] = createSignal('');
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  let addButton: HTMLButtonElement | undefined;
  const previouslyFocused = document.activeElement as HTMLElement | null;

  onMount(() => addButton?.focus());
  onCleanup(() => {
    if (previouslyFocused?.isConnected) previouslyFocused.focus();
  });

  const name = () => typedName()?.trim() || defaultWorkspaceName(paths());

  // Each folder is checked against those listed before it, again as the list
  // changes; an answer for a list that changed since is dropped.
  let checks = 0;
  createEffect(
    on(paths, async (list) => {
      const run = ++checks;
      setProblems(list.map(() => undefined));
      const found = await Promise.all(list.map((path, i) => whyNot(path, list.slice(0, i))));
      if (run === checks) setProblems(found);
    })
  );

  const add = (added: string[]) => {
    setError(null);
    if (added.length > 0) setPaths((list) => [...list, ...added]);
  };
  const remove = (index: number) => setPaths((list) => list.filter((_, i) => i !== index));

  async function pick() {
    add(await pickFolders('Choose folders for the workspace'));
  }

  function addTyped() {
    const path = typedPath(typed());
    if (!path) return;
    add([path]);
    setTyped('');
  }

  const canCreate = () =>
    !busy() &&
    paths().length > 0 &&
    !!name() &&
    problems().length === paths().length &&
    problems().every((problem) => problem === null);

  async function create() {
    if (!canCreate()) return;
    setBusy(true);
    setError(null);
    try {
      props.onCreated(await chatApi.createWorkspace(name(), paths()));
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Portal>
      <div
        class="modal-overlay"
        onMouseDown={(e) => {
          if (e.target === e.currentTarget) props.onCancel();
        }}
      >
        <form
          class="modal confirm-dialog new-project-dialog new-workspace-dialog"
          role="dialog"
          aria-modal="true"
          aria-labelledby="new-workspace-title"
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.preventDefault();
              e.stopPropagation();
              props.onCancel();
            }
          }}
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <div class="confirm-dialog-body">
            <div id="new-workspace-title" class="confirm-dialog-title">
              New workspace
            </div>
            <div class="new-project-note">
              Its folders open together in this window. Each keeps its own chats and settings.
            </div>

            <label class="new-project-label" for="new-workspace-name">
              Name
            </label>
            <input
              id="new-workspace-name"
              class="new-project-input"
              value={typedName() ?? defaultWorkspaceName(paths())}
              placeholder={defaultWorkspaceName(paths()) || 'Workspace name'}
              maxLength={NAME_MAX}
              spellcheck={false}
              autocomplete="off"
              onInput={(e) => setTypedName(e.currentTarget.value)}
            />

            <div class="new-project-label">Folders</div>
            <ul class="new-workspace-folders" aria-label="Folders">
              <For
                each={paths()}
                fallback={<li class="new-workspace-empty">Add the folders it holds.</li>}
              >
                {(path, i) => (
                  <li
                    class="new-workspace-folder"
                    classList={{ refused: typeof problems()[i()] === 'string' }}
                  >
                    <div class="new-workspace-folder-row">
                      <span class="new-workspace-folder-name">{fileName(path)}</span>
                      <span class="new-workspace-folder-path" title={path}>
                        {path}
                      </span>
                      <button
                        type="button"
                        class="icon-button new-workspace-folder-remove"
                        title="Leave it out"
                        aria-label={`Leave ${fileName(path)} out`}
                        onClick={() => remove(i())}
                      >
                        ✕
                      </button>
                    </div>
                    <Show when={problems()[i()]}>
                      {(why) => <div class="new-project-note error">{why()}</div>}
                    </Show>
                  </li>
                )}
              </For>
            </ul>
            <div class="new-project-root-row new-workspace-add-row">
              <input
                class="new-project-input new-workspace-path"
                value={typed()}
                placeholder="A folder’s full path"
                aria-label="A folder’s full path"
                spellcheck={false}
                autocomplete="off"
                onInput={(e) => setTyped(e.currentTarget.value)}
                onKeyDown={(e) => {
                  // Enter adds the path typed, rather than creating.
                  if (e.key === 'Enter' && typed().trim()) {
                    e.preventDefault();
                    addTyped();
                  }
                }}
              />
              <button
                type="button"
                class="new-project-browse new-workspace-add-path"
                disabled={!typed().trim()}
                onClick={addTyped}
              >
                Add
              </button>
              <button
                ref={addButton}
                type="button"
                class="new-project-browse new-workspace-pick"
                onClick={() => void pick()}
              >
                Add folder…
              </button>
            </div>
            <Show when={error()}>
              <div class="new-project-note error">Couldn’t create the workspace: {error()}</div>
            </Show>
          </div>

          <div class="confirm-dialog-actions">
            <button type="button" onClick={() => props.onCancel()}>
              Cancel
            </button>
            <button type="submit" class="primary" disabled={!canCreate()}>
              Create
            </button>
          </div>
        </form>
      </div>
    </Portal>
  );
}
