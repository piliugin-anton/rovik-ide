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

| Where                                               | Was                                                                                                                                                                                                                                                             | Now                                                                               |
| --------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `chat_request.rs` `parse_duration_text`             | A `Retry-After` of `inf`, `1e300` or a number past what a `Duration` holds panicked the request's task (`Duration::from_secs_f64`): in a run, the run task died without `RunEnded` and the chat stayed "running".                                               | `try_from_secs_f64`: no wait for what isn't one.                                  |
| `chat_request.rs` `feed_complete`                   | A non-streaming reply whose `tool_calls` held a number or a string panicked on `c["index"] = …`.                                                                                                                                                                | Only objects are numbered; the rest is left out.                                  |
| `project_profile/workspaces.rs` `toml_string_value` | `v[1..v.len() - 1]` panicked on an unterminated value ending in a multi-byte character; `detect()` runs at every run start.                                                                                                                                     | A slice that isn't on a character boundary is no value.                           |
| `agent_tools.rs` `is_denylisted_name`               | A file or folder literally named `key`, `pem`, `pfx` or `p12` was hidden from `list_dir`, `search_code`, the repo map and the Files search (`rsplit('.').next()` is the whole name without a dot).                                                              | The extension alone.                                                              |
| `overlay/chats.rs`                                  | `&holder[..8]` byte-sliced a chat id.                                                                                                                                                                                                                           | Characters.                                                                       |
| `overlay/fs.rs` `copy_up`                           | `fs::copy` copied the project file's mode: a read-only project file (generated code, a vendored one) could never be written in the view, even after the chat's `chmod u+w` (the backend opens the copy read and write).                                         | The chat's copy is made writable; the mode the chat sees is `perm`'s, as before.  |
| `overlay/fs.rs` `rename`                            | `RENAME_EXCHANGE` of `a/b` with `a` made `a` its own child (the "into itself" check covered one direction); `mv a b` where `b` is a hard link of `a` was taken as a case-only rename and lost the name `a`.                                                     | Both directions checked; a name the folder has exactly is a no-op, as POSIX says. |
| `pinned_browser.rs`                                 | This version's folder without its executable (an install stopped half-way, an antivirus's quarantine) made every start download 200 MB and fail at the rename, for good.                                                                                        | Such a folder is removed before the rename.                                       |
| `agent_run/execute.rs` `prepare_edit_review`        | A `write` to a path outside the project read that file whole into the edit review: its content went to the AI reviewer's provider and the approval dialog (the write itself was refused later).                                                                 | The path is resolved inside the project first, as the write does.                 |
| `agent_run/permissions.rs` `segment_is_read_only`   | The read-only classifier (which confines a worker's, a test writer's and a breaker's `bash`) let `rg --pre CMD` (runs CMD over every file), `git log --output=FILE`, `git diff --output=FILE`, `tree -o FILE` and awk's `print … \| "sh"` through as read-only. | Each is a write, or a program run.                                                |
| `agent_run/worker.rs` `WriteScope::for_step`        | A step whose Files line named `./` got the folder `""`, under which every path is: the worker could write anything.                                                                                                                                             | The project itself is no file of a step's.                                        |
| `agent_run/subagent_run.rs` `run_ide_subagent_with` | A child that couldn't launch left its chat row (with the brief) behind; the other launch path cleaned up.                                                                                                                                                       | The same `launched` cleanup.                                                      |
| `agent_run/candidates.rs` `make`                    | When creating a candidate's chat failed, the overlay copies opened before it (and the chats made so far) were left behind with no `candidates` row naming them: no round's discard ever removed them.                                                           | Discarded and deleted on that path.                                               |
| `agent_run/failure.rs`                              | `ms as i64` on a provider-supplied `retry_after_ms`: a huge value went negative, a `resetAt` in the past.                                                                                                                                                       | Saturates.                                                                        |

### `rovik-client`

| Where                     | Was                                                                                                                                                                                                                                                                                                | Now                                                                            |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| `http.rs` `send`          | The 60 s wait for a response's headers wrapped the whole upload: the daemon reads a body before it answers, so a 100 MB chat move on a slow uplink, a first snapshot push of a big repository, a 20 MB blob, all failed at 60 s as "isn't reachable"; the 10-minute budget above it never applied. | A request with a body (over 64 KB) is answered in the body's time, 10 minutes. |
| `system_ssh.rs` `forward` | The long-lived `ssh -N` forward's stderr was piped but never read after it was up: ssh writes a line for every forwarded connection that fails (the daemon down, the client trying every ≤60 s); once the pipe filled, ssh blocked and forwarded nothing, while `try_wait` said it was alive.      | Drained and dropped.                                                           |
| `client.rs` `session`     | Any session that reached the read loop counted as served and reset the backoff: a daemon in a crash loop was reconnected every second, for ever, each time with `configure`, `catch_up` and `pull_all`.                                                                                            | Served means it stayed up 30 s.                                                |

### `rovik-agentd`

| Where                   | Was                                                                                                                                                                                                                                                                                                                                                                                          | Now                                                                                                                                                 |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cli.rs`                | `--root DIR` (a folder standing in for the system, for development) was honoured through sudo, which lets everyone run `user` and the administrators `admin`: `DIR/etc/rovik/agentd.toml` of the caller's (`allow_unsigned = true`) and a link `DIR/opt/rovik/bin → /opt/rovik/bin` would have root install an unsigned binary into the real `/opt/rovik/bin`, past the release's signature. | `--root` is refused for the installed binary, the one the sudoers rule runs; a development build takes it whoever runs it, as the install tests do. |
| `server.rs`             | A request body that took longer than its 10 minutes, or a link that broke, was answered `413 Too Large` (`TooLarge`): the client took a retryable failure for a permanent refusal.                                                                                                                                                                                                           | `408`, as a request to make again; the same code is one helper for both bodies.                                                                     |
| `changes.rs` `discard`  | `WorkDiscard` reset the worktree but kept the result committed before it: `WorkResults` after a reconnect offered the discarded changes again.                                                                                                                                                                                                                                               | The result is committed again (none).                                                                                                               |
| `serve.rs`              | Under `--data DIR` the two folders were one, counted twice: the disk cap tripped at half.                                                                                                                                                                                                                                                                                                    | Each folder once (`Home::folders`).                                                                                                                 |
| `deliver.rs` `git_push` | The remote (from `ProjectEnsure`/`DeliveryTest`) went to `git push` where an option could: `--receive-pack=…` would run a program.                                                                                                                                                                                                                                                           | A remote starting with `-` is refused.                                                                                                              |
| `notify.rs`             | When the configured client couldn't be built, the notice went through a default `reqwest` client, without Settings → Network's proxy.                                                                                                                                                                                                                                                        | Logged and skipped.                                                                                                                                 |

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

Reported by the area reviews and traced to the code, but not changed
here: each needs a decision, a design, or more than a local fix. The
line numbers are of this revision.

### High

- `rovik-core/src/overlay/fs.rs:1633` `replay` with `:489` `sweep_orphans`:
  a saved change whose parent folder can't be resolved (a new file in a
  project folder that's gone: a branch without it) is dropped from the
  change list, then the sweep deletes the chat's only copy of it; a
  `manifest.json` that fails to deserialize (written by a newer Rovik
  window) empties the whole overlay the same way. Keep unresolvable
  entries parked (named by `changes_in` and the sweep), and treat a
  manifest parse error as fatal for `open`.

### Medium

- `rovik-core/src/overlay/land.rs:231` and `:283`: a moved and edited
  file's "theirs" comes from the origin path only; a file the user
  created at the destination meanwhile is overwritten without a
  conflict (Undo recovers it). Emit a conflict when `moved_from` is set
  and the destination holds a file that isn't the source.
- `rovik-core/src/overlay/fs.rs:1626` `replay`, `overlay/import.rs:272`,
  `overlay/land.rs:118`: paths from `manifest.json` and from an imported
  result tree aren't validated (`..` reaches outside the project). Run
  `check_name` on every component in `replay` and `import::build`.
- `rovik-core/src/overlay/fs.rs:1496` and `:1680`: a hard-linked file is
  saved once per name with one upper path; after a restart, unlinking
  either name trashes the shared file. Persist link groups, or re-link
  replayed writes by upper path.
- `rovik-core/src/agent_run/mod.rs:1298` `start_run`: the run is
  registered before `Run::prepare` is awaited, with no guard; a panic or
  a dropped caller leaves "The agent is already working in this chat"
  until restart. Take the `registered` guard right after `register`.
- `rovik-core/src/agent_run/subagent_run.rs:1693` `child_for`: a resumed
  `task` call is found by `(parent, tool_call_id)` alone; a provider that
  numbers calls per reply ("function-call-1") can resume the wrong child.
  Key by the reply's message id too.
- `rovik-core/src/agent_run/change_review.rs:1173`: in Autopilot, a
  follow-up change review that can't report (the model failed) on a
  round that isn't the last neither blocks nor re-asks: held high
  findings go unverified and the change lands. The plan review handles
  the same case fail-closed (`plan_review.rs:1365`).
- `rovik-core/src/agent_run/autopilot/driver.rs:432` and
  `artifacts.rs:40`: the saved plan is looked up after the latest Plan
  record only; a Retry of a Plan stage that failed after saving the plan
  reports "No plan was saved". Search from the task's first Plan record.
- `rovik-core/src/agent_run/autopilot/driver.rs:145` with
  `run.rs:590`: a Retry/Continue in a chat whose task ended at an inside
  stage gets Autopilot's permissions (`allow_any_command`, no checkpoint)
  in an ordinary Agent run. Refuse the start, or run it as a plain run.
- `rovik-core/src/chat_request.rs:521` with `agent_run/run.rs:2952`: a
  reasoning model that streams thoughts, then stalls before any text, is
  retried, and the second attempt's reasoning is appended to the first's.
  Reset the reasoning on `Retrying`.
- `rovik-client/src/remote/results.rs:64`: one sequential results worker
  for every host, and `Job::Import`'s pack fetch has no body timeout; one
  stalled host blocks every host's imports and moves back. An idle
  timeout between frames, as the daemon's `push_pack` has.
- `rovik-client/src/http.rs:241` `health` and `deploy.rs:404` `confirm`:
  no timeout on a daemon that accepts the socket and never answers; a
  deploy can hang with no way to cancel. Wrap in `STEP_WAIT`, as the
  connection test does.
- `rovik-agentd/src/moves.rs:85`: an import removes the chat's earlier
  stay before the database transaction that can still fail, and
  `state.json` is updated only at the end; on that path the store keeps
  the old base for the new checkout. Forget the worktree on a failure
  after `clear_stay`, or record the new place first.

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
