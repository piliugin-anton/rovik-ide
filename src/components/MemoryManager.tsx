// MemoryManager.tsx
//
// Settings → Long-term memory → "Manage memories…": what the open project's
// memory holds, as a list the user works on (lib/memory.ts). The search asks
// the store once the typist pauses; a memory's title and text can be
// corrected; one can be removed, or every memory the project keeps cleared.
// Both removals ask first (components/ConfirmDialog.tsx) — they can't be
// undone. Everything else a memory carries (its trust state and evidence,
// its tags and files) stays as the agent left it.
import { createResource, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { confirmDialog, type ConfirmOptions } from './ConfirmDialog';
import {
  memoryClear,
  memoryDelete,
  memoryEdit,
  memoryList,
  memoryStateLabel,
  type MemoryEntry,
} from '../lib/memory';
import { plural } from '../lib/textUtils';

/** How long the search waits for the typist to pause, as the chat's search does. */
const SEARCH_DEBOUNCE_MS = 150;

/** The memory being edited: its id, and what its fields say as they're typed. */
interface Draft {
  id: string;
  title: string;
  content: string;
}

export default function MemoryManager(props: { root: string; onClose: () => void }) {
  // `typed` is the box; `search` is what the store is asked, once typing pauses.
  const [typed, setTyped] = createSignal('');
  const [search, setSearch] = createSignal('');
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const [saving, setSaving] = createSignal(false);
  const [editError, setEditError] = createSignal<string | null>(null);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  // True while a confirm dialog is on top: it takes the keys, not this dialog.
  const [confirming, setConfirming] = createSignal(false);

  let pending: number | undefined;
  let dialogRef: HTMLDivElement | undefined;
  let searchRef: HTMLInputElement | undefined;

  const [rows, { refetch }] = createResource(search, (query) =>
    memoryList(props.root, query).catch(() => null)
  );

  /** What the list shows: the last answer, which stays while a new one comes. */
  const listed = () => rows.latest ?? [];
  /** The last ask failed, rather than not having answered yet. */
  const unreadable = () => rows() === null;

  /** What an empty list means: nothing kept yet, or nothing matches. */
  const emptyText = () =>
    search()
      ? `No memories match “${search()}”`
      : 'No memories yet: the agent adds them as it works.';

  /** Takes what's typed, and asks for it once the typist pauses. */
  function type(query: string) {
    setTyped(query);
    window.clearTimeout(pending);
    pending = window.setTimeout(() => setSearch(query), SEARCH_DEBOUNCE_MS);
  }

  /** Empties the box and the search it made, the pending pause included. */
  function clearSearch() {
    window.clearTimeout(pending);
    setTyped('');
    setSearch('');
  }

  onCleanup(() => window.clearTimeout(pending));

  /** Asks a question on top, while this dialog leaves the keys to it. */
  async function ask(options: ConfirmOptions): Promise<boolean> {
    setConfirming(true);
    try {
      return await confirmDialog(options);
    } finally {
      setConfirming(false);
    }
  }

  /**
   * Keeps Tab inside the dialog: past its last control the focus goes to the
   * first, and Shift+Tab from the first to the last, as the confirm dialog
   * does over its own. The settings page behind is still focusable, so
   * without this a Tab would walk into it.
   */
  function onDialogKeyDown(e: KeyboardEvent) {
    if (e.key !== 'Tab' || !dialogRef) return;
    const focusable = [
      ...dialogRef.querySelectorAll<HTMLElement>(
        'button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled)'
      ),
    ];
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }

  /**
   * The keys the dialog's own handler can't see, a press whose target lies
   * outside it: Escape drops the correction being typed, or closes the
   * manager when none is (a click on a memory's text leaves the focus on the
   * page behind), and a Tab from outside goes to the search box rather than
   * into the page's controls. While a confirm dialog is up, it takes both.
   */
  function onWindowKeyDown(e: KeyboardEvent) {
    if (confirming()) return;
    const dialog = dialogRef;
    if (!dialog) return;
    const inside = e.target instanceof Node && dialog.contains(e.target);
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      // Escape backs out of the form first: a key pressed while correcting a
      // memory drops the correction, not the list the user is working in.
      if (draft()) {
        cancelEdit();
        return;
      }
      props.onClose();
      return;
    }
    if (e.key === 'Tab' && !inside) {
      e.preventDefault();
      searchRef?.focus();
    }
  }

  onMount(() => {
    // The search box starts with the focus, as a confirm dialog's safe button
    // does: the keys a press starts from are then this dialog's.
    searchRef?.focus();
    window.addEventListener('keydown', onWindowKeyDown, { capture: true });
  });
  onCleanup(() => window.removeEventListener('keydown', onWindowKeyDown, { capture: true }));

  /** Opens a row's form; it takes the focus (its title's ref), so typing lands there. */
  function startEdit(entry: MemoryEntry) {
    setEditError(null);
    setDraft({ id: entry.id, title: entry.title, content: entry.content });
  }

  /** Drops the correction being typed, leaving the memory as it's stored. */
  function cancelEdit() {
    setDraft(null);
    setEditError(null);
  }

  function changeDraft(field: 'title' | 'content', value: string) {
    setDraft((form) => (form ? { ...form, [field]: value } : form));
  }

  /** Saves the title and text, trimmed: a memory needs both. */
  async function save() {
    const form = draft();
    if (!form) return;
    const title = form.title.trim();
    const content = form.content.trim();
    if (!title) {
      setEditError('A memory needs a title.');
      return;
    }
    if (!content) {
      setEditError('A memory needs some text.');
      return;
    }
    setSaving(true);
    setEditError(null);
    try {
      await memoryEdit(props.root, form.id, title, content);
      setDraft(null);
      await refetch();
    } catch (err) {
      setEditError(`Couldn't save the memory: ${String(err)}`);
    } finally {
      setSaving(false);
    }
  }

  async function remove(entry: MemoryEntry) {
    setFailure(null);
    setNotice(null);
    const confirmed = await ask({
      title: 'Delete this memory?',
      message: entry.title,
      confirmLabel: 'Delete',
      danger: true,
    });
    if (!confirmed) return;
    try {
      await memoryDelete(props.root, entry.id);
      await refetch();
    } catch (err) {
      setFailure(`Couldn't delete the memory: ${String(err)}`);
    }
  }

  async function clearAll() {
    setFailure(null);
    setNotice(null);
    const confirmed = await ask({
      title: "Clear this project's memory?",
      message: 'Every memory this project keeps is deleted, not only the ones shown here.',
      confirmLabel: 'Clear memory',
      danger: true,
    });
    if (!confirmed) return;
    try {
      const cleared = await memoryClear(props.root);
      setNotice(`Cleared ${plural(cleared, 'memory', 'memories')}.`);
      await refetch();
    } catch (err) {
      setFailure(`Couldn't clear the memory: ${String(err)}`);
    }
  }

  return (
    <div class="modal-overlay">
      <div
        ref={dialogRef}
        class="modal memory-manager"
        role="dialog"
        aria-modal="true"
        aria-label="Memories in this project"
        onKeyDown={onDialogKeyDown}
      >
        <div class="modal-header">
          <span class="modal-title">Memories in this project</span>
          <button class="icon-button" onClick={() => props.onClose()} aria-label="Close">
            ×
          </button>
        </div>

        <div class="modal-body">
          <div class="memory-manager-tools">
            <div class="memory-manager-search">
              <input
                ref={searchRef}
                value={typed()}
                onInput={(e) => type(e.currentTarget.value)}
                placeholder="Search memories"
                spellcheck={false}
                aria-label="Search this project's memories"
              />
              <Show when={typed()}>
                <button
                  class="memory-manager-search-clear"
                  onClick={clearSearch}
                  aria-label="Clear search"
                  title="Clear"
                >
                  ×
                </button>
              </Show>
            </div>
            <button class="link-button danger-link" onClick={() => void clearAll()}>
              Clear all memories…
            </button>
          </div>

          <Show when={notice()}>
            <p class="settings-hint memory-manager-notice" role="status">
              {notice()}
            </p>
          </Show>
          <Show when={failure()}>
            <p class="settings-status warning" role="alert">
              {failure()}
            </p>
          </Show>

          <Show when={rows.loading && rows.latest === undefined}>
            <p class="settings-hint" role="status">
              Reading the memories…
            </p>
          </Show>
          <Show when={unreadable()}>
            <p class="settings-status warning" role="alert">
              The memories couldn't be read.
            </p>
          </Show>

          <Show when={rows.latest !== undefined && !unreadable()}>
            <Show when={listed().length > 0} fallback={<p class="settings-hint">{emptyText()}</p>}>
              <ul class="memory-list">
                <For each={listed()}>
                  {(entry) => (
                    <li class="memory-entry">
                      <Show when={draft()?.id !== entry.id}>
                        <div class="memory-entry-head">
                          <span
                            class="prior-work-state"
                            classList={{ [entry.verification]: true }}
                            title={entry.evidence}
                          >
                            {memoryStateLabel(entry.verification)}
                          </span>
                          <span class="prior-work-kind">{entry.kind}</span>
                          <span class="memory-entry-title">{entry.title}</span>
                          <span class="memory-entry-date">
                            {new Date(entry.updatedAt).toLocaleDateString()}
                          </span>
                        </div>
                        <div class="memory-entry-content">{entry.content}</div>
                        <Show when={entry.files.length > 0}>
                          <div class="memory-entry-files">{entry.files.join(', ')}</div>
                        </Show>
                        <div class="memory-entry-actions">
                          <button class="link-button" onClick={() => startEdit(entry)}>
                            Edit
                          </button>
                          <button class="link-button" onClick={() => void remove(entry)}>
                            Delete
                          </button>
                        </div>
                      </Show>

                      <Show when={draft()?.id === entry.id}>
                        <div class="memory-entry-edit">
                          <input
                            ref={(el) => queueMicrotask(() => el.focus())}
                            aria-label="Memory title"
                            value={draft()?.title ?? ''}
                            onInput={(e) => changeDraft('title', e.currentTarget.value)}
                            spellcheck={false}
                          />
                          <textarea
                            aria-label="Memory text"
                            rows={3}
                            value={draft()?.content ?? ''}
                            onInput={(e) => changeDraft('content', e.currentTarget.value)}
                          />
                          <Show when={editError()}>
                            <p class="settings-status warning" role="alert">
                              {editError()}
                            </p>
                          </Show>
                          <div class="memory-entry-edit-actions">
                            <button class="link-button" onClick={() => cancelEdit()}>
                              Cancel
                            </button>
                            <button class="primary" disabled={saving()} onClick={() => void save()}>
                              Save
                            </button>
                          </div>
                        </div>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </Show>
        </div>
      </div>
    </div>
  );
}
