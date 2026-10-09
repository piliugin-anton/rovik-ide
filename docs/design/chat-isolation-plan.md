# Chat isolation: implementation plan

Several chats can run on one project at once. Today they all write the same
files, so one chat (or a command it runs, or an approval that waited) can
silently overwrite another's work. This plan gives each chat a private
**overlay** that stores only its changes, with the project underneath, and
one **landing** path that merges a chat's changes into the project, checks
them, and applies them.

Requirements:
- Overlays hold changes only. Their content lives on disk, and memory is a
  cache, so the 2 GiB virtual-files pool (`vfs/mod.rs`, `CAPACITY`) is not a
  limit.
- No git needed: no git program, and the project need not be a repository.
  Git, when present, only makes some steps cheaper.
- Works without the FUSE / macFUSE / WinFsp driver. The driver makes shell
  commands faster; it isn't required.

## Summary of the design

```
 project on disk (base) ◄── user's editor, git panel, outside tools
   ▲ reads go straight through                 ▲ landing: one at a time,
   │                                           │ merged, verified, checked
 overlay(chat A) · overlay(chat B) ────────────┘
   ▲ in-app tools: Workspace API (no driver)
   ▲ shell commands: copied tree (phase 4) or mounted overlay (phase 7)
```

| Problem | Solved by |
|---|---|
| Chats' tool edits overwriting each other | Overlays (phases 2–3) |
| An approval wait overwriting a file (`execute.rs:798`) | Approvals write into the chat's own overlay (phase 3); the change check in direct mode (phase 0) |
| Shell commands (`fmt`, codegen, `git checkout`) | Commands run in the chat's tree (phases 4, 7) |
| Logic conflicts across files | Checks run on the merged result before landing (phase 5) |
| Two app instances | OS file lock on landing (phase 5) |
| Conflicts found only at the end | Base changes pushed to overlays, merged in the background (phase 6) |
| 2 GiB RAM limit, build output | Content stored on disk; build output and dependency folders on plain disk (phases 2, 4) |
| Crash or restart | Overlay index and landing journal on disk (phases 2, 5) |
| No git, no driver | Built-in hashing, `git2::merge_file` on buffers, copied-tree fallback |

## Phase 0: protect direct mode (A + B)

Direct mode (no overlay) remains: for single chats, for users who switch
isolation off, and for the user's own editor. Landing needs this check too.

Tasks
1. `src-tauri/src/file_guard.rs` (new):
   - `PathLocks`: a lock per file path, keyed by the normalised path:
     canonicalise the parent folder, and lowercase on case-insensitive file
     systems. Held only around read → check → write, never across `.await`.
   - `ContentHash` (sha2) and
     `write_if_unchanged(path, expected: Option<Hash>, content) -> Result<(), Changed { current }>`.
2. Use it in `agent_tools::tool_edit_file` / `tool_write_file`:
   - Record what each run has read: a map from path to hash, filled by
     `read`, by `read_symbol`, and by the write tools themselves.
   - A whole-file `write` over an existing file must match the recorded hash.
   - An `old_text` edit is accepted if `old_text` still matches exactly once,
     even if the file changed elsewhere.
3. Approvals (`execute.rs`, `prepare_edit_review` → the `Partial` /
   `Allow` write):
   - Store the hash of `EditProposal.current`.
   - When writing, if the file changed, re-apply the accepted hunks to the
     current file with a three-way merge (phase-5 engine, brought forward):
     base = `current`, ours = accepted content, theirs = disk.
   - If the hunks conflict, show the user a fresh review instead of writing.
4. Editor saves (`filesystem.rs:687 write_file_content`): take the hash the
   tab loaded. On a mismatch, return an error the frontend turns into
   "changed on disk: Reload / Overwrite / Compare" (`Editor.tsx`).
5. Error wording for the model: what changed, plus the current content (or
   a diff for large files), so it can retry without a separate re-read.

Tests
- e2e harness (`agent_run/e2e_tests.rs`): two chats, same file; an approval
  waiting while another chat writes; edits whose `old_text` still matches.
- Unit: path normalisation, edits that race each other (two threads).

Done when: the overwrite at `execute.rs:798` can't happen, and every
in-app write path checks the file first. **Size: S.**

## Phase 1: the `Workspace` interface (refactor, no behaviour change)

Today every tool takes `root: &str` and uses `std::fs`. Put that behind an
interface so a chat can be pointed at an overlay.

```rust
pub trait Workspace: Send + Sync {
    fn read(&self, rel: &RelPath) -> io::Result<Vec<u8>>;
    fn write(&self, rel: &RelPath, data: &[u8], expect: Option<Hash>) -> Result<(), WriteError>;
    fn metadata(&self, rel: &RelPath) -> io::Result<Meta>;
    fn list_dir(&self, rel: &RelPath) -> io::Result<Vec<DirEntry>>;
    fn walk(&self, opts: WalkOpts) -> Box<dyn Iterator<Item = WalkEntry> + '_>; // honours ignore rules
    fn remove(&self, rel: &RelPath) -> io::Result<()>;
    fn rename(&self, from: &RelPath, to: &RelPath) -> io::Result<()>;
    fn command_dir(&self) -> CommandDir;    // where shell commands run (phases 4, 7)
    fn display_root(&self) -> &Path;        // what the model and UI see as the root
}
```

Tasks
1. `src-tauri/src/workspace/mod.rs` with `RelPath` (relative, checked,
   rejects `..`; moves the check out of `resolve_within_root`) and
   `DirectWorkspace` (today's code plus the phase-0 checks).
2. Move to `&dyn Workspace` (or `Arc<dyn Workspace>`):
   - `agent_tools.rs`, `read_file.rs`, `read_symbol.rs`, `edit_file.rs`
     callers, `code_search.rs` (its walker goes through `walk`),
     `outline/`, `repo_map/`, `agent_run/tools.rs`, `agent_run/execute.rs`.
   - `Run` (`run.rs:93`, `root: Option<String>`) gets a
     `workspace: Option<Arc<dyn Workspace>>`.
3. Tauri commands the frontend calls with `root` keep their signatures and
   build a `DirectWorkspace` inside.

Tests: the existing suite passes unchanged. That is the whole point of this
phase. **Size: M** (wide but mechanical).

## Phase 2: the overlay core (pure Rust, tested on every OS)

New module `src-tauri/src/overlay/`:

| File | Contents |
|---|---|
| `blobs.rs` | Content store keyed by hash at `<app data>/overlays/blobs/<sha256>`; written with `atomic_file::write_atomic`; reference counts and cleanup; an in-memory cache shared by all overlays (default 512 MiB) |
| `entry.rs` | `Entry { Modified{content, base, meta}, Created{content, meta}, Deleted, ReplacedDir, Moved{from}, Symlink{target} }`; `BaseRef { Blob(hash), Absent }` (+ `GitBlob(oid)` later, optional) |
| `index.rs` | Each overlay's entries + `read_set`, saved as `<app data>/overlays/<chat>/index.json` (versioned format); rewritten atomically, debounced, and always on close / end of a turn |
| `tiers.rs` | Which tier a path belongs to: `Source`, `BuildOutput`, `Dependencies`. Built-in list + `.gitignore`/`.ignore` via the `ignore` crate (no git needed) + `project_profile` rules |
| `fs.rs` | `OverlayFs`: resolve a path (entry → else base on disk), copy on first write (none on truncate), merged folder listing (cached and refreshed by the watcher), `Moved` folders, deletes, stable inode table |
| `budget.rs` | Limits per chat (soft 80% warning → event; hard → `NoSpace`) for in-memory and on-disk bytes |
| `workspace.rs` | `OverlayWorkspace: Workspace`; `walk` merges the base walk with entries |

Rules
- Build-output and dependency tiers are not stored as entries. They map to
  `<app data>/overlays/<chat>/out/<path>`, a plain folder. Dependencies are
  read from the project's copy until the chat first writes there, then the
  chat gets its own copy (phase 4 decides how).
- The base version is recorded the first time a chat writes a file. The
  merge needs it, so it is never dropped while the entry exists.
- Binary files: content stored as is; merges take a whole side (same as
  `git_conflict_file`).

Tests (like `vfs/store.rs`'s): copy on first write; delete then recreate;
renaming a base folder; listings that merge both layers; restart reload;
budget warning and hard limit; a property test that compares random
operations on `OverlayFs` with the same operations on a real temporary
folder copy.

**Size: L.**

## Phase 3: connect chats to overlays (in-app tools, approvals, UI)

Tasks
1. Isolation mode per chat (`Direct` | `Isolated`), stored in `chat_db`
   (`schema.rs` migration). App setting: default `Direct` for now (rollout:
   phase 9).
2. When a run starts in an isolated chat, open or create its overlay; the
   `Run.workspace` is an `OverlayWorkspace`. Runs that resume after a
   restart reopen the same overlay from `index.json`.
3. Approvals read and write through the workspace, so the review compares
   with the chat's own view and the write lands in its overlay.
4. Commands: `overlay_status(chat)`, `overlay_diff(chat)` (entries
   compared with their bases, no scan), `overlay_read(chat, path)`,
   `overlay_discard(chat, paths?)`.
5. Frontend:
   - A changes panel per chat (extend `src/lib/agentChanges.ts` +
     `EditDiffBlock.tsx`).
   - A read-only "View as chat X" tab mode in `Editor.tsx`.
   - Overlay status in `ChatList.tsx` (changes, conflicts, budget).
6. Until phase 4, isolated chats run shell commands in direct mode, behind
   an approval prompt that warns about it.

Tests: e2e with two isolated chats editing the same file → both succeed, the
project is unchanged, both diffs are right. **Size: M.**

## Phase 4: shell commands in a copied tree (works everywhere, no driver)

The fallback that makes isolation complete before the mount exists, and
keeps working on machines without a driver.

Tasks
1. `overlay/materialize.rs`: a real folder per chat at
   `<app data>/overlays/<chat>/tree`, built once and then kept in sync:
   - Copy-on-write clone (`clonefile` on APFS, `FICLONE` on btrfs/XFS,
     block cloning on ReFS) where the file system supports it; otherwise a
     plain copy of the **source tier** only.
   - The overlay's entries are applied on top. Writes by in-app tools go to
     both the overlay and the tree.
   - Build output: the tree's `target/` etc. are the chat's `out/` folders.
   - Dependencies: cloned with copy-on-write where possible. Otherwise
     linked to the project's copy **read-only**; a command matching an
     install rule (`npm|pnpm|yarn install`, `pip install`, `cargo add`, …;
     a list in `permissions.rs`) first switches that folder to a private
     copy.
2. `command_tool.rs:168` → `current_dir(workspace.command_dir())`. The
   environment sets `CARGO_TARGET_DIR` (and the same for other tools) to
   the chat's `out/`, and gives each chat its own `PORT` range.
3. Bringing changes back after each command: a `notify` watcher on the tree
   collects changed paths. Each one is compared by hash with the overlay's
   view, then becomes an overlay entry (or a delete). Build-output and
   dependency tiers are skipped. A full scan with an mtime+size prefilter
   is the backup if the watcher overflows.
4. Base changes reach the tree for paths the overlay hasn't touched
   (phase 6 calls `materialize::refresh(paths)`).
5. Git in the tree:
   - If `.git` exists, link it read-only.
   - `permissions.rs` blocks git commands that change state in isolated
     chats (`checkout`, `reset`, `commit`, `stash`, `push`, `clean`, …)
     with a clear message.
   - Without `.git`, nothing to do.
6. Watching for writes to the real project: changes to the project during a
   chat's command that no landing or editor save explains are flagged in
   the chat ("this command wrote outside its sandbox"), with Move to
   overlay / Restore.

Tests: `sed -i`, `cargo fmt`, codegen and `rm` in one chat leave the project
and the other chats untouched; two chats `cargo build` at the same time
without blocking each other. **Size: L.**

## Phase 5: landing

New `src-tauri/src/overlay/land.rs` + `src-tauri/src/merge.rs`.

1. `merge.rs`: move the in-memory three-way merge out of
   `git_sync.rs:796-809` (`git2::merge_file` with diff3 style) and
   `parse_merged` (`git_sync.rs:631`) into one shared function. It needs no
   repository. `git_sync` and landing both call it, and its segments feed
   `ConflictResolver.tsx` / `conflictMerge.ts` unchanged.
2. Landing lock: a `tokio::sync::Mutex` per project + an OS file lock at
   `<app data>/locks/<hash of project path>.lock` (`std::fs::File::try_lock`,
   Rust 1.89+, or the `fs4` crate) so a second app instance waits.
3. Steps:
   1. For each entry, compute the result. Project == base → ours.
      Project == ours → no-op. Otherwise merge. A conflict stops landing and
      goes to resolution (step 5).
   2. **Verify**: a temporary overlay = the merged entries on top of the
      current project, with a tree via phase 4 (or the mount, phase 7). Run
      `project.rs::ordered_checks` there, reusing the chat's `out/` folders.
      If they fail, give the failures to the chat as a fix turn (reusing
      `verify.rs`'s round logic) and don't land.
   3. **Journal**: write `<app data>/overlays/<chat>/landing.json`:
      paths, old-version blob, new-version blob, project hashes. Flush to
      disk.
   4. **Check and apply**, under the per-file locks: every path must still
      match the project hash from step 1, or go back to step 1 (at most 3
      times, then report). Write with `write_atomic`, or keep the inode
      for files open in tabs.
   5. Mark the journal done. Rebase the overlay: landed entries are dropped
      and their files now come from the project.
4. Recovery at startup: an unfinished journal is finished (it contains
   every new version), then reported.
5. Conflict resolution: either an agent turn ("resolve these conflicts",
   with diff3 markers written into the overlay) or the user in
   `ConflictResolver.tsx`, opened on the overlay. Either way the result is
   written to the overlay and landing runs again.
6. Undo landing: apply the journal's old versions, using the same change
   check; if something changed since, merge, or report a conflict.
7. Auto-land per chat: when a run ends, the merge is clean and the checks
   pass, land. Otherwise show **Land** in the changes panel.

Tests: user typing during the checks (step 4 retries); crash injected
between journal and apply; undo after a later edit; conflicting landings
from two chats; landing a binary file; two app instances landing at once.
**Size: L.**

## Phase 6: pushing base changes to overlays (find conflicts early)

1. One watcher per project (`filesystem.rs`, `active_watcher`) feeds an
   overlay hub. Landings and editor saves announce their own changes
   directly, so the reason ("landed by chat B", "saved in the editor") is
   known.
2. For each active overlay:
   - Paths it hasn't touched: refresh the tree (phase 4). If they are in
     `read_set`, add a note to the chat's next turn (at most one note per
     turn, listing files).
   - Paths it has touched: a background merge with the new base.
     - Clean: mark it "can rebase"; the rebase happens before landing.
     - Conflict: a notice to the chat now, with the diff, and a badge in
       the UI.
   - Large changes (more than N paths in one batch, e.g. a branch switch or
     a pull): pause the run and ask the user to Rebase / Discard / Keep.
3. Optional frozen mode: before the app itself writes a file, it gives a
   copy of the old version to active frozen overlays that haven't touched
   it. Outside writers are noticed only afterwards, so frozen mode is
   offered only with copy-on-write snapshots or git (snapshot of tracked
   files). Otherwise it is hidden.

Tests: landing in A creates a conflict notice in B within one turn; notes
only for files that were read; a branch switch pauses the runs.
**Size: M.**

## Phase 7: mounting overlays (speed, no copying)

1. Add an `FsBackend` interface between the drivers (`vfs/fuse.rs`,
   `vfs/macfuse.rs`, `vfs/winfsp_fs.rs`) and `VirtualFs`, so the same
   mount serves `/<virtual files>` from `VirtualFs` and `/chats/<id>/`
   from `OverlayFs`.
2. Reads of untouched files pass through to the project. On Linux 6.9+,
   use FUSE passthrough for files opened only for reading.
3. `command_dir()` returns the mount path when the driver is there.
   Otherwise it falls back to phase 4's tree (chosen per machine, shown in
   `VirtualFilesSettings.tsx`).
4. Advisory locks (`flock`/`fcntl`) inside the mount, so tools that lock
   files inside the source tier work.

Tests: the phase-4 command tests repeated through the mount; a benchmark
comparing `cargo check` / `tsc` in a direct tree, the mount and the copied
tree. **Size: L.**

## Phase 8: hardening

- Linux: run commands in a mount namespace with the overlay mounted **over
  the project's real path**, so full paths can't escape. Use
  `unshare(CLONE_NEWUSER|CLONE_NEWNS)`; fall back to watching for writes if
  user namespaces are off.
- Copy-on-write seeding of `out/` from the project's build folders.
- Language servers per chat for `diagnostics`: started inside the chat's
  tree or mount when first needed, stopped when idle.
- Git, when present: `BaseRef::GitBlob` for files that match HEAD (no base
  copy); read-only `git status/diff/log` in chats; frozen snapshots of
  tracked files.
- Cleanup: deleting or archiving a chat removes its overlay; unreferenced
  blobs are cleared at startup.

## Phase 9: rollout

1. Behind a setting ("Isolate chats"), off by default, through phases 3–6.
2. Then on by default when more than one chat in a project is changing
   files; a single chat keeps working directly unless set otherwise.
3. Auto-land on by default once phase 5's undo is solid.
4. Logging (`api_log.rs` / `agent_run/log.rs`): landing results, conflicts,
   merge retries, budget warnings, writes outside the sandbox.

## Order and dependencies

```
0 ─► 1 ─► 2 ─► 3 ─► 4 ─► 5 ─► 6 ─► 9
                     └──► 7 ─► 8
```

Phases 0–3 already remove every in-app way for chats to overwrite each other.
Phase 4 adds shell commands, phase 5 makes it usable day to day, and phases
6–8 improve speed and how early problems show up.

## Open decisions

1. Default mode when only one chat is active: direct (fast, as today) or
   always isolated (one consistent model).
2. Auto-land, or always ask, when the merge is clean and the checks pass.
3. Dependency installs on file systems without copy-on-write: a private
   copy (disk and time) or a prompt.
4. Whether to offer frozen mode at all (phase 6.3).
5. Budget defaults: memory cache size, per-chat soft and hard limits.
