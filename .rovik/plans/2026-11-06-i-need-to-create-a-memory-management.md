# Manage a project's long-term memory

The memory store can only be written by the agent and counted by Settings: there is no way to
read a project's memories back, fix one that says the wrong thing, or drop one that is stale.
Add four store operations (list with a search, edit title and content, delete one, clear all),
expose them as four Tauri commands behind the existing `MemoryState` store, and put a
"Manage memories…" modal over Settings → Long-term memory: a debounced search box, one row per
memory with Edit and Delete, and a "Clear all memories…" button. Both destructive actions go
through `confirmDialog`.

## Steps

1. Store: list, edit, delete, clear
   - Files: `src-tauri/crates/verified-code-memory/src/store.rs`
   - In `impl MemoryStore`, beside `stats`:
     - `pub fn list(&self, repo: &str, query: &str) -> Result<Vec<MemoryRecord>>`: one prepared
       statement, `WHERE repo = ?1 AND (title LIKE ?2 ESCAPE '\' OR content LIKE ?2 ESCAPE '\'
OR tags_json LIKE ?2 ESCAPE '\' OR files_json LIKE ?2 ESCAPE '\') ORDER BY updated_at
DESC`, the pattern `%query%` with `\`, `%` and `_` escaped so a typed `%` is a `%`; an empty
       query matches every row. Every trust state is listed, deprecated ones included.
     - `pub fn edit(&self, id: Uuid, title: &str, content: &str) -> Result<MemoryRecord>`: trims
       both, `VcmError::EmptyField { field: "title" }` / `"content"` when empty (the checks
       `validate_new_memory` makes), `get(id)`, sets both, `updated_at = Utc::now()`, then
       `self.update(&record)` — whose `memories_au` trigger reindexes the FTS row.
     - `pub fn delete(&self, id: Uuid) -> Result<()>`: `DELETE FROM memories WHERE id = ?1`;
       no row changed is `VcmError::NotFound(id)`.
     - `pub fn clear(&self, repo: &str) -> Result<usize>`: `DELETE FROM memories WHERE repo = ?1`,
       returning how many went.
   - In `impl SharedMemoryStore`, one lock-and-forward wrapper each: `list`, `edit`, `delete`,
     `clear`, written like `stats`.
   - Each gets a doc comment with its `# Errors` line.

2. Store tests
   - Files: `src-tauri/crates/verified-code-memory/tests/integration.rs`
   - With `MemoryStore::open_in_memory()` and the file's `sample()` helper:
     - "listing a project's memories is newest first and leaves other projects out" (two repos,
       an edit bumping one to the top).
     - "the list narrows to what the query mentions" (title, content, a file path, a tag; and a
       query of `%` or `_` matching only a text that really holds one).
     - "an edit rewrites a memory's title and text and leaves its trust state alone" (and the
       edited text is what retrieval now finds).
     - "an edit that would empty the title or the text is refused".
     - "deleting a memory removes it, and deleting it twice is not found".
     - "clearing removes one project's memories and says how many".

3. rovick-core: the shape the page reads, and the operations
   - Files: `src-tauri/crates/rovik-core/src/memory.rs`
   - Add `pub struct MemoryEntry` (`Serialize`, `Clone`, `Debug`, `PartialEq`,
     `#[serde(rename_all = "camelCase")]`): `id: String`, `kind: String` (`MemoryKind`'s
     `Display`: `"episode"`, `"project fact"`, `"procedure"`), `title`, `content`,
     `files: Vec<String>`, `tags: Vec<String>`, `verification: String` (`Verification`'s
     `Display`), `evidence: Option<String>`, `updated_at: i64` (millis since the Unix epoch, as
     the protocol's other timestamps are), with `impl From<&MemoryRecord> for MemoryEntry`.
   - Add beside `stats`: `fn id(text: &str) -> Result<Uuid, String>` (a text that isn't a UUID is
     an error naming it), then
     `pub fn list(store: &SharedMemoryStore, repo: &str, query: &str) -> Result<Vec<MemoryEntry>, String>`,
     `pub fn edit(store: &SharedMemoryStore, id: &str, title: &str, content: &str) -> Result<(), String>`,
     `pub fn remove(store: &SharedMemoryStore, id: &str) -> Result<(), String>`,
     `pub fn clear(store: &SharedMemoryStore, repo: &str) -> Result<u32, String>` (a count too big
     for `u32` is an error, not a silent cut).
   - Re-export `SharedMemoryStore`: drop the private `use verified_code_memory::SharedMemoryStore;`
     at line 11 and name it in the `pub use verified_code_memory::{…}` line instead (the file
     having both would define the name twice), so `stats` and the new helpers use it, and the
     commands name it as their helper's return type while the app crate keeps depending on
     rovik-core only.
   - `#[cfg(test)] mod tests` in the same file, over `SharedMemoryStore::open_in_memory()`:
     "an entry names its kind and state as the chat's panel does", "the list is this project's
     memories, newest first", "a search matches part of a title, the text or a file",
     "an edited memory is listed with its new title", "a memory id that is not one is an error",
     "clearing says how many memories went".

4. The four commands
   - Files: `src-tauri/src/commands/memory.rs`
   - `fn store(core: &Core, root: &str) -> Result<SharedMemoryStore, String>`: `core.memory.open`
     for the project, or the page's error text; `memory_stats` uses it too, so all five open the
     same way.
   - `memory_list(core, root: String, query: String) -> Result<Vec<MemoryEntry>, String>`,
     `memory_edit(core, root, id: String, title: String, content: String) -> Result<(), String>`,
     `memory_delete(core, root, id: String) -> Result<(), String>`,
     `memory_clear(core, root: String) -> Result<u32, String>` — each opening the store inside
     `crate::util::blocking`, as `memory_stats` does, since it reads a file.
   - The app crate needs GTK/WebKit, so these are covered by the core tests they call, not by
     tests of their own.

5. Register the commands
   - Files: `src-tauri/src/lib.rs`
   - `commands::memory::memory_list`, `memory_edit`, `memory_delete`, `memory_clear` in the
     `generate_handler!` list, beside `commands::memory::memory_stats`.

6. lib/memory.ts: the page's calls
   - Files: `src/lib/memory.ts`
   - `export interface MemoryEntry` mirroring step 3 (`id`, `kind`, `title`, `content`, `files`,
     `tags`, `verification: MemoryState`, `evidence?`, `updatedAt: number`).
   - `memoryList(root, query): Promise<MemoryEntry[]>`,
     `memoryEdit(root, id, title, content): Promise<void>`,
     `memoryDelete(root, id): Promise<void>`,
     `memoryClear(root): Promise<number>` — each an `invoke` of the command in step 4.
   - `export function memoryStateLabel(state: string): string`, the map
     `verified → verified`, `provisional → provisional`, `deprecated → invalidated`, so the manager
     and the chat's panel read a state the same way.

7. The chat's panel uses the shared label
   - Files: `src/components/PriorWorkPanel.tsx`
   - Drop its local `STATE_LABEL` and call `memoryStateLabel(memory.verification)`; nothing it
     renders changes.

8. The manager modal
   - Files: `src/components/MemoryManager.tsx` (new)
   - `export default function MemoryManager(props: { root: string; onClose: () => void })`:
     - A `typed` signal for the input and a `search` signal its `window.setTimeout` sets after the
       pause (150 ms, a named constant), `onCleanup` clearing the pending timer — the same pair of
       calls `ChatPanel`'s search box makes.
     - `createResource(search, (query) => memoryList(props.root, query).catch(() => null))`; rows
       drawn from the resource's `latest`, so typing keeps the last rows on screen.
     - A `modal-overlay` + `modal memory-manager` with `role="dialog"`, `aria-modal="true"`,
       `aria-label="Memories in this project"`, the `modal-header` (title and a `×` close button)
       and a `modal-body`; the search input takes the focus as it opens (a ref and `onMount`, as
       `ConfirmDialog` focuses its safe button).
     - Its `onKeyDown` wraps Tab at its ends — past the last control to the first, Shift+Tab from
       the first to the last — as `ConfirmDialog`'s does over its own controls, since the settings
       page's controls stay mounted and focusable behind the overlay.
     - A `window` keydown listener, added in `onMount` in the capture phase (`{ capture: true }`,
       removed in `onCleanup`), takes the keys that handler cannot see: a press whose target is
       outside the dialog never reaches it. On Escape — wherever the press starts, a click on a
       memory's text leaving the focus on the page behind — it calls `e.preventDefault()` and
       `e.stopPropagation()` before `props.onClose()`, so `SettingsDialog`'s window listener never
       sees the key and the page stays on Long-term memory. On Tab with the event's target outside
       the dialog it calls `e.preventDefault()` and puts the focus on the search box, so no Tab
       moves into the page's controls behind the overlay and the next Escape starts inside. The
       listener is silent while a confirm dialog the manager opened is on screen — a `confirming`
       signal set around each `confirmDialog` await — which takes its own keys.
     - Tools row: the search input (`aria-label="Search this project's memories"`,
       `placeholder="Search memories"`) with a `×` that clears it, and a
       `link-button danger-link` "Clear all memories…".
     - One `<li class="memory-entry">` per memory: a state chip (the existing
       `prior-work-state` classes with `verified`/`provisional`/`deprecated` and `evidence` as its
       title), `kind`, the title, the date (`new Date(updatedAt).toLocaleDateString()`), the
       content, its files, and "Edit" and "Delete" `link-button`s.
     - Edit: `draft` signal holding `{ id, title, content }`; the row swaps to an `input`
       (`aria-label="Memory title"`), a `textarea` (`aria-label="Memory text"`) and Cancel/Save.
       Save trims both, refuses either empty with a message, calls `memoryEdit`, then closes the
       form and refetches; a failure keeps the form with its values and says so.
     - Delete: `confirmDialog({ title: 'Delete this memory?', message: <the title>,
confirmLabel: 'Delete', danger: true })`, then `memoryDelete` and a refetch.
     - Clear: `confirmDialog({ title: 'Clear this project's memory?', message: 'Every memory this
project keeps is deleted, not only the ones shown here.', confirmLabel: 'Clear memory',
danger: true })`, then `memoryClear`, a notice `Cleared N memories.` (steps 6 and
       `plural`) and a refetch.
     - Lines for: reading (`Reading the memories…`), a failed read (`The memories couldn't be
read.`), nothing yet (`No memories yet: the agent adds them as it works.`), no match
       (`No memories match “…”`), and any failed save/delete/clear (`role="alert"`).

9. Styles for the manager
   - Files: `src/styles.css`
   - Beside the other modal rules: `.modal.memory-manager` (roomier than a 420px dialog,
     `width: min(720px, calc(100vw - 48px))`), `.memory-manager-tools`,
     `.memory-manager-search` (input and its clear button), `.memory-list`, `.memory-entry` and
     its head, title, date, content and files, `.memory-entry-edit` (input, textarea, actions) —
     in the file's existing variable-based style. One more, for the destructive button step 8
     names: `.memory-manager .danger-link { color: var(--error); }`, the colour the file already
     gives a destructive action.

10. Component tests for the manager
    - Files: `src/components/MemoryManager.test.tsx` (new)
    - `__TAURI_INTERNALS__.invoke` answers `memory_list` (recording each `{ root, query }`),
      `memory_edit`, `memory_delete` and `memory_clear`, from a fixture list; the render mounts
      `MemoryManager` with `ConfirmDialogHost` beside it, as `GitHooksView.test.tsx` does. The
      search's pause is crossed through the DOM window (`src/test/dom.ts` installs happy-dom's
      `Window` as the global `window`, and leaves `setTimeout` out of the globals it copies, so the
      component's `window.setTimeout` is happy-dom's while a test's own `setTimeout` is Node's): the
      test saves `window.setTimeout` and `window.clearTimeout`, puts its own pair in their place —
      `setTimeout` keeping the callback under a numeric id it returns, `clearTimeout` dropping it —
      and restores both when the test ends, so its `flush` and everything else are unaffected.
    - Tests: "the manager lists the project's memories with their state, kind and date"; "the
      search asks nothing until its pause ends, then once with what was typed" (type, flush,
      assert no `memory_list` for the new text, fire the held timer, flush, assert one with the
      final text); "editing a memory saves its trimmed title and text and shows it again"; "an
      edit that fails says so and keeps what was typed"; "a blank title or text is not saved";
      "deleting asks first, and cancelling keeps the memory"; "deleting confirmed removes it and
      lists again"; "clearing asks first and says how many went"; "a search with no match says so,
      and an empty project says it has none"; "a list that can't be read says so"; "Escape closes
      the manager after a click on a memory's text, and a listener on the window that the page
      behind would use never sees the key"; "Tab wraps between the first and last control, and a
      Tab pressed after a click on a memory's text lands on the search box".

11. The way in, and the docs
    - Files: `src/components/MemorySettings.tsx`, `src/components/MemorySettings.test.tsx`,
      `README.md`
    - A `managing` signal; a "Manage memories…" `link-button` in the "Memories" `SettingRow`
      beside the count (which therefore refreshes when the manager closes:
      `setManaging(false); void refetch()`); `<Show when={managing()}><MemoryManager root={props.root} onClose={…} /></Show>`.
    - The test answers `memory_list` in its `invoke` stub and adds "long-term memory: Manage
      memories… opens the manager for the open project" (the dialog with the search box appears,
      and the list is asked for this project's root) and "closing the manager counts again".
    - `README.md`'s "Long-term memory" section (the paragraph on what Settings shows) says the page
      also lists a project's memories — search, edit a memory's title and text, delete one, clear
      the project's — and its "nothing else writes to a project's memory" line no longer holds,
      since the user does.

## Decisions

- A modal, not a second OS window: the app builds exactly one `WebviewWindow` per process
  (`src-tauri/src/lib.rs`), and every dialog here is an in-app modal. The user chose this.
- Editing covers the title and the content, as the user chose; tags, files, evidence and the
  trust state stay as the agent left them.
- Substring search over title, content, tags and files rather than the crate's FTS
  (`build_fts_query` needs words of three or more letters and indexes title, content and tags
  only, so a file path a memory names would find nothing). These are one project's rows in a
  per-project file, so a scan is cheap.
- Invalidated memories are listed too, with the panel's "invalidated" chip: the point of the page
  is to see and remove them.
- Delete and clear are permanent, as asked, and both ask first through `confirmDialog`.
- The manager reuses `prior-work-state` and its state classes for the chip (the same chip, already
  styled) and `memoryStateLabel` for its text; no second chip is written.
- Escape is taken on `window` in the capture phase rather than on the dialog alone: a click on a
  memory's text leaves the focus on the page behind, and `SettingsDialog`'s window listener would
  otherwise take the key and leave the page.
- The search's pause is crossed in the test with a `window.setTimeout`/`window.clearTimeout` pair
  the test installs on happy-dom's window and fires itself, not by sleeping out the real delay.

## Validation

- `npm test`; `npm run typecheck`; `npm run format:check` (fix with
  `npx prettier --write src/lib/memory.ts src/components/MemoryManager.tsx src/components/MemoryManager.test.tsx src/components/PriorWorkPanel.tsx src/components/MemorySettings.tsx src/components/MemorySettings.test.tsx src/styles.css`).
- From `src-tauri/`: `cargo nextest run -p verified-code-memory`, `cargo nextest run -p rovik-core memory`, `cargo clippy -p verified-code-memory -p rovik-core --tests` (no new warnings), and `cargo check -p rovik-ide` where GTK/WebKit are (CI builds the app crate). Format the changed Rust files with `rustfmt --edition 2021`.
- New tests are those in steps 2, 3, 10 and 11; each fails without its step.
- By hand: Settings → Long-term memory → Manage memories…; type in the search and watch the
  pause; edit a memory's title and text; delete one; clear all and cancel once; close and check
  the count on the page matches; click a memory's text and press Escape (the manager closes and
  Settings stays on Long-term memory); Tab round its controls (the focus never leaves it).

## Risks

- `LIKE` is case-insensitive for ASCII only, and matches substrings rather than words: a search
  for "retr" finds "retry", a search for "Ø" needs its exact case.
- Editing a memory while an agent run is recalling it changes what the next run reads; a chat
  that already recalled it keeps its own copy in the panel until that run ends.
- The commands themselves (step 4) have no test: the app crate needs GTK/WebKit. What they do is
  the core functions tested in steps 2 and 3.
