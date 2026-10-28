// NewProjectDialog.tsx: creates an empty project folder. The user picks
// the folder it goes in (prefilled with the current project's parent, or
// the last one used) and types the new folder's name; a folder that already
// exists is offered for opening instead.
import { homeDir, sep } from '@tauri-apps/api/path';
import { open } from '@tauri-apps/plugin-dialog';
import { createEffect, createSignal, on, onCleanup, onMount, Show } from 'solid-js';
import { Portal } from 'solid-js/web';
import { createFolder, dirEntryCount, getPathId, pathExists } from '../lib/commands';
import { parentOf } from '../lib/fileTools';

const LAST_ROOT_KEY = 'rovik.newProject.lastRoot';

function loadLastRoot(): string | null {
  try {
    return localStorage.getItem(LAST_ROOT_KEY);
  } catch {
    return null;
  }
}

function saveLastRoot(root: string) {
  try {
    localStorage.setItem(LAST_ROOT_KEY, root);
  } catch {
    // Not remembering the folder is fine.
  }
}

type Existing = 'folder' | 'file' | null;

/** What's already at `path`, if anything. */
async function existingAt(path: string): Promise<Existing> {
  try {
    if (!(await pathExists(path))) return null;
    return (await dirEntryCount(path, 1)) === null ? 'file' : 'folder';
  } catch {
    return null;
  }
}

function joinPath(root: string, name: string): string {
  return root.replace(/[\\/]+$/, '') + sep() + name;
}

/** Why `name` can't be a folder name, or null when it can. */
function nameProblem(name: string): string | null {
  if (name === '') return null;
  if (name === '.' || name === '..') return 'Not a valid folder name.';
  if (/[\\/]/.test(name)) return 'The name can’t contain slashes.';
  // eslint-disable-next-line no-control-regex
  if (/[<>:"|?*\u0000-\u001f]/.test(name)) return 'The name can’t contain < > : " | ? or *.';
  if (/[. ]$/.test(name)) return 'The name can’t end with a dot or a space.';
  return null;
}

export default function NewProjectDialog(props: {
  /** The open project, if any: its parent is the default location. */
  currentPath?: string | null;
  onCreated: (path: string) => void;
  /** Opens a folder that's already there as the project. */
  onOpenExisting: (path: string) => void;
  onCancel: () => void;
}) {
  const [root, setRoot] = createSignal(
    (props.currentPath && parentOf(props.currentPath)) || loadLastRoot() || ''
  );
  const [name, setName] = createSignal('');
  const [rootMissing, setRootMissing] = createSignal(false);
  const [existing, setExisting] = createSignal<Existing>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  let nameInput: HTMLInputElement | undefined;
  const previouslyFocused = document.activeElement as HTMLElement | null;

  onMount(async () => {
    nameInput?.focus();
    if (!root()) {
      try {
        setRoot(await homeDir());
      } catch {
        // Leave it empty; the user can browse.
      }
    }
  });
  onCleanup(() => {
    if (previouslyFocused?.isConnected) previouslyFocused.focus();
  });

  const trimmedName = () => name().trim();
  const fullPath = () => (root() && trimmedName() ? joinPath(root(), trimmedName()) : '');
  const problem = () => nameProblem(trimmedName());

  // Checks the location as the user types; a stale answer is dropped.
  let checkSeq = 0;
  createEffect(
    on([root, trimmedName], async ([r, n]) => {
      const seq = ++checkSeq;
      setError(null);
      if (!r) {
        setRootMissing(false);
        setExisting(null);
        return;
      }
      const [rootOk, taken] = await Promise.all([
        pathExists(r).catch(() => false),
        n && !nameProblem(n) ? existingAt(joinPath(r, n)) : null,
      ]);
      if (seq !== checkSeq) return;
      setRootMissing(!rootOk);
      setExisting(taken);
    })
  );

  const canCreate = () =>
    !busy() && !!root() && !!trimmedName() && !problem() && !existing() && !rootMissing();

  async function browse() {
    const picked = await open({
      directory: true,
      multiple: false,
      defaultPath: root() || undefined,
      title: 'Choose where to create the project',
    });
    const folder = Array.isArray(picked) ? picked[0] : picked;
    if (folder) setRoot(folder);
    nameInput?.focus();
  }

  async function create() {
    if (!canCreate()) return;
    const r = root();
    const n = trimmedName();
    const path = joinPath(r, n);
    setBusy(true);
    setError(null);
    try {
      // Re-check: the folder may have appeared since the last check.
      const taken = await existingAt(path);
      if (taken) {
        setExisting(taken);
        return;
      }
      await createFolder(await getPathId(r), n);
      saveLastRoot(r);
      props.onCreated(path);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      props.onCancel();
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
          class="modal confirm-dialog new-project-dialog"
          role="dialog"
          aria-modal="true"
          aria-labelledby="new-project-title"
          onKeyDown={onKeyDown}
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <div class="confirm-dialog-body">
            <div id="new-project-title" class="confirm-dialog-title">
              New project
            </div>

            <label class="new-project-label" for="new-project-root">
              Location
            </label>
            <div class="new-project-root-row">
              <input
                id="new-project-root"
                class="new-project-input"
                value={root()}
                placeholder="Folder to create the project in"
                spellcheck={false}
                onInput={(e) => setRoot(e.currentTarget.value)}
              />
              <button type="button" class="new-project-browse" onClick={() => void browse()}>
                Browse…
              </button>
            </div>
            <Show when={rootMissing()}>
              <div class="new-project-note error">This folder doesn’t exist.</div>
            </Show>

            <label class="new-project-label" for="new-project-name">
              Project folder name
            </label>
            <input
              id="new-project-name"
              ref={nameInput}
              class="new-project-input"
              value={name()}
              placeholder="my-project"
              spellcheck={false}
              autocomplete="off"
              onInput={(e) => setName(e.currentTarget.value)}
            />

            <Show when={problem()}>
              <div class="new-project-note error">{problem()}</div>
            </Show>
            <Show when={!problem() && existing() === 'folder'}>
              <div class="new-project-note warning new-project-existing" role="alert">
                <span>⚠ This folder already exists.</span>
                <button
                  type="button"
                  class="new-project-open-existing"
                  onClick={() => props.onOpenExisting(fullPath())}
                >
                  Open existing
                </button>
              </div>
            </Show>
            <Show when={!problem() && existing() === 'file'}>
              <div class="new-project-note warning" role="alert">
                ⚠ A file with this name already exists here.
              </div>
            </Show>
            <Show when={error()}>
              <div class="new-project-note error">Couldn’t create the folder: {error()}</div>
            </Show>
            <Show when={fullPath() && !problem() && !existing()}>
              <div class="new-project-note">
                Will create <strong>{fullPath()}</strong>
              </div>
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
