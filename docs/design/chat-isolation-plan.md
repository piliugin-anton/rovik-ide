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

Decisions:

| Question                                  | Decision                                                                                                                                                                  |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A single active chat: direct or isolated? | **Always isolated.** Every agent run works in its own overlay. Direct writes are left only for the user's editor and for landing.                                         |
| Landing: automatic or ask?                | **Automatic when possible:** a clean merge and passing checks land without asking. Conflicts, failing checks, or anything else that can't land on its own → ask the user. |
| Dependency installs without cheap copies  | **Use package managers with one shared global store** (section "Dependencies" in phase 4). A private copy only for ecosystems that have no such option.                   |
| Frozen mode                               | **Not offered.** Overlays always see the live project for files they haven't changed.                                                                                     |
| Memory cache                              | **128 MiB**, shared by all overlays.                                                                                                                                      |
| Per-chat limit                            | **512 MiB** of changed files per chat (warning at 80%, writes refused at 100%). Build output and dependencies are not counted; they are watched through free disk space.  |
| Heavy-work slots                          | **Auto, leaning low:** 1 slot; 2 only on 16+ cores and 32 GB+ memory while the machine has room (Q4). More only by manual setting.                                        |
| Terminal panel                            | **Counted.** A heavy command in your terminal takes a slot and is never slowed; chats wait instead, with a note in the chat naming the terminal command (Q4, Q5).         |

## Summary of the design

```
 project on disk (base) ◄── user's editor, git panel, outside tools
   ▲ reads go straight through                 ▲ landing: one at a time,
   │                                           │ merged, verified, checked
 overlay(chat A) · overlay(chat B) ────────────┘
   ▲ in-app tools: Workspace API (no driver)
   ▲ shell commands: copied tree (phase 4) or mounted overlay (phase 7)
```

| Problem                                                 | Solved by                                                                                        |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Chats' tool edits overwriting each other                | Overlays (phases 2–3)                                                                            |
| An approval wait overwriting a file (`execute.rs:798`)  | Approvals write into the chat's own overlay (phase 3); the change check in direct mode (phase 0) |
| Shell commands (`fmt`, codegen, `git checkout`)         | Commands run in the chat's tree (phases 4, 7)                                                    |
| Logic conflicts across files                            | Checks run on the merged result before landing (phase 5)                                         |
| Two app instances                                       | OS file lock on landing (phase 5)                                                                |
| Conflicts found only at the end                         | Base changes pushed to overlays, merged in the background (phase 6)                              |
| 2 GiB RAM limit, build output                           | Content stored on disk; build output and dependency folders on plain disk (phases 2, 4)          |
| Crash or restart                                        | Overlay index and landing journal on disk (phases 2, 5)                                          |
| No git, no driver                                       | Built-in hashing, `git2::merge_file` on buffers, copied-tree fallback                            |
| Several chats building at once and freezing the machine | Queue for heavy work (track Q): find heavy commands, give them slots, slow down the rest         |

## Phase 0: protect direct mode (A + B)

Chats will always be isolated, but direct writes remain in two places: the
user's own editor and landing. Both need this check. It also protects
agent runs today, until isolation ships (phase 9).

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

| File           | Contents                                                                                                                                                                                                                                                                                    |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `blobs.rs`     | Content store keyed by hash at `<app data>/overlays/blobs/<sha256>`; written with `atomic_file::write_atomic`; reference counts and cleanup; an in-memory cache shared by all overlays (**128 MiB**, least recently used dropped first)                                                     |
| `entry.rs`     | `Entry { Modified{content, base, meta}, Created{content, meta}, Deleted, ReplacedDir, Moved{from}, Symlink{target} }`; `BaseRef { Blob(hash), Absent }` (+ `GitBlob(oid)` later, optional)                                                                                                  |
| `index.rs`     | Each overlay's entries + `read_set`, saved as `<app data>/overlays/<chat>/index.json` (versioned format); rewritten atomically, debounced, and always on close / end of a turn                                                                                                              |
| `tiers.rs`     | Which tier a path belongs to: `Source`, `BuildOutput`, `Dependencies`. Built-in list + `.gitignore`/`.ignore` via the `ignore` crate (no git needed) + `project_profile` rules                                                                                                              |
| `fs.rs`        | `OverlayFs`: resolve a path (entry → else base on disk), copy on first write (none on truncate), merged folder listing (cached and refreshed by the watcher), `Moved` folders, deletes, stable inode table                                                                                  |
| `budget.rs`    | Limit per chat: **512 MiB** of changed files (source tier). Warning event at 80%; at 100% writes fail with `NoSpace` and a clear message to the model and the user. Build output and dependencies are not counted: they're watched through free disk space (warning under 10% or 5 GB free) |
| `workspace.rs` | `OverlayWorkspace: Workspace`; `walk` merges the base walk with entries                                                                                                                                                                                                                     |

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

1. Every chat is isolated. While the work is in progress, a hidden
   development setting (`isolate_chats`, off) switches it on; it is removed
   in phase 9. Store the chat's overlay id in `chat_db` (`schema.rs`
   migration).
2. When a run starts, open or create the chat's overlay; the
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
6. Until phase 4, isolated chats (only with the development setting on) run
   shell commands in direct mode, behind an approval prompt that warns about
   it.

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
   - Dependencies: the project's folders, **read-only**, until the chat
     installs something; then see "Dependencies" below.
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

### Dependencies: package managers with one shared store

Decision: dependency installs in chats go through package managers that keep
one global store, so a chat's own dependency folder is made of links, not
copies. A private copy is made only where an ecosystem has no such option.

1. **Already shared, nothing to do.** These ecosystems keep dependencies in
   a global store outside the project: Rust (cargo), Go modules,
   Maven / Gradle, NuGet, Dart pub, Haskell cabal / stack, Julia, Zig,
   Conan, Ruby Bundler (default settings), R renv. An install in a chat
   changes only manifests and lockfiles (source tier, landed as usual) and
   the global store. The store is only ever added to, so sharing it is
   safe.
2. **JavaScript:**
   - pnpm, Bun or Yarn 4 (PnP) → the chat gets its own `node_modules` (or
     PnP files) in its `out/` folder, linked from the global store.
   - npm → no shared store. On the first install in a chat, suggest
     switching the project to pnpm, **once per project**. It replaces
     `package-lock.json` with `pnpm-lock.yaml`, which affects everyone on
     the repository, so it is never done without the user's yes. If the
     user says no: a private copy of `node_modules` (copy-on-write where
     the file system supports it).
3. **Python:**
   - uv → the chat's own venv in `out/`, filled from uv's global cache.
     It needs **no project change**: it reads the same
     `requirements.txt` / `pyproject.toml`. If uv isn't installed, suggest
     installing it.
   - In chats, `pip install …` is refused (`permissions.rs`) with "use
     `uv pip install …`", and the system prompt says so. Without uv, the
     plain command is allowed and the venv is copied privately.
   - conda / pixi: shared package cache, linked the same way.
4. **No shared option (PHP Composer, Elixir mix, vcpkg manifest mode):**
   a private copy, made only when the chat installs (copy-on-write where
   possible).
5. **Protecting the shared store.** On ext4 and NTFS the links are **hard
   links**: the same file as in the store. A command that edits one in
   place (debug patching, `patch-package`) would change it for every
   project. So:
   - Use clone mode where the file system supports it (pnpm
     `package-import-method=clone-or-copy`, `UV_LINK_MODE=clone`,
     `bun install --backend=clonefile`).
   - Otherwise, after each install, mark the chat's linked dependency
     files **read-only** (the shared inode, so the store is protected
     too). An in-place write then fails. Tools that save via a temporary
     file and rename only break the link and never touch the store. The
     package managers themselves re-link and never write into files.
   - In the mount (phase 7), writes to the dependency tier are copied
     first, as for any other file.
6. **Suggestions:** `project_profile` already detects the package manager
   (`workspaces.rs:15 detect_package_manager`). A one-time card per project
   offers "Install uv" or "Switch to pnpm", with what changes and who it
   affects. Installing runs the official installer **only after the user
   approves**.
7. A chat's dependency folders are deleted when it lands or is closed.

Tests: install in chat A doesn't change the project's or chat B's
dependencies; a hard-linked file is read-only after install; `sed -i` on a
linked file breaks the link and leaves the store intact; `pip install` in a
chat is refused with the uv hint; the npm → pnpm suggestion appears once per
project.

Tests (phase 4 as a whole): `sed -i`, `cargo fmt`, codegen and `rm` in one chat leave the project
and the other chats untouched; two chats `cargo build` into their own
`out/` folders without sharing a cargo lock (track Q decides whether they
run at the same time). **Size: L.**

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
7. **Automatic when possible, otherwise ask.** When a run ends, the merge
   is clean and the checks pass, land without asking and show "Landed N
   files · Undo" in the chat. Otherwise the chat shows why it didn't land
   (conflicts, failing checks after the fix rounds, budget, a command that
   wrote outside its sandbox) with **Land anyway** / **Resolve** /
   **Discard**. Nothing that failed lands without the user's yes.

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
3. No frozen mode (decision): files a chat hasn't changed always show the
   live project.

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
  copy); read-only `git status/diff/log` in chats.
- Cleanup: deleting or archiving a chat removes its overlay; unreferenced
  blobs are cleared at startup.

## Phase 9: rollout

1. The hidden development setting `isolate_chats` stays off through
   phases 3–6 and Q-a…Q-c.
2. Then isolation is switched on for **every** chat, and the setting is
   removed. Chats that existed before get an empty overlay the next time
   they run; what they already wrote stays in the project as it is.
3. Auto-land (phase 5.7) is on from the start of isolation; Undo landing
   must be done first.
4. Logging (`api_log.rs` / `agent_run/log.rs`): landing results, conflicts,
   merge retries, budget warnings, writes outside the sandbox.

## Track Q: a queue for CPU-heavy work

Separate from overlays and useful on its own: even today, in direct mode,
three chats running `cargo build` at once can freeze a laptop. Isolation
makes this more likely, because each chat builds into its own folder and
nothing makes them wait for each other.

You can't know for certain in advance whether a command will be heavy: it
depends on caches and inputs (`cargo build` with nothing changed takes a
second; after `cargo clean` it takes minutes). So detection has four layers.
The first three **predict** before the command starts; the fourth
**observes** it while it runs and is the safety net for wrong predictions.

### Q1. Predicting from the command text (`src-tauri/src/workload/classify.rs`)

Each command gets one class: `Heavy`, `Light`, `LongRunning` (dev servers,
watchers) or `Unknown`.

1. Split the command with the existing parser (`permissions.rs:169
parse_command`) into segments (`&&`, `;`, `|`), skipping `cd`, `env`
   and `VAR=x` prefixes. A command's class is the heaviest of its
   segments. Commands the parser marks `opaque` (`$(...)`, backticks,
   `eval`) → `Unknown`.
2. A rule table of program + subcommand, stored as data and editable in
   settings:

   | Class       | Examples                                                                                                                                                                                                                                                                         |
   | ----------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
   | Heavy       | `cargo build/check/test/clippy/run/bench/doc/install`, `rustc`, `go build/test`, `tsc`, `vite build`, `webpack`, `next build`, `jest`, `vitest run`, `pytest`, `make`, `ninja`, `cmake --build`, `gradle`, `mvn`, `dotnet build/test`, `msbuild`, `docker build`, `gcc/clang/cc` |
   | LongRunning | `vite` / `vite dev`, `next dev`, `cargo watch`, any `--watch` / `-w`, `npm start`, `serve`, `tauri dev`                                                                                                                                                                          |
   | Light       | `ls`, `cat`, `head`, `grep`, `rg`, `find`, `sed`, `git` (except `gc`/`repack`), `echo`, `cargo fmt`, `cargo metadata`, `prettier --check` on a few files                                                                                                                         |

3. Follow indirection, at most 3 levels deep:
   - `npm|pnpm|yarn|bun run <s>` / `npm test` → the script body from
     `package.json` (the workspace helpers in `project_profile/workspaces.rs`
     already read it), including `run-s`, `run-p`, `concurrently`. A script
     named `dev`, `start`, `serve` or `watch` → `LongRunning`.
   - `npx` / `pnpm exec` / `yarn dlx <tool>` → classify `<tool>`.
   - `just <r>` / `task <r>` → the recipe from `justfile` / `Taskfile.yml`.
   - `./script.sh` / `sh|bash script.sh` inside the project → classify the
     script's lines.
   - `make <target>` → `Heavy` (Makefiles are too varied to read).
4. The project's own checks (`project.rs`, `ordered_checks`) are always
   `Heavy`: their commands are known exactly.

### Q2. Learning from past runs (`workload/history.rs`)

The most accurate prediction is what the same command cost last time.

- Every finished command records a **fingerprint**: program, subcommand,
  script name and flags, with paths and file names stripped. For example,
  `cargo test -p foo some_test` → `cargo test -p foo`.
- Saved per fingerprint: CPU seconds, peak memory, wall time and average
  cores used (CPU ÷ wall), as running averages. Store the 75th percentile
  per project in `chat_db` (a schema migration).
- With 3+ samples, history overrides the Q1 rules:
  `Heavy` if the 75th percentile is over **15 CPU seconds**, or over
  **1.5 cores** on average, or over **1 GB** peak memory.
- Memory history is also used for admission (Q5): don't start a job whose
  usual peak doesn't fit in free memory.

### Q3. Watching a command's process tree (`workload/tracker.rs`)

Every command (the agent's, the checks', and the terminal panel's) runs in a container the app can measure, slow down and pause as
a whole:

| OS                                                       | Container                                                                    | Measures                                                                                                         | Slows down / pauses                                                                                   |
| -------------------------------------------------------- | ---------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Linux (cgroup v2 delegated, which is usual with systemd) | One cgroup per command                                                       | `cpu.stat`, `memory.peak`, exactly, including daemons that left the process group                                | `cpu.max` (CPU cap), `cgroup.freeze`                                                                  |
| Linux fallback, macOS                                    | Process group (`process_group(0)`, which `command_tool.rs:185` already sets) | Poll the group's processes (`/proc` on Linux, `proc_listpids` / `proc_pidinfo` through `libc` on macOS)          | `setpriority` (lower priority), `SIGSTOP`/`SIGCONT`                                                   |
| Windows                                                  | Job Object: start the process suspended, assign it to the job, resume        | `JobObjectBasicAccountingInformation` (CPU of the whole tree), `JobObjectExtendedLimitInformation` (peak memory) | `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION` (hard CPU cap); this also replaces `taskkill` in `kill_tree` |

The tracker samples every 500 ms. A command becomes **observed heavy** when:

- it averages over 1.5 cores across 3 seconds, or
- its memory goes over 1.5 GB, or
- a known heavy program appears in its tree: `rustc`, `cc1`, `cc1plus`,
  `clang`, `ld`, `lld`, `link.exe`, `javac`, `kotlinc`, `swiftc`,
  `esbuild`, `go` (`compile`), or `node` running `tsc`, `webpack` or
  `jest`. This one triggers at once, without the 3-second window.

### Q4. The scheduler (`workload/queue.rs`)

- **Slots.** Default "auto", leaning low (decision): **1 slot**. 2 only on
  machines with **16+ cores and 32 GB+ memory**, and never more than 2
  automatically. Even then the second slot opens only while free memory is
  over 8 GB and the user's own processes leave at least half the cores idle;
  otherwise auto drops back to 1. A higher number is a manual setting. Each
  slot is a lock file
  `<app data>/locks/heavy-slot-N.lock` (`try_lock`), so several app windows
  share them.
- **Predicted `Heavy`:** waits for a slot before it starts.
- **`Light` / `Unknown`:** starts at once. If the tracker sees it turn
  heavy and no slot is free, it is **slowed down** (CPU capped at about
  one core, lowest priority) until a slot frees up; then the limit is
  lifted. It is **paused** (freeze, `SIGSTOP`) only when memory is
  running low, because a paused process can trip network or test
  timeouts.
- **`LongRunning`:** starts at once and holds a slot only while it's busy.
- **A slot follows activity, not the process's lifetime.** It's released
  when the tree has been idle (under 0.3 cores) for 5 seconds, even if the
  process is still running, and taken again the next time it gets busy.
  So a dev server that only works hard while starting doesn't hold a slot
  forever.
- **Priority:** landing checks (phase 5) → commands from the chat the user
  is looking at → background chats; first come, first served within each.
- **Memory admission:** don't start the next heavy job unless free memory ≥
  its usual peak (Q2) + 1 GB.
- **Fair CPU split** when 2+ slots run: `CARGO_BUILD_JOBS`,
  `MAKEFLAGS=-jN`, `GOMAXPROCS` = cores ÷ running heavy jobs. Lower
  priority for agent work, so the editor stays responsive.
- **Timeouts:** time spent waiting, slowed or paused doesn't count toward
  the command's timeout (`command_tool.rs`, `CHECK_TIMEOUT_MS` in
  `verify.rs`). Stop cancels a wait at once.
- **The user's terminal counts too** (decision). The terminal panel's shell
  (`terminal.rs`) runs in a tracked container like any command, but only
  for measuring: the user is never made to wait or slowed down.
  - When the terminal turns heavy, it takes a slot, even if all slots are
    in use. The agent work holding that slot is slowed down until one
    frees up.
  - What's running is read from the terminal's foreground process (the
    process group in front of the PTY, with its command line), so notes
    can name it, e.g. `cargo build`.

### Q5. User interface

- In the tool call: "Waiting for a CPU slot (chat B: `cargo build`, 40 s)"
  or "Slowed down: chat B is building". Buttons: **Run now** (skip the
  queue) and **Stop**.
- **Notes in the chat** whenever a chat waits or is slowed down, naming
  the cause:
  - "Waiting: your terminal is running `cargo build` (1 min 20 s)"
  - "Slowed down: your terminal is running `npm test`"
  - "Waiting: chat B is running `cargo test`"

  The note updates while it waits and stays in the chat afterwards as
  "Waited 1 min 20 s for your terminal (`cargo build`)", saved with the
  chat like other notes. The model gets one line in the tool result only
  when the wait was over 10 s ("waited 80 s for a CPU slot"), so it
  doesn't mistake the wait for a slow command.

- A small queue indicator in the status bar listing what's running and
  waiting, the terminal included.
- "Treat this command as heavy / light" on a tool call. It writes a rule,
  which takes precedence over history.
- Settings: slots, thresholds, the rule table.

### Limits

- Prediction is a guess. Q3 corrects it within 1–3 seconds, so a wrong
  guess costs a few seconds of contention, not a frozen machine.
- Without delegated cgroups (Linux fallback, macOS), a daemon that leaves
  the process group (`setsid`) escapes measurement. The leftover-process
  check in `command_tool.rs` still sees it.
- macOS has no hard CPU cap for normal apps: only lower priority and
  `SIGSTOP`.
- The user's terminal can be observed, never queued.

### Steps and tests

| Step | Contents                                                                                                    | Size |
| ---- | ----------------------------------------------------------------------------------------------------------- | ---- |
| Q-a  | Tracker (Q3) on all three OSes; record history (Q2). Observe only, no behaviour change                      | M    |
| Q-b  | Classifier (Q1) + scheduler (Q4) for agent commands and project checks                                      | M    |
| Q-c  | Slowing down / pausing; slot released when idle; memory admission; terminal counted; UI and chat notes (Q5) | M    |
| Q-d  | Landing checks get priority (after phase 5)                                                                 | S    |

Tests:

- The rule table and script expansion, on fixtures (`package.json`,
  `justfile`, shell scripts).
- History fingerprints and the 75th-percentile override.
- The tracker on a synthetic busy loop (`sh -c 'while :; do :; done'`):
  detected heavy within 3 s; capping and lifting work; a slot is released
  after it goes idle.
- The queue: priority order; timeouts don't count waiting; Stop while
  waiting; two app instances share slots.

## Order and dependencies

```
0 ─► 1 ─► 2 ─► 3 ─► 4 ─► 5 ─► 6 ─► 9
                     └──► 7 ─► 8
Q-a ─► Q-b ─► Q-c ─────────► Q-d (after 5)
```

Phases 0–3 already remove every in-app way for chats to overwrite each other.
Phase 4 adds shell commands, phase 5 makes it usable day to day, and phases
6–8 improve speed and how early problems show up. Track Q doesn't depend on
overlays and can start at any time; it helps direct mode too.

## Open decisions

None. All decisions are in the table at the top.

## Implementation order for an AI agent

The plan above split into tasks an agent can do one at a time. Each task is
small enough for one branch and one review, leaves the app working, and
says when it is done.

### Rules for every task

1. **One task per branch / PR.** Don't start the next task until the
   current one is merged. Don't mix in refactors from other tasks.
2. **Read first:** the files named in the task, plus this plan's section
   for it. Follow the surrounding code's style: module doc comments that
   explain _why_, small functions, tests next to the code (as in
   `vfs/store.rs`, `agent_run/e2e_tests.rs`).
3. **Keep the app working.** All new behaviour sits behind `isolate_chats`
   (off) until T34. Phase 0 and track Q work in today's mode and need no
   setting.
4. **Checks before every push:**
   - `cd src-tauri && cargo check && cargo test`
   - `npm test`
   - `npx tsc --noEmit`
   - `npm run format:check`

   For a task that touches platform code (T27, T32–T34), also build on
   macOS and Windows, or mark the PR as needing it.

5. **Tests in the same task**: each task's "Done when" names them.
6. **When a task doesn't fit reality** (an API is missing, a file works
   differently than this plan assumes): don't improvise a new design. Add
   a short note under "Notes from implementation" at the end of this file,
   take the smallest safe path, and say so in the PR.
7. **Tauri commands:** a new or changed command is registered in
   `lib.rs` and given a typed wrapper in `src/lib/commands.ts` in the same
   task.

### Tasks

**Milestone 1: chats can't silently overwrite each other's files today (phase 0)**

| ID  | Task                                                                                                                                                              | Needs    | Done when                                                                                                                                     |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| T01 | `file_guard.rs`: `PathLocks` (normalised keys), `ContentHash` (sha2), `write_if_unchanged`                                                                        | –        | Unit tests: same file through different path spellings → same lock; two threads racing → no lost update; mismatch returns the current content |
| T02 | Track what each run read (path → hash). Check it in `tool_write_file` / `tool_edit_file` (the `old_text` rule from phase 0.2), with the error text from phase 0.5 | T01      | e2e: two chats writing the same file → the second gets "changed since you read it"; an `old_text` edit after an unrelated change succeeds     |
| T03 | Move the three-way merge (`git_sync.rs:796-809` + `parse_merged`) into `merge.rs`; `git_sync` calls it                                                            | –        | Existing git_sync tests pass unchanged; new tests on plain buffers (clean, conflict, one side deleted, binary)                                |
| T04 | Approvals: remember the reviewed file's hash. When writing, re-apply the accepted hunks with `merge.rs`; on a conflict, show a fresh review                       | T01, T03 | e2e: another chat writes during an approval wait → both changes are kept; overlapping hunks → a new review, nothing written                   |
| T05 | Editor saves check the hash the tab loaded; "changed on disk: Reload / Overwrite / Compare" in `Editor.tsx`                                                       | T01      | Rust test for the command; manual: saving after an agent edit prompts instead of overwriting                                                  |

**Milestone 2: the heavy-work queue (track Q, useful before isolation)**

| ID   | Task                                                                                                                                                                                        | Needs   | Done when                                                                                                                                                                        |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T06  | Q-a: `workload/tracker.rs`: containers per OS (cgroup → process group fallback; Windows Job Object, replacing `taskkill` in `kill_tree`); sampling only                                     | –       | A busy-loop child is reported over 1.5 cores within 3 s on each OS; a killed tree leaves no processes                                                                            |
| T07  | Q-a: `workload/history.rs`: fingerprints, saved stats, `chat_db` migration                                                                                                                  | T06     | Fingerprint tests (paths stripped); the 75th percentile is computed from saved runs                                                                                              |
| T08  | Q-b: `workload/classify.rs`: rule table and following `npm run`, `just`, scripts; `parse_command` reused                                                                                    | –       | Fixture tests for each rule and indirection; `opaque` → Unknown                                                                                                                  |
| T09  | Q-b: `workload/queue.rs`: slot lock files, priority order, waiting outside the timeout, Stop while waiting; used by `command_tool.rs` and `verify.rs`                                       | T06–T08 | Tests: priority order, timeout excludes waiting, two app instances share slots                                                                                                   |
| T10  | Q-c: slowing down / pausing, slot released after 5 s idle, memory admission, CPU split variables; UI (Q5)                                                                                   | T09     | Busy loop is capped and released; a dev-server-like process frees its slot when idle; tool call shows "Waiting for a CPU slot"                                                   |
| T10a | The terminal panel counted: its shell's tree tracked (`terminal.rs`), takes a slot when heavy and never waits; agent work slowed instead; chat notes name the terminal's foreground command | T10     | A busy loop in the terminal makes a chat's build wait with "Waiting: your terminal is running …"; the note stays in the chat after the wait; the terminal itself is never slowed |

**Milestone 3: the `Workspace` interface (phase 1, no behaviour change)**

| ID  | Task                                                                                                   | Needs    | Done when                                                                        |
| --- | ------------------------------------------------------------------------------------------------------ | -------- | -------------------------------------------------------------------------------- |
| T11 | `workspace/mod.rs`: `Workspace` trait, `RelPath`, `DirectWorkspace` (with T01's check). No callers yet | T01      | Unit tests for `RelPath` (rejects `..`, absolute paths) and `DirectWorkspace`    |
| T12 | Move `agent_tools.rs`, `read_file.rs`, `edit_file.rs` callers, `read_symbol.rs` onto `Workspace`       | T11      | All existing tests pass unchanged                                                |
| T13 | Move `code_search.rs`, `outline/`, `repo_map/` onto `Workspace::walk`                                  | T11      | All existing tests pass unchanged; search results identical on a fixture project |
| T14 | `Run.workspace` in `run.rs`; `execute.rs` / `tools.rs` use it                                          | T12, T13 | All existing tests pass unchanged                                                |

**Milestone 4: the overlay core (phase 2)**

| ID  | Task                                                                                                                                     | Needs    | Done when                                                                         |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------- | -------- | --------------------------------------------------------------------------------- |
| T15 | `overlay/blobs.rs`: content store keyed by hash, 128 MiB cache, reference counts, cleanup                                                | –        | Tests: dedupe, cache eviction, crash-safe writes, cleanup of unreferenced content |
| T16 | `overlay/entry.rs` + `index.rs`: entries, base versions, `read_set`, versioned `index.json`                                              | T15      | Round-trip and reload tests; old format versions are rejected cleanly             |
| T17 | `overlay/tiers.rs`: built-in list + `.gitignore` / `.ignore` + `project_profile` rules                                                   | –        | Fixture tests for Rust, Node and Python layouts                                   |
| T18 | `overlay/fs.rs`: `OverlayFs`: read through, copy on first write, delete, recreate, rename, `Moved` folders, merged listings, inode table | T16, T17 | Property test comparing random operations with a real temporary folder copy       |
| T19 | `overlay/budget.rs`: 512 MiB per chat, 80% warning, free-disk warning                                                                    | T18      | Tests for the warning event and the refused write                                 |
| T20 | `overlay/workspace.rs`: `OverlayWorkspace` incl. merged `walk`                                                                           | T14, T18 | The Workspace test suite from T11 passes against `OverlayWorkspace` too           |

**Milestone 5: chats in overlays (phases 3–4, behind `isolate_chats`)**

| ID  | Task                                                                                                                                                        | Needs | Done when                                                                                                      |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | -------------------------------------------------------------------------------------------------------------- |
| T21 | The `isolate_chats` setting; overlay id in `chat_db`; open the overlay on run start or resume                                                               | T20   | e2e: two isolated chats edit one file → project unchanged, each sees its own version, resumed after restart    |
| T22 | Approvals through the workspace; `overlay_status/diff/read/discard` commands                                                                                | T21   | e2e: an approval writes into the overlay only                                                                  |
| T23 | Frontend: changes panel per chat, "View as chat", status in `ChatList.tsx`                                                                                  | T22   | `npm test` covers the diff and status logic; manual check in the app                                           |
| T24 | `overlay/materialize.rs`: copied tree (copy-on-write or source-tier copy), changes brought back by a watcher, mtime+size scan backup                        | T21   | Tests: `sed -i`, `rm`, codegen in the tree become overlay entries; build folders are ignored                   |
| T25 | `command_dir()`, environment (`CARGO_TARGET_DIR`, `PORT` range), git commands blocked in chats                                                              | T24   | e2e: `cargo fmt` in chat A leaves the project and chat B untouched; `git checkout` is refused with the message |
| T26 | Dependencies (phase 4 "Dependencies"): detection, shared-store installs, read-only linked files, `pip` → `uv` rule, private-copy fallback, suggestion cards | T25   | The tests listed in that section                                                                               |
| T27 | Detect writes outside the sandbox: unexplained changes to the project during a command → chat warning with Move to overlay / Restore                        | T25   | e2e: a command writing the project's full path is flagged                                                      |

**Milestone 6: landing (phase 5)**

| ID  | Task                                                                                                                                                               | Needs         | Done when                                                                                     |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------- | --------------------------------------------------------------------------------------------- |
| T28 | Landing lock (mutex + OS lock file), journal, recovery at startup                                                                                                  | T21           | Crash injected after the journal → finished at next start                                     |
| T29 | Merge per entry, check-and-apply with retries, Undo landing                                                                                                        | T03, T28      | Tests: user typing during landing retries; undo after a later edit merges or reports          |
| T30 | Checks on the merged result in a temporary overlay; failures become a fix turn (`verify.rs` rounds); landing checks get queue priority (Q-d)                       | T24, T29, T09 | e2e: chat B's change breaks with chat A's landed change → not landed, chat B gets the failure |
| T31 | Auto-land when clean and passing; otherwise ask (Land anyway / Resolve / Discard); conflict resolution in the overlay with `ConflictResolver.tsx` or an agent turn | T30, T23      | e2e: clean → landed with Undo; conflict → asks, project unchanged                             |

**Milestone 7: conflicts found early (phase 6)**

| ID  | Task                                                                                                               | Needs    | Done when                                                                    |
| --- | ------------------------------------------------------------------------------------------------------------------ | -------- | ---------------------------------------------------------------------------- |
| T32 | Base-change hub: watcher + announcements from landing and the editor; refresh trees; notes for files the chat read | T24, T29 | e2e: landing in A → note in B's next turn only if B read the file            |
| T33 | Background merges for changed files; immediate conflict notices; pause on large changes                            | T32      | e2e: conflicting landing → notice within one turn; branch switch pauses runs |

**Milestone 8: switch it on (phase 9)**

| ID  | Task                                                                        | Needs                   | Done when                                                                               |
| --- | --------------------------------------------------------------------------- | ----------------------- | --------------------------------------------------------------------------------------- |
| T34 | Every chat isolated: remove `isolate_chats`, handle existing chats, logging | T26, T27, T31, T33, T10 | Full e2e suite passes with isolation on; manual run with three chats on this repository |

**Milestone 9: speed and hardening (phases 7–8)**

| ID  | Task                                                                                                                                                  | Needs    | Done when                                                          |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ------------------------------------------------------------------ |
| T35 | `FsBackend` interface in the VFS drivers; `VirtualFs` implements it                                                                                   | –        | Existing `vfs` tests pass unchanged                                |
| T36 | `OverlayFs` served at `/chats/<id>/`; passthrough on Linux 6.9+; `flock`; `command_dir()` prefers the mount, falls back to the tree                   | T34, T35 | T24–T27 tests pass through the mount; benchmark recorded in the PR |
| T37 | Linux mount namespaces over the project path; copy-on-write seeding of `out/`; language servers per chat; `BaseRef::GitBlob`; cleanup of closed chats | T36      | Tests per item; full paths can't escape on Linux                   |

### Order at a glance

```
M1: T01 ─► T02 ─► T03 ─► T04 ─► T05
M2: T06 ─► T07 ─► T08 ─► T09 ─► T10 ─► T10a     (can run in parallel with M3–M4)
M3: T11 ─► T12 ─► T13 ─► T14
M4: T15 ─► T16 ─► T17 ─► T18 ─► T19 ─► T20
M5: T21 ─► T22 ─► T23 ─► T24 ─► T25 ─► T26 ─► T27
M6: T28 ─► T29 ─► T30 ─► T31
M7: T32 ─► T33
M8: T34   ← isolation on for every chat
M9: T35 ─► T36 ─► T37
```

M1 helps users right away. M2 can run alongside M3–M4 if two agents work at
once; they touch different files. Everything else is sequential.

## Notes from implementation

- **Phase 0 (T01–T05).**
  - A whole-file write over a file the run never read is refused only when
    its current content is another writer's change through the app (the
    ledger in `file_guard.rs`). Requiring a read before every write would
    have changed how existing chats work. A run that did read the file is
    refused on any change since.
  - libgit2's in-memory merge fails with "no error" unless libgit2 was
    initialised first; the Git panel used to do that by accident.
    `merge::merge3_raw` initialises it itself.
  - The editor's dialog offers **Overwrite** and **Compare** (the version
    on disk in a read-only tab, differences from the unsaved text marked)
    instead of Reload / Overwrite / Compare. Reload would throw away the
    unsaved text in one click; it stays reachable by closing the tab.
  - `e2e_tests::plan_mode_asks_and_saves_the_plan` and
    `e2e_tests::a_revised_plan_replaces_the_chats_plan_file` already fail
    on the base branch, unrelated to this work.
- **Track Q (T06–T10a)**, in `src-tauri/src/workload/`.
  - Process trees are measured with the `sysinfo` crate on all three
    systems (descendants of the command's process, plus its process group
    on Unix), not with cgroups or Windows Job Objects. One implementation,
    testable here; daemons that leave both the tree and the group escape
    measurement, as the plan's limits already said for the fallback.
  - "Slowed down" is the lowest scheduling priority (`nice 19`, Windows
    idle priority), not a hard CPU cap: without cgroups there is none, and
    an unprivileged process can't raise its priority again on Unix. It
    only matters while something else wants the CPU, so it isn't undone.
    Pausing under low memory is Unix only (`SIGSTOP`/`SIGCONT`).
  - No lock files for slots: the app is single-instance
    (tauri-plugin-single-instance), so the slots live in one process.
  - History is a JSON file in the app's data folder
    (`workload-history.json`), not a chat database table: it describes
    the machine and the project, not a chat.
  - The note in the chat: live on the tool call (and on a check's row)
    while it waits; kept afterwards only for waits of 10 s or more, as the
    line the model also gets. The terminal's command is its shell's
    newest child process.
  - Slots can be set by the `workload_set_slots` command; there is no
    settings screen for it yet.
- **Milestone 3, revised: the overlay is served as a real folder.** Every
  agent tool already takes the project root as a path, and FUSE mounts
  work (also in the container this was built in). So a chat's overlay is
  mounted through the existing drivers at its own folder, and the chat's
  run simply uses that folder as its root: `read`, `write`, `search_code`,
  the repository map, commands and checks all see the chat's view
  without each going through a `Workspace` interface. That replaces
  T11–T14 (a trait threaded through every tool) with T35 brought forward:
  the drivers now take any `vfs::backend::Backend`, the in-memory
  `VirtualFs` being one. Without a driver, the chat's view is the copied
  tree of phase 4 at the same place, so tools don't care which it is.
- **Milestone 4 (overlay core), in `src-tauri/src/overlay/`.**
  - `OverlayFs` implements the drivers' `Backend`. A chat's changed files
    are plain files in its folder (`upper/`, with `base/` copies for
    merging); there is no separate content store keyed by hash and no
    128 MiB in-app cache: the OS caches those files, and the app's memory
    holds only the folder structure. Identical edits in two chats are not
    shared on disk.
  - A rename over a project file (how `sed -i` and most editors save)
    continues that file: same origin, same merge base.
  - Folders are listed from the project when first needed and again on a
    missed lookup at most every 2 s, so files that appear in the project
    show up in the chat's view (no frozen mode).
  - The change list (`manifest.json`) is saved at most every 2 s while
    things change, and by `flush()` (end of each tool call, from
    milestone 5).
  - What counts as a by-product (not landed, not counted toward the
    512 MiB): what the project's `.gitignore` files ignore, plus `.git`;
    a short fallback list when the project has none.
- **Milestone 5 (chats in overlays).**
  - A run in an isolated chat opens the chat's overlay and mounts it at
    `$XDG_RUNTIME_DIR/rovik-ide/chats/<chat id>` (Linux; a temp folder
    elsewhere); the run's root is that folder. Plan mode isn't isolated
    (it changes nothing) and plans are saved in the project.
  - Turned on by `ROVIK_ISOLATE_CHATS=1` (or `isolate` in the start
    request) until milestone 8 switches it on for every chat.
  - **No copied-tree fallback yet.** Without the driver, the chat works in
    the project directly with the phase-0 protections, and a notice says
    so. Linux has FUSE nearly everywhere; macOS and Windows need macFUSE /
    WinFsp, which the app can already install (`vfs_install_driver`).
  - Dependencies: no separate private copies or `pip`→`uv` rule. In an
    overlay an install writes into the chat's own folder (only what it
    writes), so the copy the decision wanted to avoid only happens for a
    chat that installs, and only for what it installs; package managers
    that hard-link can't link across the mount and copy instead.
  - `CARGO_TARGET_DIR` points at the overlay's own folder, so Rust builds
    don't go through the mount and don't share the project's `target/`.
  - Escapes: a command naming the project's own folder by its full path
    is refused with a hint to use relative paths (on top of git commands
    that change the repository). Watching the project for unexplained
    writes is left for milestone 9.
  - UI: a bar above the composer lists the chat's changed files; each
    opens as a read-only comparison with the project's version. Discard
    throws them away. "Open in editor" from a tool block still opens the
    project's file.
- **Milestone 6 (landing), in `overlay/land.rs` and `agent_run/landing.rs`.**
  - When an isolated chat's run ends with its checks passed (or none to
    run), its changes land on their own if every file merges cleanly.
    If some file also changed in the project, the merged result is
    written into the chat's view and the project's checks run there first
    (queued as landing work, ahead of other chats).
  - Otherwise nothing lands (all or nothing) and the chat's bar says why:
    conflicts (marked per file), checks failing on the merged result, or
    a run that didn't finish. The user lands anyway (the chat's version
    wins where both changed the same lines) or discards.
  - **Not done yet:** resolving conflicts in `ConflictResolver.tsx`, or in
    a turn of the chat, and handing failing merged checks back to the
    chat as a fix turn. Today those wait for the user's Land anyway /
    Discard, with each file comparable to the project's version.
  - The journal (`<app data>/landings/<chat>/<id>/`) keeps old and new
    content of every touched path; an interrupted landing is finished at
    start; Undo restores each path unless it changed since (those are
    named and left alone). Deleted folders are kept whole in the journal.
  - No OS lock file between app instances: the app is single-instance.
    Landings into one project take turns in-process, and writes take each
    file's lock from phase 0, checking nothing changed since the merge
    (replanning up to three times).
  - After a landing the chat's overlay is discarded; its next run starts
    from the project as it is.
- **Milestone 7 (conflicts found early).**
  - The project's file watcher (while the project is open in the editor)
    and every landing tell open isolated chats on that project what
    changed. Untouched files show the new content at once (the kernel's
    cache of them is dropped); folders are listed again.
  - A chat's model gets one note with its next tool result: files it had
    looked at that changed, files it changed that now conflict, or that
    many files changed (more than 200 at once, or too many for the
    watcher to list).
  - A conflict is found when it happens: the chat's bar shows it and a
    notice names the file, instead of waiting for the run to end.
  - **Not done:** pausing runs on a large change (a note and fresh content
    instead), and "landed by chat B" in notes (the note says another chat
    or the user).
- **Milestone 8 (on for every chat).**
  - Every Agent-mode run is isolated. `ROVIK_ISOLATE_CHATS=0` turns it off
    for troubleshooting; there is no other setting.
  - Without the driver, the "not isolated" notice comes once per chat per
    session and points at Settings → Virtual files.
  - Rust builds of a project's isolated chats share one build folder in
    the app's data (not the project's `target/`), so only the first chat
    builds from cold; cargo locks it and the queue runs one build at a
    time.
  - Existing chats get an overlay on their next run; what they wrote
    before stays in the project.
  - Landing outcomes go to the agent log (`event: landing`).
  - Known rough edge: reviewing an edit that needs approval opens the file
    from the chat's view (its mount path); a tab of it stops working once
    the chat's changes land.
- **Milestone 9 (hardening), partly.**
  - Done: the drivers take any backend (brought forward as milestone 3);
    deleting a chat removes its overlay and landing journals, and the
    delete dialog says how many unlanded changed files go with it.
  - Measured (debug build, this repository): through a chat's mount,
    `grep -r` over `src-tauri/src` takes 94 ms against 13 ms directly,
    `find` 30 ms against 7, `git status` 166 ms against 10; `cat` is the
    same. Each read opens the file anew; an open-file cache and FUSE
    passthrough (Linux 6.9+) are the next steps if it matters.
  - **Not done:** running commands in a Linux mount namespace with the
    view mounted over the project's path (the textual check refuses
    commands naming the project's path instead); watching the project for
    writes no landing or save explains; seeding build folders with
    copy-on-write clones; language servers per chat; a copied-tree
    fallback for machines without the driver.
- **Follow-ups (after milestone 9).**
  - Conflicts are resolved in the Git panel's conflict resolver, opened
    from the chat's bar (Resolve…, or a conflicted file): the project's
    version on the left, the chat's on the right. A resolved file becomes
    the chat's version, merged against the project's file as it is now,
    so it lands as it is (`OverlayFs::rebase_file`). The resolver takes
    its files and words from a `source`; git's conflicts work as before.
  - Ask the chat: each conflicted text file gets git-style markers
    (labelled `project (now)` / `this chat`) in the chat's version, and
    the chat is sent a request to combine both changes. Nothing lands,
    not even "anyway", while those markers are left.
  - When a run is about to end and the project changed files it changed
    too, the merged result goes into the chat's view and the checks run
    again (a second verification record). Failures go back to the chat
    with a line naming the merged files, like its own failures; it gets
    the same three rounds in all. A landing that finds new merges after
    that still checks them, and waits for the user if they fail.
  - Pausing on a large change: more than 200 files at once, a change to
    `.git/HEAD` or `.git/ORIG_HEAD` seen by the watcher, or one of the Git
    panel's checkout, pull, stash, discard-all, continue or abort marks
    the project busy. Isolated runs wait before their next tool call and
    before landing until it has been still for two seconds (ten minutes
    at most), with a notice and an `event: project-busy` log line.
  - Commands of an isolated chat (its bash calls, its checks, the checks
    of a landing) run, on Linux, in a mount namespace of their own in which
    the project's path is the chat's view (`overlay/confine.rs`): a
    command naming the project by its full path reaches the chat's files.
    Bubblewrap makes it where it's installed and may
    (`bwrap --dev-bind / / --bind <view> <project>`; Ubuntu 24.04's
    AppArmor, which forbids unprivileged user namespaces to other
    programs, has a profile that lets bwrap make them), and the app's own
    way otherwise: a user namespace mapping the user to itself, which
    needs no privileges. The way is found once, in that order; where
    neither works (or `ROVIK_CONFINE_COMMANDS=0`; `bwrap` or `unshare`
    picks one), and on macOS and Windows, such commands are refused by the
    textual check as before. The .deb and .rpm packages recommend
    `bubblewrap`. The user's own terminal isn't confined.
  - Settings → Builds & checks sets how many heavy commands run at once:
    Auto (one, or two on a big machine with room) or a fixed number from
    1 to 8, kept in the app's data folder (`workload-settings.json`) and
    applied at start. The page shows what auto gives on this machine and
    how many heavy commands run and wait right now.
  - The conflict resolver names its sides after where they come from
    (`ConflictSource.sideNames`): Project and Chat for a chat's conflicts.
