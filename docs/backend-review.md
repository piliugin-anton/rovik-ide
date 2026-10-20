# Backend review, October 2026

A read-through of the Rust workspace in `src-tauri` for bugs, and a
refactoring of the app crate (`src-tauri/src`), the Tauri side of Rovik.
The app crate was read whole; the other crates (`rovik-core`,
`rovik-client`, `rovik-agentd`, `rovik-protocol`) were reviewed by area,
each finding traced to its code before it was kept. What was fixed is
listed first, then what's left, with where it is and what it would take.

## Fixed in this change

### The app crate (`src-tauri/src`)

| Where                                                                     | Was                                                                                                                                                                                                                                                                                 | Now                                                                                                                                            |
| ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `filesystem/entries.rs` `copy_path`                                       | A folder copied into itself (or below itself) copied its own copy, without end, until the path got too long or the disk was full; `new_name` wasn't checked, so `../x` left the folder; the destination wasn't checked to be a folder.                                              | Refused with `invalid-name: can't copy … into itself`; the name is checked as for create, rename and move; a half copy is removed on an error. |
| `filesystem/entries.rs`, `filesystem/mod.rs`, `terminal.rs`, `lsp/mod.rs` | Deleting, renaming, moving and copying entries, reading a file, starting the watcher (a tree walked folder by folder on Linux), writing to a terminal (a full pty blocks) and sending to a language server ran on the app's main thread: the window froze for as long as they took. | Each is an async command whose work runs on a worker thread (`util::blocking`).                                                                |
| `formatter.rs` `run`                                                      | A formatter that exited with an accepted code, printed nothing and said nothing on stderr "formatted" the file to nothing, and the editor took it.                                                                                                                                  | Nothing on stdout is an error (the input is never blank there).                                                                                |
| `git/changes.rs` `revert_in`                                              | The file's path was a pattern to libgit2: reverting `pages/[id].tsx` also checked out `pages/i.tsx` and `pages/d.tsx` by force, with no copy in the trash.                                                                                                                          | The checkout takes the path literally, and the reset's pathspec is escaped.                                                                    |
| `git/changes.rs` `backup_name`                                            | A copy restored from the trash under "name (before revert).ext" was overwritten by the next revert's backup.                                                                                                                                                                        | Numbered when the name is taken.                                                                                                               |
| `git/changes.rs` `git_commit_blocking`                                    | Committing during a merge wrote a single-parent commit and committed conflict markers as text (the panel disables the button; the backend didn't).                                                                                                                                  | Refused while a merge or rebase is in progress, or with conflicts in the index.                                                                |
| `git/changes.rs` `git_discard_all_blocking`                               | During a rebase, the hard reset dropped the rebase's state, leaving HEAD detached in the middle of it.                                                                                                                                                                              | Refused while an operation is in progress.                                                                                                     |
| `git_sync.rs`                                                             | Its own, weaker copy of the "inside the repository" path check: on Windows `\temp\x` and `C:x` aren't absolute, so a conflict's resolution could be written outside the repository.                                                                                                 | One check, `git::inside_repo`, everywhere a path comes from the frontend.                                                                      |
| `lsp/mod.rs`                                                              | A frame over 64 MB, or a body shorter than its header, ended the reader thread silently: the server kept running, the editor got no `Exit` and no more messages.                                                                                                                    | The server is stopped, and its exit reported.                                                                                                  |
| `lsp/rust_analyzer.rs` `version_of`                                       | A probe that hung was killed but never waited for: a zombie per hung probe.                                                                                                                                                                                                         | Reaped.                                                                                                                                        |

### `rovik-core`

| Where                                               | Was                                                                                                                                                                                                                                                                                                                                        | Now                                                                                                                                                                                                                |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `chat_request.rs` `parse_duration_text`             | A `Retry-After` of `inf`, `1e300` or a number past what a `Duration` holds panicked the request's task (`Duration::from_secs_f64`): in a run, the run task died without `RunEnded` and the chat stayed "running".                                                                                                                          | `try_from_secs_f64`: no wait for what isn't one.                                                                                                                                                                   |
| `chat_request.rs` `feed_complete`                   | A non-streaming reply whose `tool_calls` held a number or a string panicked on `c["index"] = …`.                                                                                                                                                                                                                                           | Only objects are numbered; the rest is left out.                                                                                                                                                                   |
| `project_profile/workspaces.rs` `toml_string_value` | `v[1..v.len() - 1]` panicked on an unterminated value ending in a multi-byte character; `detect()` runs at every run start.                                                                                                                                                                                                                | A slice that isn't on a character boundary is no value.                                                                                                                                                            |
| `agent_tools.rs` `is_denylisted_name`               | A file or folder literally named `key`, `pem`, `pfx` or `p12` was hidden from `list_dir`, `search_code`, the repo map and the Files search (`rsplit('.').next()` is the whole name without a dot).                                                                                                                                         | The extension alone.                                                                                                                                                                                               |
| `overlay/chats.rs`                                  | `&holder[..8]` byte-sliced a chat id.                                                                                                                                                                                                                                                                                                      | Characters.                                                                                                                                                                                                        |
| `overlay/fs.rs` `copy_up`                           | `fs::copy` copied the project file's mode: a read-only project file (generated code, a vendored one) could never be written in the view, even after the chat's `chmod u+w` (the backend opens the copy read and write).                                                                                                                    | The chat's copy is made writable; the mode the chat sees is `perm`'s, as before.                                                                                                                                   |
| `overlay/fs.rs` `rename`                            | `RENAME_EXCHANGE` of `a/b` with `a` made `a` its own child (the "into itself" check covered one direction); `mv a b` where `b` is a hard link of `a` was taken as a case-only rename and lost the name `a`.                                                                                                                                | Both directions checked; a name the folder has exactly is a no-op, as POSIX says.                                                                                                                                  |
| `pinned_browser.rs`                                 | This version's folder without its executable (an install stopped half-way, an antivirus's quarantine) made every start download 200 MB and fail at the rename, for good.                                                                                                                                                                   | Such a folder is removed before the rename.                                                                                                                                                                        |
| `agent_run/execute.rs` `prepare_edit_review`        | A `write` to a path outside the project read that file whole into the edit review: its content went to the AI reviewer's provider and the approval dialog (the write itself was refused later).                                                                                                                                            | The path is resolved inside the project first, as the write does.                                                                                                                                                  |
| `agent_run/permissions.rs` `segment_is_read_only`   | The read-only classifier (which confines a worker's, a test writer's and a breaker's `bash`) let `rg --pre CMD` (runs CMD over every file), `git log --output=FILE`, `git diff --output=FILE`, `tree -o FILE` and awk's `print … \| "sh"` through as read-only.                                                                            | Each is a write, or a program run.                                                                                                                                                                                 |
| `agent_run/worker.rs` `WriteScope::for_step`        | A step whose Files line named `./` got the folder `""`, under which every path is: the worker could write anything.                                                                                                                                                                                                                        | The project itself is no file of a step's.                                                                                                                                                                         |
| `agent_run/subagent_run.rs` `run_ide_subagent_with` | A child that couldn't launch left its chat row (with the brief) behind; the other launch path cleaned up.                                                                                                                                                                                                                                  | The same `launched` cleanup.                                                                                                                                                                                       |
| `agent_run/candidates.rs` `make`                    | When creating a candidate's chat failed, the overlay copies opened before it (and the chats made so far) were left behind with no `candidates` row naming them: no round's discard ever removed them.                                                                                                                                      | Discarded and deleted on that path.                                                                                                                                                                                |
| `agent_run/failure.rs`                              | `ms as i64` on a provider-supplied `retry_after_ms`: a huge value went negative, a `resetAt` in the past.                                                                                                                                                                                                                                  | Saturates.                                                                                                                                                                                                         |
| `overlay/fs.rs` `open`, `replay`                    | A saved change whose folder can't be found now (a new file in a project folder a branch doesn't have) was dropped from the list on open, and the sweep that follows deleted the chat's only copy of it; a `manifest.json` that didn't parse (a newer Rovik's) emptied the whole overlay the same way. Paths from the list weren't checked. | A change that can't be placed is parked: kept, saved again, named to the sweep, and placed at the next open. A list that can't be read fails the open. A path with a part that isn't a name is left out.           |
| `overlay/fs.rs` `replay`                            | A hard-linked file was saved once per name, with one upper file; opened again, its names were separate nodes sharing that file, and unlinking either trashed it under the other.                                                                                                                                                           | Names that share an upper file share one node again.                                                                                                                                                               |
| `overlay/land.rs` `plan`                            | A moved and edited file's project side came from where it came from only: a file the user created at its new place meanwhile was written over without a conflict. Paths weren't checked for `..`.                                                                                                                                          | Another file at the new place is a conflict (Land anyway takes the chat's); a file the user moved too, as it was or as the chat has it, is the project's side. A path that isn't inside the project is a conflict. |
| `overlay/import.rs` `build`                         | The paths of an imported result tree weren't checked part by part: `..` reached outside the project.                                                                                                                                                                                                                                       | Such a path counts as outside the project.                                                                                                                                                                         |
| `agent_run/mod.rs` `start_run`                      | The run was registered before `Run::prepare` was awaited, with no guard: a panic or a dropped start left "The agent is already working in this chat" until a restart.                                                                                                                                                                      | A guard from the registration on, moved into the run's task.                                                                                                                                                       |
| `agent_run/subagent_run.rs` `child_for`             | A resumed `task` call's child was found by the call's id alone: a provider that numbers calls per reply ("call_1") could resume an earlier reply's child.                                                                                                                                                                                  | Found among the children started since the reply was stored.                                                                                                                                                       |
| `agent_run/change_review.rs`                        | In Autopilot, a follow-up change review that couldn't report (its model failed) before the run's last review neither blocked nor asked again: the held high findings went unverified and the change landed.                                                                                                                                | The held findings are asked about again, as the plan review's are; the last review's block stands.                                                                                                                 |
| `agent_run/autopilot/driver.rs`, `artifacts.rs`     | The saved plan was looked for after the latest Plan record only: a Retry of a Plan stage that failed after saving it reported "No plan was saved".                                                                                                                                                                                         | Looked for from the task's first Plan record.                                                                                                                                                                      |
| `agent_run/run.rs` `prepare`                        | A Retry or a Continue in a chat whose task ended at an inside stage ran as an ordinary Agent run under Autopilot's permissions (any command, no checkpoint).                                                                                                                                                                               | Refused: there's no stage to go on with.                                                                                                                                                                           |
| `agent_run/run.rs` `on_event`                       | A reasoning model that streamed thoughts, then stalled before any text, was retried with the second attempt's reasoning appended to the first's.                                                                                                                                                                                           | The reasoning starts over on `Retrying`.                                                                                                                                                                           |

### `rovik-client`

| Where                     | Was                                                                                                                                                                                                                                                                                                | Now                                                                            |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| `http.rs` `send`          | The 60 s wait for a response's headers wrapped the whole upload: the daemon reads a body before it answers, so a 100 MB chat move on a slow uplink, a first snapshot push of a big repository, a 20 MB blob, all failed at 60 s as "isn't reachable"; the 10-minute budget above it never applied. | A request with a body (over 64 KB) is answered in the body's time, 10 minutes. |
| `system_ssh.rs` `forward` | The long-lived `ssh -N` forward's stderr was piped but never read after it was up: ssh writes a line for every forwarded connection that fails (the daemon down, the client trying every ≤60 s); once the pipe filled, ssh blocked and forwarded nothing, while `try_wait` said it was alive.      | Drained and dropped.                                                           |
| `client.rs` `session`     | Any session that reached the read loop counted as served and reset the backoff: a daemon in a crash loop was reconnected every second, for ever, each time with `configure`, `catch_up` and `pull_all`.                                                                                            | Served means it stayed up 30 s.                                                |
| `packs.rs` `fetch`        | The results worker, one for every host, fetched a result's pack with no timeout between frames: one stalled host held every host's imports and moves back.                                                                                                                                         | 60 s between frames, then the host counts as unreachable.                      |
| `http.rs` `health`        | No timeout on a daemon that accepted the connection and never answered `/health`: a deploy's `confirm` hung with no way to cancel.                                                                                                                                                                 | 15 s for the whole call.                                                       |

### `rovik-agentd`

| Where                   | Was                                                                                                                                                                                                                                                                                                                                                                                          | Now                                                                                                                                                 |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cli.rs`                | `--root DIR` (a folder standing in for the system, for development) was honoured through sudo, which lets everyone run `user` and the administrators `admin`: `DIR/etc/rovik/agentd.toml` of the caller's (`allow_unsigned = true`) and a link `DIR/opt/rovik/bin → /opt/rovik/bin` would have root install an unsigned binary into the real `/opt/rovik/bin`, past the release's signature. | `--root` is refused for the installed binary, the one the sudoers rule runs; a development build takes it whoever runs it, as the install tests do. |
| `server.rs`             | A request body that took longer than its 10 minutes, or a link that broke, was answered `413 Too Large` (`TooLarge`): the client took a retryable failure for a permanent refusal.                                                                                                                                                                                                           | `408`, as a request to make again; the same code is one helper for both bodies.                                                                     |
| `changes.rs` `discard`  | `WorkDiscard` reset the worktree but kept the result committed before it: `WorkResults` after a reconnect offered the discarded changes again.                                                                                                                                                                                                                                               | The result is committed again (none).                                                                                                               |
| `serve.rs`              | Under `--data DIR` the two folders were one, counted twice: the disk cap tripped at half.                                                                                                                                                                                                                                                                                                    | Each folder once (`Home::folders`).                                                                                                                 |
| `deliver.rs` `git_push` | The remote (from `ProjectEnsure`/`DeliveryTest`) went to `git push` where an option could: `--receive-pack=…` would run a program.                                                                                                                                                                                                                                                           | A remote starting with `-` is refused.                                                                                                              |
| `notify.rs`             | When the configured client couldn't be built, the notice went through a default `reqwest` client, without Settings → Network's proxy.                                                                                                                                                                                                                                                        | Logged and skipped.                                                                                                                                 |
| `moves.rs` `import`     | An import cleared the chat's earlier stay, then could fail (its worktree, its rows' transaction) with `state.json` still naming the stay's base for the new checkout: the project's own changes read as the chat's.                                                                                                                                                                          | A failed import clears what it made and forgets the worktree.                                                                                       |

## Refactoring of the app crate

The app crate does the same with fewer and smaller pieces. What to know
when adding to it:

- `util.rs` has what every module shares. `blocking(work)` runs blocking
  work (disk, git, a child process) on a worker thread and is what a
  command awaits; `blocking_in(root, load)` is the same for work on a
  folder; `rewriting(path, work)` holds the project's isolated chats
  while the working tree is rewritten. `Result::err_text()` turns any
  error into the text a command returns, in place of
  `map_err(|e| e.to_string())` (160 copies went). `Registry<T>` is the
  live things a command addresses by id (terminals, language servers).
- `git/` is one module per concern: `mod.rs` has the status and what the
  others share (`open_repo`, `inside_repo`, `head_commit`, `signature`,
  `tree_blob`, `blob_text`, `stage_all`), `remote.rs` fetches and pushes,
  `branches.rs` lists, switches, creates and deletes, `changes.rs`
  commits, reverts and discards, `stash.rs` shelves and shows,
  `history.rs` is the log and its search (one scan of a patch's lines,
  `change_lines`, for the match and the badge). `git_sync.rs` uses the
  shared helpers too (`merge_heads`, `start_rebase`, `nothing_in_progress`
  replaced their copies).
- `filesystem/` is `mod.rs` (the folders' entries, a file's text and
  images, saving, the settings beside them), `watch.rs` (the watcher, a
  `Watch` with `drain`, `saw` and `report` in place of one 200-line
  command) and `entries.rs` (create, delete, rename, move, copy: one
  `move_entry` for rename and move, one `copy_entry`, one name check).
  `WatcherState` derives `Default`.
- A formatter is a `Spec::new(id, name, languages, program, args)` with
  `.local()`, `.signals()`, `.declared_only()` and `.ok_codes()` for what
  differs from the usual (a standard tool on PATH, fine without a
  config, 0 its only success): the table is a third shorter, and a new
  formatter is one entry.
- Commands that route a chat's request to where the chat is call
  `commands::route(remote, dialog_id, request)`.

## Open findings

Reported by the area reviews and traced to the code, but not changed.
The high and medium findings of the review were fixed in a second
change (the tables above); the low ones are left for changes of their
own. The line numbers are of the revision the review read.

### Low

- `rovik-core/src/net/tunnel.rs:160`, `net/mod.rs:843`: a proxy or host
  written as `[2001:db8::1]` works for HTTP but not for SSH tunnels
  (the brackets aren't stripped); `system_one.rs:408` never matches
  `[::1]` as local.
- `rovik-core/src/net/tunnel.rs:295`: SOCKS5 user and password lengths
  go as `u8` without a check for system-proxy credentials over 255
  bytes.
- `rovik-core/src/agent_tools.rs:529` vs `:456`: editor markers are
  keyed by the normalized relative path but looked up by the path as
  the model wrote it (`./src/a.ts`): "no diagnostics reported".
- `rovik-core/src/agent_tools.rs:325` → `outline/mod.rs:123`:
  `outline_of_file` picks the language with empty content (`.m` as
  MATLAB, `.h` as C), and caches the result.
- `rovik-core/src/command_tool.rs:429`: on a timeout, output a detached
  process still holds the pipe for is thrown away.
- `rovik-core/src/agent_run/permissions.rs:1046` `is_risky_command`
  misses `find -delete`, `find -exec rm`, `git stash drop|clear`,
  `git branch -d`, `shred`, `truncate`, `perl -e`/`ruby -e`.
- `rovik-core/src/agent_run/execute.rs:412`, `repair.rs:414`: the test
  writer's "test files only" rule is by name; a symlinked `tests/fixtures
-> ../src` passes it.
- `rovik-core/src/agent_run/tools.rs:841`: `read` of `@checks/…` is
  served before the hidden-files filter.
- `rovik-core/src/agent_run/log.rs:51`: a panic in `append_agent_log`
  leaves `writing` set and every later line queued in memory.
- `rovik-core/src/agent_run/diff.rs:27`: `myers` clones its frontier per
  step; a diff needing over 2000 edits allocates ~64 MB before falling
  back.
- `rovik-core/src/agent_run/autopilot/stage.rs:421`: a failed Deliver
  record ends the task and a Retry never reopens it; `rate_limit.rs:54`
  ends an over-budget rate-limited run as "error", not "budget";
  `driver.rs:280` `lock_of` never prunes its per-chat map;
  `plan_review.rs:1283` compares a new task's plan against an earlier
  task's for the "small revision" shortcut.
- `rovik-core/src/overlay/land.rs:736` `is_content` treats any directory
  as equal to a journal's directory copy: `recover_where` may remove a
  repopulated folder after a crash mid-landing.
- `rovik-core/src/chat_db/sync.rs:914`: a colliding row is parked at
  `-seq` with no check that `-seq` is free; a second collision aborts the
  pull transaction. `chat_db/mod.rs:299` `settle` unlocks before it takes
  the shared lock. `chat_hold.rs:301` `forget` checks, then deletes.
- `rovik-core/src/vfs/macfuse.rs:234` (macOS): `capturing_stderr`
  redirects the process's fd 2 to a non-blocking pipe for up to 30 s.
- `rovik-client/src/client.rs:691`: a frame that doesn't deserialize is
  skipped silently; a bad `Reply` leaves its call to time out as "didn't
  answer in time". `ssh.rs:919`: a key error is mapped to
  `ConnectionRefused` and retried for ever as "Offline".
  `hosts_page.rs:459`: the forward port is picked once; taken later,
  every reconnect fails until the host is re-added. `hosts_page.rs:609`:
  secrets are written before the row; changing the auth kind doesn't
  delete the unused one. `rovik-protocol/src/release.rs:70`: the
  signature's trusted comment (`file:<name>`) isn't checked, so a validly
  signed older artifact can be served under a newer URL.
- `rovik-agentd/src/moves.rs:177`: `ChatRelease` writes the database
  before the store. `work.rs:498`: start-up `collect` forgets a released
  chat's folder whose clear failed, orphaning its rows.
  `events.rs:101`, `notify.rs:51`: `leads`, `changes` and `told` grow
  without bound.
- App crate, `filesystem/mod.rs` `resolve_id`: the id → path cache is
  trusted; after a rename outside the app and before the tree refreshes,
  a delete by the old id deletes the new file at that path. A check that
  the path still has the id's identity would also break the design that
  keeps an old id resolving after a cross-device move; it needs a
  decision.
- App crate, `git_sync.rs` abort: for a merge, cherry-pick or revert
  started from a terminal with unrelated uncommitted edits, the hard
  reset loses them (git's `--abort` keeps them). The app's own pull
  refuses to start with uncommitted changes, so this is reachable from
  a terminal only.

## Duplication left in the crates

Repeated code the reviews found, for a later pass:

- `rovik-core`: six SHA-256-to-hex encoders (`overlay/fs.rs:162`,
  `overlay/chats.rs:302`, `:849`, `:2251`, `chat_hold.rs:326`,
  `chat_db/attachments.rs:75`); `check_name` in `vfs/store.rs:481` and
  `overlay/fs.rs:363`, already diverged; `repo_map/mod.rs:1187`
  `ancestors` is `read_symbol.rs:134`; "the latest real user message" in
  seven variants with drifting filters (`run.rs:3380`, `:3395`,
  `execute.rs:1761` omit `!m.failed`); `command_tool::Queued { kind:
Check }` built three times; six `Skip → String` mappers in the
  planning and review stages; `test_writer.rs` `tests_stage` and
  `tests_for_an_untested_change` copied; `ChatError::new(Cancelled,
"Stopped.")` seven times; four char-based truncation helpers;
  `chat_db/schema.rs:370` ten identical migration blocks.
- `rovik-client`: `session()` and `welcome()` repeat the Hello/Welcome
  handling; "the daemon serves feature X" four times; clearing six
  `State` maps twice; config + policy + network three times.
- `rovik-agentd`: `bad_request`/`internal`/`git`/`joined` helpers copied
  in seven files; the Content-Length pre-check in three handlers;
  `Signature::now("Rovik", …)` four times.
