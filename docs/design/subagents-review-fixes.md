# Subagents branch: review findings and the plan to fix them

Branch reviewed: `claude/subagents-plan-impl-v6luj1` (head `a2ecfd2`, 39
commits on top of `1859a45`, 141 files, about 34,000 lines added). Design:
[`subagents.md`](subagents.md) and [`subagents-plan.md`](subagents-plan.md).

## 1. What was checked and what holds

| Check                                                       | Result                                                                 |
| ----------------------------------------------------------- | ---------------------------------------------------------------------- |
| `tsc --noEmit`                                              | clean                                                                  |
| `npm test` (frontend)                                       | 87 passed                                                              |
| `npm run test:eval`                                         | 16 passed                                                              |
| `cargo test --features headless`                            | 906 passed, 1 failed (`outline` Perl sample, untouched by this branch) |
| FUSE-only tests (`--ignored subagent_tests child_overlays`) | 15 passed                                                              |

Sound as built: the child-overlay landing (true three-way merge against the
child's base, refusal before any write, journalled apply), the V8 migration
and child-chat hiding, cascade deletes, the `Registered` drop guard, the stop
watch (`send_replace`), approval keys by child id, depth 1 (no `task` for a
subagent), feature gating of the headless binary, and every
frontend/backend event and record shape that was compared.

The six reviews found **9 high**, about **40 medium** and about **45 low**
issues. They cluster into seven root causes, and the plan below fixes the
root causes rather than the symptoms one by one:

1. **Paths are normalised in four different ways** (`plan::as_path`,
   `worker::norm`, `test_writer::is_hidden_as`, `repair::rel_key`), so every
   check that compares paths (group disjointness, write scope, hidden files,
   checkpoints) has a spelling that slips through.
2. **Shell commands are classified by four different matchers** with
   different wrapper handling (`permissions::segment_is_read_only`,
   `worker::is_check_tool`, `workload::classify::skip_wrappers`,
   `permissions::programs`), so `env rm -rf x` is "read-only" for a worker.
3. **"What the file was" is kept in three forms** (`baseline` as lossy text,
   `Checkpoint.files` as bytes, overlay snapshot blobs), filled by four code
   paths, so a rollback can corrupt a binary and a Redo can revert a file the
   chat changed earlier.
4. **Run state that must survive the run lives in memory only** (child
   registrations, the verify carry, group owners, scope grants, the ledger),
   so a reload, a restart, a lead error, or a second run loses or leaks it.
5. **Refusals, retries and resumes sit outside the budgets** (refused tool
   calls, retried group workers, resumed children, fixers), so the limits the
   design promises are not enforced.
6. **Regression detection reads failure keys that mix tests and
   error-shaped lines**, so it is inert on pytest, Go and cargo workspaces,
   and over-eager on lint slips.
7. **The frontend reconstructs parent/child links from live events only**,
   so after a reload a child's approval is invisible and the run hangs.

## 2. Plan overview

| Phase | Theme                                        | Tasks   | Blocks                       |
| ----- | -------------------------------------------- | ------- | ---------------------------- |
| 0     | Shared helpers that later phases use         | P0.1–3  | everything below             |
| 1     | Data loss and hung runs                      | P1.1–8  | release of the branch        |
| 2     | Containment and budgets                      | P2.1–9  | turning any writing kind on  |
| 3     | Verification gate and repair loop            | P3.1–10 | turning the repair loop on   |
| 4     | Plan parsing and reviewer output parsing     | P4.1–9  | plan reviewer, parallel runs |
| 5     | Frontend                                     | P5.1–9  | release of the branch        |
| 6     | Storage, headless runner, evaluation harness | P6.1–10 | milestone G measurements     |
| 7     | Tests for behaviours that have none          | P7      | each phase's close           |
| 8     | Docs                                         | P8      | last                         |

Phases 1 and 5 are release blockers. Phases 2–4 must land before the
corresponding kinds are switched on by default (D14 keeps them off, so they
can follow). Phase 6 only affects measurement.

Every task names the files to change, the defect, the change, and the test
that proves it. Line numbers refer to the branch head.

---

## Phase 0: shared helpers

### P0.1 One path normaliser

- Files: new `src-tauri/src/agent_run/paths.rs`; callers in `plan.rs`
  (`as_path`, 528–566), `worker.rs` (`norm`, 136–147; `extend_scope_paths`,
  204–250), `test_writer.rs` (`is_hidden_as`, 103–115), `repair.rs`
  (`rel_key`, 571–578), `group.rs` (`owner_of`), `causes.rs` (`worker_of`).
- Defect: each site strips a different subset of `./`, `\`, `//`, trailing
  `/`, `..`, and case.
- Change: `pub fn norm_rel(path: &str) -> Option<String>` that replaces `\`
  with `/`, walks components dropping `.` and empty ones, **rejects** any
  `..` and any absolute or drive-prefixed path (returns `None`), strips a
  trailing `/`, and lowercases when
  `cfg!(windows) || cfg!(target_os = "macos")`. Add
  `pub fn under(folder: &str, path: &str) -> bool` (folder equal or prefix
  plus `/`). Replace every local normaliser with these two.
- Test: unit table with `src/./x.ts`, `src//x.ts`, `src/x.ts/.`,
  `src\x.ts`, `SRC/X.ts`, `src/../x`, `/abs`, `C:\x`, `src/` and `src`.

### P0.2 One command unwrapper

- Files: `permissions.rs` (`segment_is_read_only` 540–598, `programs` 351,
  `READ_ONLY_COMMANDS` 445), `worker.rs` (`is_check_tool`/`unwrapped`
  554–660, `writer_command_problem` 716–750), `workload/classify.rs`
  (`skip_wrappers`).
- Defect: `env`, `timeout`, `xargs`, `time`, `nice`, `sudo` are wrappers in
  some matchers and commands in others; `env` is in `READ_ONLY_COMMANDS`, so
  `env rm -rf src/db` is read-only for a step worker (confirmed: a worker's
  bash falls through to `segment_is_read_only`).
- Change: `pub fn unwrap_wrappers(seg: &[String]) -> &[String]` in
  `permissions.rs` that strips `env` (and its `K=V` words and `-i`/`-u X`),
  `timeout N`, `nice [-n N]`, `time`, `xargs` (only the program it names),
  `sudo` (always refuse). Call it first in `segment_is_read_only`,
  `is_check_tool`, `command_modifies_files` (610) and `classify`. Remove
  `env` from `READ_ONLY_COMMANDS`; a bare `env` stays read-only.
- Test: `env rm x`, `env sed -i s/a/b/ f`, `timeout 5 rm x`,
  `env FOO=1 cat f` (read-only), `env` alone (read-only), for all four
  matchers, so they can't drift again.

### P0.3 One "original content" type

- Files: `subagent.rs` (`ChildWrite`), `group.rs` 566–573,
  `subagent_run.rs` 825–847 and 849–878 (`child_writes`,
  `absorb_child_writes`), 1496, `repair.rs` (`Checkpoint`, `note_before_write`
  630–646), `repair_run.rs` 752–763.
- Defect: `ChildWrite.original` is `String::from_utf8_lossy` text and is
  inserted into `Checkpoint.files` (bytes) and `baseline`; a rollback then
  rewrites a binary or Latin-1 file as U+FFFD text (confirmed at
  `subagent_run.rs:860-866`).
- Change: `pub struct Original { bytes: Option<Vec<u8>>, source: Source }`
  where `Source` is `ProjectCopy | ViewAtRoundStart | ViewAtRunStart`;
  `ChildWrite.original: Original`; `baseline` keeps a lossy `String` only
  for diff display, derived on demand. `absorb_child_writes` inserts
  `bytes` into the checkpoint, and when the key is absent reads the file's
  **current** bytes at absorb time instead of the pre-run original (verify
  reviewer L5).
- Test: a group worker rewrites `fixtures/data.bin` (non-UTF-8), the join
  lands it, a later round regresses and is rolled back, and the bytes equal
  the pre-round bytes exactly.

---

## Phase 1: data loss and hung runs (release blockers)

### P1.1 Children registered before they are owned leak as "running" forever

- Files: `subagent_run.rs` 587–596 (`launch_child` → `register_child`),
  151–162 (`go_sub` creates the `Registered` guard), 226–246 (lazy spawn
  inside `buffered`), 185 and 299 (`?` out of `execute_tasks`); `mod.rs`
  `is_running` 370–377, `running_children`; `overlay/chats.rs` 1676.
- Defect: a child that was registered but never polled (queue beyond
  `max_at_once`, or a DB error in `add_tool_result`) is dropped with its
  `runs` entry intact. `is_running(lead)` is then true for the life of the
  process, landing waits forever on `running_children`, and
  `AgentState::stop(lead)` returns false because the lead itself isn't
  registered.
- Change: create the `Registered` guard in `launch_child` and store it in
  the child `Run` (`Role::Sub(sub).registered`), so dropping an unstarted
  child unregisters it. Keep `go_sub`'s explicit unregister (it is a no-op
  on the second call because `unregister` compares `run_id`). On an error
  in `execute_tasks`, set `stop` on every child already spawned and await
  the spawned handles before returning the error.
- Test: lead sends 5 scouts with `max_at_once = 3`; the scripted DB fails
  the first `add_tool_result`; assert `state.lock().runs` has no entry with
  `parent == lead` and `is_running(lead)` is false after the run ends.

### P1.2 A join conflict discards the whole worker and the later members

- Files: `group.rs` 639–680 (`join_one` `Conflicts` and `Err` arms),
  544–551 (`stopped_join`); `overlay/chats.rs` 647–682 (`land_child`,
  `discard_child`).
- Defect: `land_checked` refuses before writing anything, so none of the
  worker's files land; `join_one` keeps 2,000 chars per conflicting file and
  then `discard_child` deletes the overlay; later members that didn't
  conflict are discarded too. The design (F02) says the child is kept.
- Change: on `Conflicts`, keep the child overlay; land the non-conflicting
  paths (add `land_child_except(child, &conflicting)` in `chats.rs` built
  on the same plan), and report the conflicting ones with both full
  versions saved under the chat's checks folder (`check_output`), naming
  the files in the result instead of inlining capped excerpts. Allow
  `task{continue: "<worker handle>"}` to resume the kept child in its own
  overlay. Never discard later members on a conflict; join them after the
  lead resolves it (keep them in `GroupRecord` with status `waiting`).
- Test: w2 changes two files, one conflicts; assert the other landed, the
  child store still exists, w3 landed, and the lead's result names the
  saved versions.

### P1.3 A failed or `Busy` landing throws away finished work and can leave a half-applied journal

- Files: `group.rs` 670–680; `overlay/chats.rs` 664–682 (`discard_child`
  removes `landings/<child>`), 1767–1781 (`land_now_with` checks interrupted
  landings only when the parent is closed); `overlay/land.rs` 435–475
  (`Busy`), 681–746 (`apply` rollback can fail).
- Defect: `Busy` (the editor touched the view mid-landing) discards the
  worker's correct output and starts a fresh worker; a `Failed` apply
  mid-way loses its journal with the child, and the half-landed step lands
  into the project because the interrupted check runs only on a closed
  parent.
- Change: on `Busy`, retry `land_child` up to 3 times with backoff before
  giving up, and keep the child. On `Failed`, keep the child and its
  journal, mark the group `interrupted`, and make `land_now_with` run
  `interrupted_into` whether or not the parent overlay is open. Only
  `discard_child` on `Failed | Stopped` worker status, never on a landing
  error.
- Test: inject `FAIL_WRITING` through the group path with two files; assert
  the journal survives, the child survives, and Land refuses until the
  landing is finished.

### P1.4 Redo records fixer paths as `Untouched` when the snapshot failed

- Files: `repair_run.rs` 545–552 (`unwrap_or_default()`), 580–588
  (`unwrap_or(Before::Untouched)`); `overlay/chats.rs` 2699–2751
  (`snapshot` is all-or-nothing), 2836–2930 (`restore_path`).
- Defect: one unreadable path fails the whole snapshot, the error is
  swallowed, and every file the fixer then changes is recorded as
  `Untouched`; Redo then `revert_file`s them to the project copy, dropping
  changes the chat made in an earlier run. The same happens for a fixer
  file outside `paths`.
- Change: `snapshot` returns `Vec<(String, Result<Before, String>)>`; in
  `add_to_redo` a missing or failed entry takes a per-path snapshot at that
  moment, and if that fails too, the group is marked `redo: unavailable`
  with the reason, never `Untouched`. Snapshot `overlay_paths()` of the chat
  before the fixer runs so a file the chat changed in an earlier run is
  recorded as `Before::File`.
- Test: run 1 edits `src/util.rs`; run 2's fixer edits it with a folder in
  `cause.files` that makes `snapshot` fail; Redo restores run 1's content.

### P1.5 After a reload, a child's approval is never shown and the run hangs

- Files: `src/lib/agentRuns.ts` 403–408, `src/lib/subagents.ts` 112–121
  (`noteRunningSubagent`), `src/components/ToolCallBlock.tsx` 620–636
  (`working`, `approvals`), `src/lib/approvals.ts` 51 and 142,
  `src-tauri/src/agent_run/mod.rs` 809–824 (`RunningSubagent`),
  `events.rs` 137–141.
- Defect (confirmed): `agent_status.subagents` seeds `state.children` only;
  `working()` needs `live()`, and `approvals()` needs `working()`, so after
  a reload `pendingApprovalsOf(child)` is never rendered anywhere and the
  backend waits forever.
- Change: extend `RunningSubagent` with `kind`, `goal`, `model`,
  `startedAt`, `toolCalls` and seed `noteSubagentStarted` from it. Make
  `childId()` fall back to a reverse index of `state.children` by
  `(dialogId, toolCallId)`. Show pending approvals whenever `childId()` is
  known, not only when `working()`. Have `ApprovalRequest` (TS) carry the
  `parentDialogId`/`parentCallId` Rust already sends, and use them in
  `pendingApprovalsOf` so a child approval is findable without live state.
- Test: `agentRuns` reload test: status with one subagent and one approval
  keyed by the child; assert the task block's `approvals()` is non-empty
  and `meta()` isn't "starting…".

### P1.6 Child approvals are hidden inside a collapsed "Called task" group

- Files: `src/lib/toolGroups.ts` 27 (`standsAlone`), 290–291
  (`describeGroup`), `src/components/ToolCallGroup.tsx` 42–45
  (`awaiting`).
- Defect: `task` isn't stand-alone, parallel scouts collapse into one row,
  and `awaiting()` checks approvals keyed by the lead, so a child approval
  never opens the group (D8 unmet).
- Change: add `'task'` to `standsAlone`; a `task` case in `describeGroup`
  ("Scout: <goal>"); `awaiting()` also consults the child id of each `task`
  call and `pendingApprovalsOf(child)`.
- Test: group of three `task` calls with one child approval renders open
  with an approval bar.

### P1.7 The stale verify carry is destroyed by an older "needs approval" record

- Files: `mod.rs` 231–241 (`take_carry` removes on mismatch), `run.rs`
  450–460, `verify.rs` 497–500.
- Defect: run 2's carry replaces run 1's; clicking run 1's still-live
  button removes run 2's carry and runs the checks with run 1's file list
  and no history, baseline or checkpoint, silently.
- Change: `take_carry` only removes on a match. When a run starts in a chat,
  mark older `needs-approval` records `superseded` (new status; the UI hides
  the button). When no carry matches, write `stateRestored: false` into the
  record and say so in the lead's message.
- Test: two runs end `needs-approval`; approving the first is refused as
  superseded; approving the second finds its carry.

### P1.8 Carries are never freed

- Files: `mod.rs` 201, 231–241; `verify.rs` (`VerifyCarry`: `checkpoint` up
  to 64 MB, `baseline` uncapped); `chat_db/commands.rs` delete path;
  `overlay/chats.rs` `forget`.
- Change: remove the carry when the chat is deleted or discarded (next to
  the `check_output::clean` call), when a new run in the chat ends by any
  path other than `needs-approval`, and when a carry is older than 24 h.
  Cap `baseline` by spilling file contents past 8 MB to the chat's checks
  folder (keep hashes in memory).
- Test: delete a chat with a carry; assert `carries` is empty.

---

## Phase 2: containment and budgets

### P2.1 Refused tool calls count toward the budget and loop detection

- Files: `execute.rs` 326–417 (all subagent refusals return via
  `add_tool_result` without `guard.record`), `subagent_run.rs` 929
  (`over_budget`), 1013–1016 (in-reply split), `guard.rs`.
- Defect (confirmed): a child that keeps calling tools it doesn't have, or
  writing outside its scope, never increments `tool_calls`, so the only
  bound is wall time, checked once per turn.
- Change: route every refusal through
  `guard.record(FinishedCall { rejected: true, .. })` before
  `add_tool_result`, so the budget, `SubagentProgress.tool_calls` and
  `rejectedAgain` detection all see it.
- Test: scout with `max_tool_calls: 3` issues 10 refused `write` calls in
  one reply; assert status `partial` after the first turn.

### P2.2 Wall time is checked inside a reply and bounds command timeouts

- Files: `subagent_run.rs` 949–956, 1013–1027; `tools.rs` ~808
  (`timeout_seconds`).
- Change: in the in-reply split, stop executing remaining calls once
  `worked >= wall`, answering them with the budget message; clamp a
  subagent's bash `timeout_seconds` to `wall - worked`.
- Test: worker with a 10 s wall budget calls `bash sleep 60`; the child ends
  `partial` within about 10 s.

### P2.3 `extend_scope` is capped and persisted

- Files: `execute.rs` ~1460–1490, `worker.rs` 181–250
  (`extend_scope_paths`), `group.rs` 90–110 (`refusal`),
  `subagent_run.rs` 455–458 (`start_follow_up` rebuilds the scope),
  `subagent.rs` 784–812 (`for_lead`).
- Defect: a worker may grant itself `src/` or `.github/` at once; grants are
  lost when the worker is continued; grants and notes are appended to the
  result head without a cap.
- Change: refuse folder grants (files only) unless the folder is under one
  of the step's own folders; cap grants at 8 per worker; refuse paths in
  another step's scope in every mode, not just groups; store grants in the
  child chat's meta and reload them in `start_follow_up`; cap the notes
  section of `for_lead` at 1,500 chars.
- Test: `extend_scope{paths:["src/"]}` refused; `["src/api/x.rs"]` granted;
  a continued worker still writes `src/api/x.rs`.

### P2.4 Hidden files are hidden under every spelling and every tool

- Files: `test_writer.rs` 103–115, `tools.rs` 640–648 (`read`), 909
  (`search_code`), 949–968 (`callers`, `neighbours`, `diagnostics`).
- Change: use `paths::norm_rel` on both sides; filter `callers` and
  `neighbours` results by `is_hidden`; refuse `diagnostics` on a hidden
  path.
- Test: `read src/./greet.ts`, `read src//greet.ts`, `callers greet` and
  `diagnostics src/greet.ts` all refused for the test writer.

### P2.5 The test writer's bash guard sees shell expansion

- Files: `worker.rs` 712–717 (`names_file`), 732; `permissions.rs` 184–316
  (`parse_command`).
- Defect: `… | grep -rn "" *`, `… | head -c 9999 ./src/gre*`, `$PWD/src/x`
  read the code under test because only literal, existing, relative paths
  are checked.
- Change: in `writer_command_problem` for the test writer, refuse a filter
  segment whose arguments contain `*`, `?`, `[`, `~`, `$` or `{`; in
  `parse_command`, mark words with those characters as opaque for every
  caller.
- Test: the three commands above are refused; `cargo test 2>&1 | tail -50`
  still allowed.

### P2.6 Out-of-scope files never land from a child

- Files: `group.rs` 555–565, `overlay/chats.rs` 647–662 (`land_child`).
- Defect: the scope is enforced on the `write` tool only; whatever a check
  leaves in the child view (fixtures, caches, a sibling's file edited by a
  side effect) lands through the three-way merge.
- Change: before `land_child`, split the child's changes into in-scope and
  out-of-scope (`WriteScope::allows` with grants); land only the in-scope
  set (reuse `land_child_except` from P1.2) and report the rest to the lead
  by path; treat an out-of-scope path as a seam for F06 attribution.
- Test: a check writes `src/posts.rs` from w1's overlay; assert it isn't in
  the parent view after the join and is named in the result.

### P2.7 Retried and resumed workers count against the limits

- Files: `group.rs` 230 (admissions for the initial members), 328–357
  (`fork_worker` for retries), `subagent.rs` 1026–1044 (`admit_worker`,
  `MAX_SAME_BRIEF`), `subagent_run.rs` 360–377 (resume with
  `Guard::new(None, 0)`), 371–388 (admit before `new_sub`/`store_child`).
- Change: `admit_worker` inside `fork_worker`, including retries, and roll
  back admissions when the group is refused before starting; on resume,
  seed `guard.tool_calls` from the child's stored tool-call count; admit
  after `new_sub` succeeds and delete the child row if `launch_child` fails.
- Test: 6-step group where every worker fails once; assert at most
  `steps + 2` workers ran.

### P2.8 `continue` by call id goes through the same rules as by handle

- Files: `subagent_run.rs` 1673–1687 (`writes_task`), 403–409 (goal cap),
  `chat_db/store.rs` 352–375 (`child_by_handle`).
- Change: resolve the handle first and decide "writes" from the child's
  stored `kind`; apply `MAX_GOAL_CHARS` and `max_tool_calls` to follow-ups.
- Test: `continue` of a worker by its call id in a batch with two scouts
  runs alone.

### P2.9 Worker outcomes match what they wrote

- Files: `subagent_run.rs` 1546–1580 (`after_worker`), 1586–1660
  (`writer_end`); `group.rs` 526–543.
- Defect: a step is ticked when the worker wrote nothing; a `Blocked` or
  `Unformatted` worker's half-step is kept (and landed, in a group) without
  the "kept" note a `Partial` one gets.
- Change: mark a step done only when `result.writes` is non-empty and the
  mismatch note didn't fire, else downgrade to `partial`; treat `Blocked`
  and `Unformatted` like `Partial` for the kept-changes note, and in a
  group give the lead the choice by not landing them until it says so.
- Test: a worker that writes nothing and says done leaves the step unticked.

---

## Phase 3: verification gate and repair loop

### P3.1 Regression detection works on pytest, Go and cargo workspaces

- Files: `command_output.rs` 381–396 (GENERIC rule), 293 (Rust keys drop
  the crate), `RUST_SUMMARY`; `repair.rs` 79–95 (`broke_passing`).
- Defect (confirmed): traceback and summary lines produce error-shaped keys
  next to the test keys, so `errors_before` is true and nothing is ever a
  regression; `error: 2 targets failed:` becomes a key.
- Change: in `failure_keys`, don't emit GENERIC keys from a check whose
  output already yielded test keys, and drop GENERIC lines whose message is
  a bare exception class; add `N target(s) failed` to `RUST_SUMMARY`;
  include the crate in Rust test keys. In `broke_passing`, compute
  `errors_before` only from keys with a code or a file plus code.
- Test: fixtures for a pytest traceback, a Go `t.Errorf` line and a cargo
  workspace summary through `failure_keys` → `judge`, asserting a regression
  is found.

### P3.2 A lint slip in the fixed code isn't a regression

- Files: `verify.rs` 577–618, `repair.rs` 79–83.
- Change: a broken key is a regression only when it is a test key, or an
  error in a file the round did **not** write (`checkpoint.files`);
  otherwise it is reported as a new failure and the fix is kept.
- Test: round 2 passes the test and adds an unused import; assert
  `Progress`, no rollback.

### P3.3 A timed-out check is incomparable, not progress

- Files: `command_output.rs` 398–402, `repair.rs` 85 and 147–176 (`judge`).
- Change: when either side's key for a check is opaque, drop the check from
  `both` so nothing counts as fixed or broken for it.
- Test: round 2 times out; assert `First` for that check and no "resolved"
  message.

### P3.4 Rollback restores deletions and tells the editor

- Files: `repair.rs` 720–729, 753–763; `agent_run/events.rs`.
- Change: emit a file-removed event (a `FileWritten` with `deleted: true`)
  on each rollback deletion and clear the `seen` entry; in direct mode
  treat `current == None` as ours when `seen` holds the checkpointed hash.
- Test: run creates `c.txt`, rollback removes it, the frontend store has no
  buffer for it.

### P3.5 Direct-mode rollback is honest about commands

- Files: `verify.rs` 181–210, `repair.rs` 651–679, `execute.rs` 828–834.
- Change: a round-scoped flag "a file-modifying command ran"; when set in a
  non-isolated chat, append "commands ran this round; their changes weren't
  undone" to the rollback note and set `writes_since_verify = true` so the
  checks run again.
- Test: fix = `write a.ts` + `bash sed -i … b.ts`; after rollback the checks
  re-run and the note names the command.

### P3.6 The R08 fix round is protected and ends as `verified`

- Files: `verify.rs` 430–461, 667–674.
- Change: call `take_checkpoint()` instead of clearing it when findings go
  back after green; return `verified` (not `done`) when the last round
  passed and nothing changed since.
- Test: review-after-green fix breaks a test → rolled back; a no-write reply
  ends `verified`.

### P3.7 "Fewer tests ran" is measured against the run's first test count

- Files: `verify.rs` 547–556, 538–540; `repair.rs` 190–200.
- Change: compare with the highest `tests_ran` seen before the run's first
  write to a test file; skip the flag when every per-file integrity check is
  clean; limit `without_regression` to keys named in `integrity_open`.
- Test: the run adds two tests then removes one of its own; no flag.

### P3.8 Integrity rules cover the common bypasses, and fail closed elsewhere

- Files: `repair.rs` 288–319, 324–355, 377–404, 440–517.
- Change: flag any change to a test-path file whose language isn't
  supported; add `conftest.py`, `pytest.ini`, `[package] autotests/autobins`,
  `pytestmark = pytest.mark.skip`, `#[cfg(feature = …)]` on a test module,
  and `if (false)`/early `return` at the top of a test body. Keep the
  decision with the user (a flag, not a block), as today.
- Test: one fixture per bypass, each flagged.

### P3.9 Carry survives a restart or says it didn't

- Files: `verify.rs` 103–107, `run.rs` 459.
- Change: persist with the `needs-approval` record the baseline paths with
  content hashes and the project copy's hash; on approval after a restart,
  rebuild what can be rebuilt and set `integrityChecked: false` otherwise,
  and never report `verified` when the baseline was lost (report `passed,
integrity unchecked`).
- Test: weaken a test, restart (new `AgentState`), approve; the record isn't
  `verified`.

### P3.10 Fixers draw on the documented limit and are launched safely

- Files: `repair_run.rs` 472–474, 483–492; `subagent.rs` 1026; `verify.rs`
  142 and 257 (`maxRounds`), `repair_run.rs` 264–270.
- Change: fixers draw on the shared 12 (D11), not the worker budget; carry
  `ledger` in `VerifyCarry`; append the fixer message to the child chat only
  after `launch_child` succeeds; pass `max_rounds()` into `record()` and
  `failure_report` ("attempt N of M"); add an explicit `recheck_count` cap.
- Test: `admit` refuses the 13th fixer; "attempt 4 of 4" after a bonus
  round.

---

## Phase 4: plan parsing and reviewer-output parsing

### P4.1 Group disjointness covers folders, new files and read-only members

- Files: `plan.rs` 701–708, 736–744; `worker.rs` 111–150
  (`WriteScope::for_step`, `new_in`).
- Defect (reviewer-verified): the check compares exact strings; `src/api/`
  and `src/api/mod.rs` in one group pass, two `(new)` files in the same
  folder give both workers `new_in` on it, and a member may change a file
  another member lists as `(read)`.
- Change: normalise with `paths::norm_rel`; a folder overlaps any path
  under it (`paths::under`); a `new_in` folder is exclusive to one step per
  group; a file any member changes may not appear in another member even
  read-only.
- Test: the three cases above are refused with the step numbers named.

### P4.2 Marks apply to the adjacent path only

- Files: `plan.rs` 591–608.
- Change: match `` `path` (new|read) `` adjacency; apply line-wide only when
  the line has one path. Fix the unit test that asserts the current
  behaviour.
- Test: `` `a.rs` (new): register in `routs.rs` `` leaves `routs.rs` subject
  to the existence check.

### P4.3 `as_path` and `names_a_file` agree

- Files: `plan.rs` 120–142, 528–566, 596.
- Change: with a `/`, accept any `name.ext` with 1–10 alphanumeric
  extension chars; reject `..` segments; normalise `\`; make `plan_problems`
  use `steps()[i].files.is_empty()` so the form check and the structure
  share one definition; require backticks in both or neither (choose
  neither: accept unbackticked paths in `steps()` too).
- Test: `src/grammar.pest`, `tests/x.snap`, an unbackticked path, and
  `src\x.rs` each give a non-empty scope.

### P4.4 Step numbering, groups and headings are robust

- Files: `plan.rs` 416–418, 583, 614–617, `STEPS_HEADING`, `split_steps`.
- Change: use the positional index for logic and the written number for
  display; read `Group:` only from unfenced lines, lowercase the name,
  ignore `none`; pick the `## Steps` heading exactly before falling back to
  the first heading containing "step"; handle CRLF everywhere by
  normalising line endings once at parse entry.
- Test: a CRLF plan, `1. 1. 1.`, `- Group: none`, and a "## Next steps"
  section before "## Steps".

### P4.5 Reviewer findings parse the way models write them

- Files: `change_review.rs` 126–137, 152, 166, 219–226, 264–276
  (`same_finding`); `plan_review.rs` 364.
- Change: strip backticks and a trailing `:` from the location before
  `AT_LINE`; take the first word of `Confidence`; treat a Findings body
  starting with `none` as empty; accept `- **Scenario:**`; in
  `same_finding`, require a title match when either side lacks a line or
  when both are step findings.
- Test: a round-2 finding about a different problem in the same step is
  kept; `` `src/a.ts:42` `` yields `line = 42`.

### P4.6 Plan review records every outcome

- Files: `plan_review.rs` 287–295.
- Change: a revision after a Hold is reviewed regardless of size while
  `plan_findings` is non-empty; a save made because the review budget is
  used writes a record saying so.
- Test: Hold, then a 7-line revert; the held finding is still surfaced.

### P4.7 Mid-run check output survives the run's end

- Files: `verify.rs` 505–507, 872–882.
- Change: track `cleaned_this_run` and skip `clean` when the folder already
  has this run's files.
- Test: `verify_now` output is readable after `verify_at_end`.

### P4.8 Triage says what it was given

- Files: `triage.rs` 138–160 (`paths_in`), 286, 446–451.
- Change: the brief says "the focused part (N chars; the whole output wasn't
  saved)" on the unsaved path; `paths_in` without backticks requires a `/`
  or a known extension.
- Test: `Node.js` and `e.g.` are not paths.

### P4.9 Small panics and probes

- Files: `plan.rs` 336 (`rest[1..]`), 660 (existence probe outside root).
- Change: `strip_prefix(['/', '\\'])`; reject `..` before probing.

---

## Phase 5: frontend

### P5.1 Plan-review records keep their state

- Files: `src/components/MessageList.tsx` 157–168, 1302.
- Change: `<For>` over the `submit_plan` calls (stable references) and
  derive `meta` per row, or key by `callId` with `<Index>`.

### P5.2 Fallback chain order for a kind set to Default

- Files: `src/lib/subagentSettings.ts` 260–262.
- Change: `[default, ...k.fallbacks, ...defaultFallbacks]` as the design
  table states; add a test with own fallbacks.

### P5.3 Token totals include triage

- Files: `src/lib/subagents.ts` 231, `src/lib/verification.ts` 60–71.
- Change: `TriageMeta.usage?: SubagentUsage`; include it in `runUsage`.

### P5.4 Transcript loading doesn't drop rows and retries on error

- Files: `src/lib/subagents.ts` 134, 180, 183.
- Change: buffer rows received while `loading` and merge by id after the
  page lands; delete the entry on error so the next click retries.

### P5.5 Types match what Rust sends

- Files: `src/types.ts` 232–244 (`harness: 'planCheck'`),
  `src/lib/changeReview.ts` (`ReviewMeta.afterGreen`), `reviewHeadline`.
- Change: add both; count only `high` as "sent to the agent" after green.

### P5.6 Missing styles

- Files: `src/styles.css`.
- Change: add `.chat-verify.stopped`, `.chat-verify.running`,
  `.chat-review.running`, `.chat-verify-triage`, `.chat-verify-repair`,
  `.subagent-block`, `.subagent-group`, `.subagent-context`,
  `.subagent-limits-toggle` using existing tokens, both themes.

### P5.7 Settings page details

- Files: `src/components/SubagentSettings.tsx` 224–262, 272;
  `src/components/ChatPanel.tsx` 1910.
- Change: disable "parallel groups" and "red check" with a reason when the
  virtual-file driver isn't available; `placeholder="no limit"`; don't write
  `subagents` from ChatPanel's own saves when the last settings reload
  failed.

### P5.8 Duplicate call ids across replies

- Files: `src/components/MessageList.tsx` 145–151, `src/lib/subagents.ts`
  `live`.
- Change: key `toolResults`, `planReviews` and `live` by `(messageId,
callId)`.

### P5.9 Redo error wording and store bounds

- Files: `src/components/IsolationBar.tsx` 153–158; `src/lib/subagents.ts`
  46–54.
- Change: separate the two `try` blocks and word the second error; evict
  `live` entries once `done` exists and transcripts when their block
  unmounts.

---

## Phase 6: storage, headless runner, evaluation harness

| Task  | Files                                                                      | Change                                                                                                                                                                                                                     |
| ----- | -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P6.1  | `scripts/eval/eval.mjs` 73–105                                             | Track the live child; on `SIGINT`/`SIGTERM`/`exit` kill its process group, then re-raise; add `child.on('error')`; `fakeModel` rejects on exit before output.                                                              |
| P6.2  | `run.rs` 1546–1554, `headless.rs` 254–262, 296–305, 807–815                | Log `usage` on asked-again replies; a usage object with neither prompt nor completion keys counts as `missing_usage`; fix the test that enshrines the opposite.                                                            |
| P6.3  | `project_profile.rs` 730–740, `agent_run/log.rs` 21                        | Format `line + "\n"` and `write_all` once under a `static Mutex`; the watcher resets its offset on shrink and reads the rotated file.                                                                                      |
| P6.4  | `headless.rs` 599–601, 470–479, 386–387, 380–384                           | Poll for the run's `"event":"run"` line (bounded) instead of a 300 ms sleep; attribute lines by `parentDialogId`; RAII reset of the log dir; remove an auto-created `--data` dir after the report.                         |
| P6.5  | `scripts/eval/eval.mjs` 158–218                                            | Copy (reflink when possible) `node_modules` into the workspace and use a per-run venv, or make the caches read-only; warn when a cache hash changed after a run.                                                           |
| P6.6  | `scripts/eval/eval.mjs` 560–566, 633; `lib.mjs` 165, 187                   | Exclude null costs from means; refuse to project when any exist.                                                                                                                                                           |
| P6.7  | `chat_db/store.rs` 588+, `chat_db/commands.rs` 203–258                     | Skip FTS indexing for child chats; run `remove_chat_files` for each child when a parent is deleted.                                                                                                                        |
| P6.8  | `overlay/chats.rs` 2681–2687, 2935–2990, 575–603, `test_writer.rs` 258–268 | Delete a group's blobs when redone or superseded; open the overlay once in `redo_group`; give scratch overlays a `parent.txt` and sweep them at init; on a resumed group `task`, land or discard the saved children first. |
| P6.9  | `overlay/chats.rs` 327–345, `land.rs` 756–786                              | Record the parent chat id in a landing and compare ids, not mountpoint paths, in `is_chat_view`/`interrupted_into`.                                                                                                        |
| P6.10 | `scripts/eval/README.md`, `permissions.rs` 364–407                         | State Linux/macOS only; document that denied-command matching is word-based; normalise `rm -r -f`/`rm -fr` to `rm -rf` before matching.                                                                                    |

---

## Phase 7: tests to add

Beyond each task's test above, these behaviours have none today and should
get one in the phase named:

| Phase | Behaviour                                                                                             |
| ----- | ----------------------------------------------------------------------------------------------------- |
| 1     | Parent Stop while a child waits on an approval; `is_running(lead)` after a child panics mid-batch     |
| 1     | Two children landing the same path without a line conflict (silent merge into a sibling's file)       |
| 2     | A subagent's tool list never contains `task` (assert on the schema list, not just a refusal)          |
| 2     | `max_per_run` reached end to end through the lead; `MAX_SAME_BRIEF` across repair rounds              |
| 2     | Model fallback after a trim (`trimmed` reset on `next_model`); an error on the report turn            |
| 3     | A regression caused by fixers in a recheck round; `Checkpoint.too_big`; Stop mid-round                |
| 3     | Headless approval answering end to end; `--timeout` → `agent_stop` → `"timeout"`                      |
| 4     | B07 project checks on an overlay root where a file exists only in the chat's view                     |
| 4     | Plan review across an approval wait and across a second run                                           |
| 5     | `TaskBlock`/`ReviewRecord`/`VerificationRecord` component tests: Stop enablement, interrupted display |
| 6     | Project delete with child chats; `new_log_lines` with a partial trailing line and after rotation      |

Also unignore the FUSE tests in CI where `/dev/fuse` exists (they passed
here in 34 s), so the group, repair and Redo paths are run on every push.

---

## Phase 8: docs

- `subagents.md` §4: the result contract says `## Status`; the code derives
  status from the format. Replace the section with what `subagent.rs`
  actually parses (`## Answer`/`## Summary`, `## Blocked` only as a last
  section).
- `subagents-plan.md` D7: state that the write scope is enforced on the
  `write` tool and on landing (after P2.6), and that commands run by checks
  can still write anywhere inside the child's view.
- `subagents-plan.md` D11: say which limit fixers draw on (after P3.10) and
  that retries count.
- `subagents-plan.md` "Fallbacks" row: match P5.2.
- `scripts/eval/README.md`: platform, cache sharing, Ctrl+C behaviour.

---

## 3. Ordering and dependencies

```
P0.1 ──► P2.4, P4.1, P4.3, P2.3
P0.2 ──► P2.5 (and closes the env bypass on its own)
P0.3 ──► P1.4, P3.4, P3.5
P1.1, P1.5, P1.6, P1.7, P1.8 ── independent, do first
P1.2 ──► P1.3 ──► P2.6
P3.1 ──► P3.2 ──► P3.3
P3.9 ──► P3.10
P5.* ── independent of the Rust phases except P1.5/P1.6 (which touch both)
P6.* ── independent
```

Rough size: Phase 0 about 400 lines with tests; Phase 1 about 900; Phase 2
about 700; Phase 3 about 900; Phase 4 about 500; Phase 5 about 400; Phase 6
about 500. The order inside a phase is the task number.

## 4. Deliberately not planned

- `task` calls skipping the "approve every tool" rule: document, don't
  change (the lead's own tool calls inside the child are still approved).
- Depth-1 enforcement, approval keying, SQL building, the V8 migration and
  the landing merge itself: reviewed, no change needed.
- Denied-command evasion through `$(…)`, aliases or interpreters: the
  policy targets a cooperative model; document the limit (P6.10) rather
  than attempt a sandbox.
- The outline `sample.pl` test failure: code untouched by this branch.
