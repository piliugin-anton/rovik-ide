// RestrictedFilesSettings.tsx
//
// Settings → Restricted files: the files of the open project the agent may
// not read or change (lib/restrictedFiles.ts). The table lists what the
// backend found whose names say they hold credentials, both boxes ticked,
// and the files the user added; a ticked box keeps the agent from reading
// or changing that file, and every call that would is refused with the
// reason. Ticking or unticking a box saves the file's entry for the
// project at once, kept with the entries saved before it however fast the
// clicks come; nothing to save on close.
import { createEffect, createSignal, Index, on, Show } from 'solid-js';
import {
  isPattern,
  loadRestrictedFiles,
  projectPath,
  updateRestrictedFiles,
  withEntry,
  withoutEntry,
  type RestrictedFile,
  type RestrictedFileRow,
} from '../lib/restrictedFiles';

export default function RestrictedFilesSettings(props: { root: string }) {
  const [rows, setRows] = createSignal<RestrictedFileRow[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [error, setError] = createSignal<string | null>(null);
  const [adding, setAdding] = createSignal('');
  const [addProblem, setAddProblem] = createSignal<string | null>(null);

  const folder = () => props.root.split(/[\\/]/).filter(Boolean).pop() ?? props.root;

  // Loads made: only the last one's table is shown, so an earlier one that
  // comes back late doesn't undo a change shown since.
  let loads = 0;
  async function load() {
    const mine = ++loads;
    try {
      const found = await loadRestrictedFiles(props.root);
      if (mine === loads) {
        setRows(found);
        setError(null);
      }
    } catch (err) {
      if (mine === loads) setError(`Couldn't look at the project: ${String(err)}`);
    } finally {
      if (mine === loads) setLoading(false);
    }
  }
  // The open project's table, again when another project opens.
  createEffect(
    on(
      () => props.root,
      () => {
        setLoading(true);
        void load();
      }
    )
  );

  /**
   * Changes the user's list as it's saved, then shows the table as it is
   * now: saved, or as it was if the save failed.
   */
  async function change(update: (entries: RestrictedFile[]) => RestrictedFile[]) {
    // What's loading now was found before this change.
    loads++;
    try {
      await updateRestrictedFiles(props.root, update);
    } catch (err) {
      await load();
      setError(`Couldn't save: ${String(err)}`);
      return;
    }
    await load();
  }

  function setBox(row: RestrictedFileRow, box: 'blockRead' | 'blockWrite', on: boolean) {
    const entry: RestrictedFile = {
      path: row.path,
      blockRead: row.blockRead,
      blockWrite: row.blockWrite,
      [box]: on,
    };
    setRows(rows().map((r) => (r.path === row.path ? { ...r, ...entry, listed: true } : r)));
    void change((entries) => withEntry(entries, entry));
  }

  function add(e: Event) {
    e.preventDefault();
    if (isPattern(adding())) {
      setAddProblem('Give one file or folder: a pattern such as *.pem isn’t matched.');
      return;
    }
    const path = projectPath(adding(), props.root);
    if (!path) {
      setAddProblem('Give the path of a file or folder in the project, such as config/keys.json.');
      return;
    }
    if (rows().some((r) => r.path === path)) {
      setAddProblem(`${path} is in the list already.`);
      return;
    }
    setAddProblem(null);
    setAdding('');
    void change((entries) => withEntry(entries, { path, blockRead: true, blockWrite: true }));
  }

  return (
    <div class="permissions-settings restricted-files-settings">
      <section class="settings-section">
        <div class="settings-section-title">Restricted files in {folder()}</div>
        <p class="settings-hint">
          A ticked box keeps the agent from reading or changing the file: a call that would is
          refused, and the agent is told why. Files whose names say they hold credentials (.env
          files, keys, cloud and package registry logins) are found and ticked for you; untick a box
          to let the agent in. Templates such as .env.example aren’t restricted.
        </p>
        <Show when={!loading()} fallback={<p class="settings-hint">Looking at the project…</p>}>
          <table class="restricted-files">
            <thead>
              <tr>
                <th scope="col">File</th>
                <th scope="col" title="Ticked: the agent may not read it">
                  Read
                </th>
                <th scope="col" title="Ticked: the agent may not change it">
                  Write
                </th>
                <th scope="col" aria-label="Remove" />
              </tr>
            </thead>
            <tbody>
              {/* By position, not by row: a reload keeps each row's boxes,
                  and the focus on the one just ticked. */}
              <Index
                each={rows()}
                fallback={
                  <tr>
                    <td class="restricted-files-empty" colspan={4}>
                      No files that look like credentials. Add one below to keep it from the agent.
                    </td>
                  </tr>
                }
              >
                {(row) => (
                  <tr>
                    <td class="restricted-file-path" title={row().path}>
                      <span class="restricted-file-name">{row().path}</span>
                      <Show when={!row().detected}>
                        <span class="restricted-file-origin">added by you</span>
                      </Show>
                    </td>
                    <td>
                      <input
                        type="checkbox"
                        checked={row().blockRead}
                        aria-label={`Keep the agent from reading ${row().path}`}
                        title="Ticked: the agent may not read it"
                        onChange={(e) => setBox(row(), 'blockRead', e.currentTarget.checked)}
                      />
                    </td>
                    <td>
                      <input
                        type="checkbox"
                        checked={row().blockWrite}
                        aria-label={`Keep the agent from changing ${row().path}`}
                        title="Ticked: the agent may not change it"
                        onChange={(e) => setBox(row(), 'blockWrite', e.currentTarget.checked)}
                      />
                    </td>
                    <td>
                      <Show when={row().listed && !row().detected}>
                        <button
                          class="icon-button"
                          aria-label={`Remove ${row().path}`}
                          title="Remove"
                          onClick={() => {
                            const path = row().path;
                            void change((entries) => withoutEntry(entries, path));
                          }}
                        >
                          ×
                        </button>
                      </Show>
                    </td>
                  </tr>
                )}
              </Index>
            </tbody>
          </table>
          <form class="restricted-file-add" onSubmit={add}>
            <input
              value={adding()}
              placeholder="Add a file or folder, e.g. config/keys.json"
              aria-label="File or folder to restrict"
              spellcheck={false}
              onInput={(e) => setAdding(e.currentTarget.value)}
            />
            <button type="submit" class="link-button" disabled={!adding().trim()}>
              Add
            </button>
          </form>
          <Show when={addProblem()}>
            <p class="settings-hint setting-explain warning">{addProblem()}</p>
          </Show>
        </Show>
        <Show when={error()}>
          <p class="settings-hint setting-explain warning">{error()}</p>
        </Show>
      </section>
    </div>
  );
}
