# Chat isolation: fix plan

Follows the review of the isolation work
([PR #54](https://github.com/piliugin-anton/rovik-ide/pull/54),
`chat-isolation-plan.md`): five reviewers by area plus an outside review,
with the findings checked against the code. Claims that turned out wrong are
listed at the end and are not planned.

The goal stays the same: **no chat's work, and none of the user's, is ever
lost or silently overwritten by another chat**, and clashes are found as
early as possible. Today the normal flow already keeps that promise (each
chat works in its own view; a clash stops the later landing), but some
paths break it, and clashes between two running chats are found only when
the first one lands.

## Phases at a glance

| Phase | What                                                    | Why first                          | Size    |
|-------|---------------------------------------------------------|------------------------------------|---------|
| 1     | Close the paths where work can be lost or overwritten   | The promise must hold              | 2 days  |
| 2     | Wrong behaviour (settings, checks, queue, UI)           | Visible bugs in everyday use       | 2 days  |
| 3     | Early warning between running chats                     | The clash is known while it's small| 1 day   |
| 4     | Conflicts go to the chat that's still running           | No waiting for the user            | 0.5 day |
| 5     | Bottlenecks                                             | Speed under load                   | 1.5 days|
| 6     | Tests that run every time (CI)                          | Keep it fixed                      | 0.5 day |
| 7     | (Optional) land after each finished step                | Smaller clashes                    | 1 day   |

Each phase is one commit (or a few), pushed when its tests pass. Each task
lists the problem, the fix, and the test that proves it.

---

## Phase 1: nothing is lost

### 1.1 Deletes and moves are checked like edits
- **Problem:** a chat's delete always lands (`land.rs` `plan`, the `Delete`
  arm). If the user or another chat changed the file meanwhile, that change
  is gone; only Undo brings it back. Same for a deleted or moved folder and
  everything someone added to it.
- **Fix:**
  - When the overlay first removes a project entry (`detach` of a node that
    comes from the project), keep what it was: a file's content hash (and a
    base copy, so a conflict can be shown), a folder's list of
    `(path, hash)` for the files in it. Store it in `OverlayChange::Delete`
    as `base: Option<DeletedBase>` (saved in the manifest).
  - Large folders (over 2,000 files or 50 MB) keep only `(path, size,
    mtime)`; a mismatch there is reported as a conflict to confirm rather
    than merged.
  - `plan`: a delete whose target no longer matches its base is a
    `Conflict { reason: "the chat deleted it, and it changed in the project" }`.
    A target already absent is skipped (no empty "Landed").
- **Tests:** chat deletes `a.rs`, project edits it → conflict, nothing
  lands. Chat deletes `x/`, project adds `x/new.rs` → conflict. Chat deletes
  an untouched file → lands. Project already deleted it → nothing to land.

### 1.2 Moved-and-edited files, and delete-then-recreate, keep their base
- **Problem:**
  - A file the chat moves and edits always conflicts: the plan reads "the
    project's file now" at the new path, finds nothing, and says "deleted
    in the project".
  - `rm gen.rs && codegen > gen.rs`, or `mv a b` over an existing `b`,
    produces a brand-new file with no base, so it conflicts as "both
    created it".
- **Fix:**
  - In `plan`, when `origin != path`, merge against the project's file at
    `origin`.
  - In the overlay's `create` and `rename`: when the target name was a
    project file the chat removed (it's in `removed`) and the project still
    has it, the new node keeps a base copy of that file and an origin of
    that path.
- **Tests:** `mv a.rs b.rs` plus an edit → lands with the edit at `b.rs`
  and `a.rs` deleted. Generator-style recreate → lands cleanly, or merges
  if the project changed other lines.

### 1.3 A failed landing is undone, and crash recovery checks before writing
- **Problem:**
  - A write error partway through `apply` leaves the project half-landed,
    with the journal saying "applying".
  - At the next start `recover()` rewrites every entry over whatever is
    there now, then marks it "done" even if some writes failed.
- **Fix:**
  - `apply` on error: write back `old` for the entries already written
    (reverse order), set the journal to `"failed"`, and report which files
    couldn't be restored.
  - The journal records the app's start id. `recover()` finishes only
    journals from an earlier start, and per entry writes `new` only if the
    path is still `old` or already `new`. Any other path is left alone and
    reported in a notice ("couldn't finish landing these: …").
  - `fsync` the `old/` and `new/` copies and their folder before the
    journal is saved as "applying".
  - Landing ids get a counter (`secs-pid-n`), so two landings in one second
    don't share a folder.
- **Tests:** inject a write failure on the 2nd of 3 files → project as
  before, journal "failed". A journal "applying" with one path changed
  since → that path untouched, others finished, notice lists it.

### 1.4 Undo leaves what it didn't land
- **Problem:**
  - Undoing a folder the landing created removes it with everything in it,
    including files added later and files Undo reported as "kept".
  - Folder copies drop symlinks and permissions.
  - Other chats aren't told about the undo.
- **Fix:**
  - A created folder is removed only if empty after the other entries are
    undone; otherwise it's reported as kept.
  - `copy_tree` keeps symlinks and permission bits.
  - `undo` calls `project_changed` for other open chats, as `land_now` does.
- **Tests:** land a new folder, add a file to it, undo → folder and the
  new file stay, the landed files go. Undo a deleted folder holding a
  symlink → symlink restored.

### 1.5 Land anyway names what it overrides
- **Problem:** "Land anyway" takes the chat's whole file for every
  conflicted file in one click, and the confirm doesn't say which.
- **Fix:** the confirm lists the conflicted files and the deletes it will
  apply. Per-file choices stay in the conflict resolver (Resolve…).
- **Test:** the frontend shows the list; the backend is unchanged.

### 1.6 The overlay's list of changes can't point at deleted files
- **Problem:** a replaced or deleted change file (`upper/N`, `base/N`) is
  removed from disk at once, but the manifest is saved at most every 2
  seconds. A crash in between leaves the manifest pointing at a file that
  no longer exists, and that change is lost.
- **Fix:**
  - Files to delete go on a list and are removed right after the next
    successful `save()`.
  - On `open`, files in `upper/` and `base/` that the manifest doesn't
    mention are deleted.
  - `save()` runs under its own mutex with a generation number, so an
    older list can't be written after a newer one.
- **Tests:** `sed -i` on a changed file, then reopen the overlay from disk
  without a flush → the chat's latest version is there. Two threads saving
  at once → the newest list wins.

### 1.7 Merging the project in keeps the chat's own version
- **Problem:** `take_merged` (before landing, and before the post-merge
  checks) overwrites the chat's file with the merged result and drops the
  old base. If the merge is wrong, the chat's own version is gone.
- **Fix:**
  - Before rebasing, copy the chat's version to
    `store/pre-merge/<landing-round>/<path>`.
  - The note to the model adds a short diff of what the project added to
    each file, so it knows what changed under it.
  - The bar offers "Undo the merge" for those files until the run ends.
- **Tests:** after a merged-checks failure, the pre-merge copy exists and
  "Undo the merge" restores it with the old base.

### 1.8 Hardlinks can't crash a chat's mount
- **Problem:** a hardlinked untouched file whose project original is
  deleted gets dropped from the inode table while it still has a name.
  The next lookup indexes a missing node and panics; the mount dies.
- **Fix:**
  - A node gone from the project loses only that one name; it's removed
    only when it has no names left.
  - Every `inner.nodes[&x]` in fs.rs becomes `get()` with `NotFound`.
- **Test:** `ln a.rs keep.rs`, delete `a.rs` in the project, list the
  folder, read `keep.rs` → still readable, no panic.

### 1.9 Settling a conflicted file is atomic
- **Problem:** `rebase_file` truncates, writes and swaps the base in three
  steps, so the chat's processes can see an empty file in between.
- **Fix:** write a new upper file and a new base file first, then swap
  both under one lock.
- **Test:** concurrent reader during `rebase_file` never sees an empty file.

### 1.10 Commands from the frontend can't reach outside their folders
- **Problem:** `overlay_discard`, `overlay_undo`, `forget` and the
  conflict commands build paths from `dialog_id` unchecked (`../..` would
  delete outside the overlays folder). `overlay_file` reads any absolute
  path.
- **Fix:**
  - `dialog_id` must be one plain path component.
  - Paths must be relative, without `..`, and must be one of the chat's
    changes (`overlay_file`, the conflict commands).
- **Tests:** `overlay_discard("../x")` → error. `overlay_file(id,
  "/etc/passwd")` → error.

### 1.11 Without the driver, chats can't overwrite each other's files
- **Problem:** when the view can't be mounted, a chat works in the project
  directly. File tools are guarded, but its commands (`sed`, generators)
  aren't, so two chats can overwrite each other.
- **Fix:**
  - Not isolated: only one such chat at a time runs commands that write.
    A second one gets a clear refusal for commands not on the read-only
    list (ls, cat, grep, git status/diff/log, tests marked read-only),
    naming the other chat.
  - The notice says this, and how to install the driver.
- **Test:** two non-isolated runs; the second's `sed -i` is refused with
  the other chat's name; its `grep` runs.

---

## Phase 2: wrong behaviour

### 2.1 Checks and runs
- **Project settings looked up under the wrong folder.** `project::load`
  gets the chat's mount path, so the project's saved check settings
  (disabled checks, custom commands) are ignored in isolated chats.
  - Fix: load settings by `project_root`; detection may use the view.
  - Test: a disabled check doesn't run in an isolated chat.
- **Approved checks skip the merge step.** After the user starts waiting
  checks (`Entry::Verify`), the merge-and-fix step is skipped, and
  `check_merged` runs commands without approval.
  - Fix: send that outcome through `take_project_changes`; `check_merged`
    asks for approval when approval is on, as the normal checks do.
- **Outcomes with the wrong landing reason.** `verify-unverified` and
  `verify-stopped` aren't in `LANDS_AFTER` and fall to "the run didn't
  finish".
  - Fix: list each outcome explicitly with its own reason.
  - Changes that land without any checks get a notice: "Landed without
    checks: none are set up".
- **Checked result not what lands.** A landing whose re-plan finds new
  merges after the checks ran lands them unchecked.
  - Fix: `land_now` takes the hashes of what was checked and refuses
    (waits for the user) when the merged files differ.
- **Deleting a chat mid-run** removes its view under the live run.
  - Fix: stop the run first; do the unmount and removal in
    `spawn_blocking`.
- **Plan mode doesn't see the chat's unlanded changes.**
  - Fix: if the chat has an overlay, plan mode reads through it (no
    writes are allowed in plan mode anyway).
- **Wrong notice text.** "This chat isn't isolated" always says to install
  the driver, also when the cause is "changes belong to another project".
  - Fix: word it per cause.

### 2.2 Paths
- **Different spellings of one project folder.**
  - Problem: symlinked paths, `/private` on macOS and letter case on
    Windows make the watcher's events miss a chat, and reopening a chat
    can fail with "belongs to another project".
  - Fix: canonicalize the project once in `chats::open` and in the
    watcher, and compare `project.txt` canonically.
- **Per-file lock key can differ for new folders.**
  - Problem: the lock key uses the raw path when the folder doesn't exist
    yet, and the canonical one afterwards.
  - Fix: canonicalize the nearest existing parent and append the rest;
    resolve symlinked files.
- **Anonymous ledger entries.**
  - Problem: `tool_edit_file` from the UI records its writes as an unnamed
    chat, so a later chat write is refused "by another chat".
  - Fix: record it as the editor.
- **Ledger grows forever.**
  - Fix: drop a chat's view-path entries when its overlay closes.

### 2.3 Commands in chats
- **The git refusal is easy to get around.**
  - Problem: the subcommand is taken as the first word that isn't an
    option, so `git -C /p commit` and `git -c k=v commit` pass;
    `branch -f`, `branch -D`, `update-ref`, `config` (writes), `submodule`
    and `notes` aren't listed.
  - Fix: skip git's global options with values (`-C`, `-c`, `--git-dir`,
    `--work-tree`, `--namespace`). Add the missing subcommands, and
    `branch` with `-d/-D/-f/-m/-M`. Refuse `--git-dir` and `--work-tree`
    outright.
  - Tests: each bypass is refused; `git branch`, `git -C sub log` run.
- **A chat's own command writes trip its file tools.**
  - Problem: after a chat's `sed` changes a file, its next tool write
    based on an earlier read is refused as "changed since you read it".
  - Fix: the overlay records which files a command changed (writes
    between the command's start and end). After the command, the run's
    record of what it has seen is updated for those files with a note
    ("`sed` changed a.rs"), so the model knows to read again; tool writes
    then compare against that.
- **Build folders.** Only `CARGO_TARGET_DIR` is redirected.
  - Fix: also point the usual per-tool caches at shared stores in the app
    data: `GOCACHE`, `PIP_CACHE_DIR`, `npm_config_cache`, `GRADLE_USER_HOME`
    (opt-in). Show by-product size per chat in the bar; warn at 5 GB.
- **Merging text that isn't UTF-8.** Text in UTF-16, Latin-1 and similar
  always conflicts, because anything that isn't UTF-8 counts as binary.
  - Fix: treat a file as binary only when it has NUL bytes in the first
    8 KB (UTF-16 has them, so detect a UTF-16 BOM and merge decoded text).
    Merge other bytes with `merge3_raw` directly.

### 2.4 The queue for heavy work
- **The terminal holds the slot forever.** The presence of a compiler
  process (Vite's `esbuild --service`, `tsc -w`, watch-mode test runners)
  makes it count as heavy whatever its CPU use, so the terminal holds the
  only slot for as long as it runs.
  - Fix: a compiler process counts only while the tree also uses more than
    `IDLE_CORES`. It never resets the idle timer.
- **Paused processes stay paused.**
  - Fix: on app exit, send `SIGCONT` to every paused job.
  - A pause lasts at most 3 minutes, then the job resumes; the chat shows
    "Paused: memory is low" while it lasts.
  - On Windows (where pausing does nothing) `paused_since` isn't set, so
    the timeout isn't skipped.
- **History keyed by each chat's mount**, so it never helps and the file
  grows with every chat.
  - Fix: key it by the project, cap it at 50 projects (least recently
    used), and write at most every 10 s.
- **Stopped and timed-out runs count as normal ones.**
  - Fix: only commands that exited on their own are recorded.
- **Notes sent under the queue's lock.**
  - Fix: build the note under the lock, send it after.
- **The terminal's foreground is the oldest child.**
  - Fix: use the shell's newest child.
- **`bun test` / `pnpm test` without a script count as light.**
  - Fix: treat them as heavy.
- **Settings page.**
  - Fix: save the file before applying the setting.
  - Compute "auto" from a fresh reading when the machine hasn't been
    sampled yet.

### 2.5 Frontend
- **The bar outlives a chat switch.**
  - Problem: the IsolationBar is reused across chats, so "Ask the chat"
    can send its request to the chat switched to.
  - Fix: `keyed` Show. `askChat` captures the chat id before awaiting and
    passes it to `onAskChat(id, request)`.
- **"Ask the chat" can leave a chat stuck.** If no model is set or a run
  starts meanwhile, it writes markers and sends nothing.
  - Fix: check the model and run state before writing markers.
    `onAskChat` returns whether a run started; if not, the markers are
    taken back (backend: `overlay_unmark`).
  - Marked files stay listed in `pending` as "with the chat", so Resolve…
    and Land anyway stay available.
- **The resolver brings back old choices.**
  - Problem: choices are saved under a key that's the same for every round
    of conflicts in a chat.
  - Fix: the key includes the backend's pending id (new per round).
    Choices are cleared on land, discard and per-file resolve.
- **Typing during a save.**
  - Problem: keys typed while a save is in flight are marked clean, and
    the next save is wrongly refused as "changed on disk".
  - Fix: after the save, `originalContent = file.content` and
    `dirty = f.content !== file.content`.
- **Actions while a run starts.**
  - Problem: Discard and Land anyway check the run state only before
    their confirm dialog.
  - Fix: check `running` again after the confirm. The backend refuses
    discard and land while the chat's run is going. The resolver gets
    `running`, and its saves and Land are disabled during a run.
- **Resolver feedback.**
  - Problem: Land from the resolver closes it silently when new conflicts
    appeared, and an error goes unhandled.
  - Fix: show "New conflicts" and keep it open; catch errors.
- **Compare after a refused save is a dead end.**
  - Fix: add "Use the disk version" to the compare tab.
- **Status checks on the UI thread.**
  - Fix: make `overlay_status` and `overlay_file` async.
- **Accessibility.**
  - Fix: `aria-expanded` on the toggle, a hidden caret, and per-file
    button titles ("Resolve" / "Compare").

---

## Phase 3: early warning between running chats

Today a clash between two running chats is found when the first lands.
This finds it when the second chat's change is written.

- **An index of who changes what**, per project, in `overlay/claims.rs`:
  file → chats with a change to it.
  - An entry is added when a chat's overlay first changes a file (copy-up
    or create) and removed when it lands or is discarded.
  - Kept in memory, rebuilt from the open overlays at start.
- **The check, after each tool call** (where the run already flushes its
  overlay), for each file the chat changed in that call that another open
  chat has also changed:
  - Same base (both copied it from the same project version, which is
    the usual case): `merge3(base, ours, theirs)` says exactly whether the
    two would conflict.
  - Different bases: merge each with the project's current file first,
    then merge the results.
  - Clean: nothing to say (they'll merge).
  - Conflict: both chats get a note in their next tool result: "Chat
    *<title>* is also changing lines 40–60 of `src/a.rs`, in a way that
    clashes with yours. Avoid those lines if you can, or say so in your
    summary." The bars show "Clashes with chat *<title>*: `src/a.rs`" with
    a Compare button (three-way: base, this chat, the other chat).
  - Said once per file pair until either side changes it again.
- **Cost:** only files changed by both chats are merged, once per tool
  call. Line ranges come from the merge's conflict segments.
- **Not covered:** clashes in meaning across files (a renamed function
  called elsewhere). The post-merge checks (already done) catch those.
- **Tests:**
  - Two chats change the same line → both get the note with the right
    line numbers.
  - Different lines in one file → no note.
  - Chat A discards → B's warning clears.
  - Three chats → each pair is checked.

---

## Phase 4: conflicts go to the chat that's still running

- When a conflict is found for a chat that's running (early, at a landing
  of another chat, or from phase 3 once the other lands):
  - Its files get conflict markers (`mark_conflicts`).
  - Its next tool result says which files, and how to resolve them.
  - Its run doesn't end until the markers are gone; the existing check at
    landing refuses otherwise.
- A chat that isn't running keeps today's behaviour: the bar, Resolve…,
  Ask the chat.
- Binary files and delete-versus-edit can't get markers and stay the
  user's to decide; the chat's summary says so.
- A setting (Settings → Builds & checks, renamed "Chats & builds"): "When
  a running chat's changes conflict: let it resolve them (default) / ask
  me".
- **Tests:**
  - A lands while B is running with a clashing change → B's next tool
    result names the file; B fixes the markers; B lands at the end.
  - The setting set to "ask me" → today's behaviour.

---

## Phase 5: bottlenecks

### 5.1 The chat's view (overlay/fs.rs)
- **First-write copies hold the lock.** The first write to a large file
  copies it twice (chat version and base) under the one lock, blocking
  every file access in that chat's view.
  - Fix: mark the node "copying" and do the copies outside the lock;
    other operations on that node wait, the rest go on.
- **Files reopened on every read and write.**
  - Fix: keep open handles for upper files with open FUSE handles.
- **Folder listings re-read every time.** Every listing re-reads the
  project folder (`readdir_plus` and `dir_is_empty` pass `refresh`).
  - Fix: honour `RELIST_AFTER` (the watcher already drops stale listings).
- **Moved files keep stale content.** Cache drops for files the chat
  moved miss them.
  - Fix: a map from project path to node, and drop the cache by that.
- **Space accounting.**
  - Check the limit before copying, and undo the charge when a write
    fails.
  - Charge a move out of a build folder against the limit.
  - Base copies count toward the limit at half weight; say so in the
    settings text.
- **Ignore files.**
  - Fix: one matcher per folder, rooted there, checked deepest first.
  - Read `.ignore` files too.
  - Look for ignore files at any depth, not just the root.

### 5.2 The queue's sampler
- **Process-group lookups.** One process-group lookup per process per
  job on every tick.
  - Fix: one per process, shared by all jobs.
- **Constant ticking.** It wakes every 500 ms all session while a
  terminal is open.
  - Fix: tick every 2–5 s when nothing heavy is running or waiting.
  - Build command lines only for processes in tracked trees.

### 5.3 Blocking work on async threads
- **Where:**
  - Landing's plan and merge, `take_merged`, `merge_into_current`.
  - File-guarded writes that wait for a landing's locks.
  - `forget`.
- **Fix:** run them all in `spawn_blocking`.

### 5.4 Branch switches
- **Problem:** a branch switch or another large change checks nothing for
  conflicts, so they show up only at landing.
- **Fix:** after the project settles, check every file the chat changed
  (it has the base copies) against the project, as for single-file
  changes.
- **Overlays not open since a restart:**
  - They are checked when they open.
  - Their saved state gets a "project changed while closed" mark, so the
    bar can say so.

---

## Phase 6: tests that run every time

- **A CI workflow** in `.github/workflows/ci.yml`, on Ubuntu:
  - Install `fuse3`, then run `cargo test --lib`, `cargo test --lib --
    --ignored` (FUSE tests) and `npm test`, plus a type check.
  - Confinement tests skip themselves if user namespaces are forbidden.
- **Per-test data folders:** a test-only override of the overlays' data
  folder, so the FUSE tests can run in parallel.
- **The two Plan-mode tests that already fail:** fix them, or mark them
  with the reason, so CI is green.
- **New tests from phases 1–4** are not ignored wherever they don't need
  a mount (most of phase 1 works on `OverlayFs` directly).

---

## Phase 7 (optional): land after each finished step

- When a run's checklist step is marked done and its checks pass, the
  chat's changes so far land. The run goes on from the project as it is
  then.
- **Pros:** other chats see changes sooner, so clashes stay small.
- **Cons:** more check runs, and a half-finished feature reaches the
  project.
- **Off by default** (Settings → Chats & builds).

---

## Not planned (review claims that were wrong)

- **"Text edits interleave instead of conflicting."**
  `update_for_chat` re-reads the file under the lock and fails when
  `old_text` isn't there any more. A clash is refused, not interleaved.
- **"Two app instances can land at once."** The app runs as a single
  instance (`tauri-plugin-single-instance`).
- **"Lossy UTF-8 conversion can hide conflicts."** Files that aren't
  UTF-8 are rejected before the conversion.
- **"Builds and installs keep the project busy."** Busy is set only for
  more than 200 changed files at once or a change to git's current
  commit (`.git/HEAD`, `ORIG_HEAD`). The watcher ignores `target/` and
  `node_modules/`.
- **"A move reads the chat's content from the project's old path."** That
  is the content of an untouched moved file. The real bug (moved *and*
  edited) is task 1.2.

## Order

1 → 2 → 3 → 4 → 5 → 6, with 6's workflow added early (after phase 1) so
each later phase is checked by it. Phase 7 only if wanted after using 3
and 4 for a while.
