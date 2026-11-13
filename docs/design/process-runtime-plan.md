# ProcessRuntime implementation plan

Status: planned; no runtime implementation is included in this document.

This schedules phase 6 of the [backend refactoring plan](backend-refactoring-plan.md).
It builds on the refactoring merged in `06c57d6`
([PR #233](https://github.com/piliugin-anton/rovik-ide/pull/233)), where `PlanDoc`,
the nine run-state objects, and whole-object verification carries are implemented.
This planning branch is based on `master` after that merge. Do not recreate those
extractions. The earlier tool registry, typed tool outcomes and shared message-row
decoder also remain in place; this work does not reopen dispatch or storage design.

## Outcome and boundaries

Make the owner and lifetime of the backend's process services explicit. Reject a
second production initialization before it can repoint data folders. Make normal
exit and startup failure use the same cleanup path. Then move the mutable
registries behind their owners, so separate test cores cannot stop or overwrite
one another's services.

Keep the three ownership levels distinct:

- `ProcessRuntime` owns services shared by a host's core and all its runs.
- `Core` retains its database, agents, browser, MCP servers, virtual-files state,
  and event sink, and holds an `Arc<ProcessRuntime>`.
- `Run` retains its 47-field shape and its existing state objects. Runs and
  subagents share their core's runtime; they never initialize process services.
  `VerifyCarry` continues to carry fix-round state, without runtime handles.

Production continues to have one core per process (D2 in
[the cloud backend plan](cloud-agent-dev-plan.md)). This is not a multi-account
or multi-core production feature. Stored rows, protocol messages, command
approval, sandbox policy, scheduling rules, and overlay formats stay compatible.
The intended behavior changes are explicit initialization refusal and reliable
cleanup, including the headless host.

## Current code to migrate

Paths below are relative to `src-tauri/`; unqualified core-module paths are under
`crates/rovik-core/src/`. Follow the named functions rather than the historical
line numbers in the original plan.

| Area                    | Current code and ownership problem                                                                                                                                                                                                                        | Destination                                                                                                                                           |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Core lifecycle          | `crates/rovik-core/src/core.rs`: `Core::new` opens the database, then initializes workload and overlays; public `Core::open` bypasses that initialization. `Core::shutdown` also shuts down process globals.                                              | `ProcessRuntime` coordinates common initialization and shutdown; one internal core-construction path.                                                 |
| Workload                | `workload/{mod,history,machine,queue,tracker}.rs`: `SETTINGS_FILE`, `SETTINGS_SEEN`, `HISTORY`, `SLOTS`, `SHARED`, sampler `STARTED`, and `SHUTTING_DOWN`. The sampler is an unjoined thread; shutdown permanently sets its global flag.                  | A concrete `Workload` service, with history, queue, machine locks, settings tracking, and a stoppable sampler.                                        |
| Chat overlays           | `overlay/chats.rs`: `DATA_DIR`, `OPEN`, child links, gates, notices, running/direct-write ownership, conflict and peer caches. `keep_saving` uses `Once` and an endless saver thread; `check_after_quiet` has its own `WAITING` set and detached workers. | An `Overlays` service containing the whole related registry, its synchronization, and its maintenance workers.                                        |
| Background commands     | `command_tool/background.rs`: `ALL`, `NEXT`, and `TELLING`; `stop_all` currently reaches every command in the process.                                                                                                                                    | A `BackgroundCommands` registry, retaining the existing command/report types and notification ordering.                                               |
| Live connector sessions | `connectors/claude_code.rs`, `connectors/codex/mod.rs`, and `connectors/codex/rovik_tools.rs`: four `LiveRuns` statics, for Rovik-tools sessions and each harness's own-tools steering.                                                                   | Connector-owned concrete session state, reached through the runtime and the existing connector interface.                                             |
| Chat holds              | `chat_hold.rs`: global `HOLDS` shares chat-ID counts and a configurable lock directory. `Holds` and `Hold` already support an explicit owner, but runs, deletion and client move/import paths use global helpers.                                         | Reuse one owned `Arc<Holds>` per runtime; preserve the production OS-lock paths and existing `Hold` behavior.                                         |
| Executor                | `rt.rs::handle` lazily creates and leaks one Tokio runtime; Tauri adopts its handle. Tests rely on its tasks running beside a blocked caller.                                                                                                             | Keep the one process executor. `ProcessRuntime` owns service lifetimes, not an independently created Tokio runtime.                                   |
| Host process setup      | Desktop `src/lib.rs`; daemon `crates/rovik-agentd/src/{serve,daemon}.rs`; headless `crates/rovik-headless/src/lib.rs`. Setup and shutdown are split across these entry points.                                                                            | An explicit startup/cleanup sequence in each host, with common service lifecycle delegated to `ProcessRuntime`.                                       |
| Test construction       | `rovik-core/src/testing.rs::TestCore::new`, `core.rs` tests, client/daemon integration helpers, and overlay `TEST_DATA`. Many tests use `Core::open`; some spawn their own test binary to isolate global state.                                           | Isolated service owners for tests, using the same core assembler and service initialization code. Retain process isolation for actual process policy. |

The overlay move must include the less obvious state: `TOLD`, `CHILDREN`, `GATES`,
`NOTES`, `RUNNING`, `CHAT_RESOLVES`, quiet-check `WAITING`, `DIRECT`, `BUSY`, `PEERS`,
`LAST_COMPARED`, `TOLD_PEERS`, `STATE_LOCKS`, and `SHOWN`, as well as the data folder
and open mounts. Moving only `DATA_DIR` and `OPEN` would leave contexts sharing
chat IDs, locks, conflict state, and callbacks.

Browser, MCP, `AgentState`, and `VfsState` already have owners in `Core::Parts`.
Do not add a second copy of them to `ProcessRuntime` or move them merely to make
all fields look alike. `process.rs` remains the child-process creation/tree
cleanup helper, not a service locator.

## Design decisions

### Admission, construction, and failure

Add `process_runtime.rs` in rovik-core and export `ProcessRuntime` from `lib.rs`.
Use concrete services with private fields and operations, following the state
extractions already implemented. Do not introduce a plugin registry, a generic
service container, thread-local current runtime, or a task-local fallback.

The target host sequence is:

```rust
// Existing single-threaded process setup happens before the executor starts.
let runtime = ProcessRuntime::claim(paths)?;
runtime.begin_startup()?;
// Existing host-specific, folder-dependent setup goes here.
let core = Core::new(runtime.clone(), events)?;
let result = run_host(&core).await;
let cleanup = runtime.shutdown(&core).await;
// Preserve the primary error and report any cleanup failure as well.
```

This shows the success path; names describe the target API. Step 2 adds cleanup
for errors at every stage, including startup before a core exists. Host setup
remains ordinary host code.
`claim` reserves ownership and validates paths. `Core::new` attaches one core and
starts common services in the current order: database open, workload setup, then
overlay scratch cleanup and landing recovery. `Core::Parts` gets its paths from
the runtime, so callers cannot supply mismatched runtime and database roots.

- Normalize the storage identity before claiming it: resolve relative paths and
  existing symlink ancestors, including when the final directory does not exist.
  Keep the configured config/log paths too; preserve headless's custom log folder.
- A minimal process-wide admission slot is allowed. It records the selected
  identity and lifecycle, not the service registries. Claim it atomically before
  opening a database, running recovery, or changing folder-dependent host globals.
- Refuse a second independently constructed production runtime, for either the
  same or another data folder. Cloning the existing handle is how callers share
  it. Also refuse constructing a second independent core through the same handle.
- Model `Reserved -> Starting -> Running -> Stopping -> Stopped`, with a failed
  startup recorded separately. Reject new service work once stopping begins.
- `begin_startup` advances the state before folder-dependent host setup. Undo a
  reservation dropped in `Reserved`, before process setup or service side effects. After irreversible host setup, clean up what started and leave a
  failed/stopped production owner; do not promise restart in the same process.
  Existing `OnceLock` values and process security settings are not resettable.
- Unwind partially started services in reverse dependency order. Return errors as
  values with the failing step and path; do not leave a partially usable core or
  mark a failed initialization as a clean shutdown.

There must be no strong cycle from runtime to core to runtime. Shutdown borrows
its matching core to reach its already owned browser/MCP/VFS/database. Guards,
callbacks, and workers retain the narrow service handle they need; long-lived
registry callbacks must not strongly retain that registry through a core.

### Shutdown contract

Concurrent or repeated shutdown calls perform cleanup once and observe its
completion; a second caller must not return while the first is still cleaning.
Never hold a registry mutex across an await, a thread join, a process wait, or
object destruction that can re-enter it. Drain under the lock and clean up outside
it, as `LiveRuns::end_all` already does.

Preserve the current common resource order:

1. Close admission to new work and quiesce host producers/runs.
2. End live connector sessions.
3. Shut down the core's browser, then its MCP servers.
4. Stop background commands and wait for their process trees, before their views
   can be unmounted.
5. Unmount the core's virtual-files mount.
6. Stop/join overlay maintenance workers, flush overlays, and close their views,
   children before parents as `close_all` does today.
7. Resume paused workload jobs, stop/join the sampler, flush history, and release
   held heavy-work locks after producers have stopped. Release chat holds only
   after their runs, moves/imports and child processes have finished.
8. Mark the database clean only after cleanup has completed successfully.

A failed step must not skip later cleanup. Return an aggregated error with each
failed stage; preserve the interrupted/unclean marker when cleanup is incomplete.
Use bounded waits consistent with existing command and daemon deadlines. A timeout
must be visible and must not be reported as a successful clean exit. On timeout,
retain ownership of affected state and leave the database/data folder available
for recovery; do not remove it under a worker that is still running. Async cleanup
is explicit: `Drop` may signal cancellation and release synchronous guards, but
must not call `block_on` from a Tokio worker or pretend to have awaited shutdown.

### Host boundaries that must survive the move

| Host     | Startup requirements                                                                                                                                                                                                                                    | Cleanup requirements                                                                                                                                                                                                                                                                                                                 |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Desktop  | Keep confinement setup and `process::adopt_login_shell_path` before runtime threads; keep Tauri on `rt::handle()`. Once paths are known, claim before slot/chat-hold/network setup. Retain secret-provider callbacks and instance ownership in the app. | Preserve `before_exit`: terminals `end_all`, common runtime shutdown, instances `release`, terminals `wait_ended`; cover update-restart paths too.                                                                                                                                                                                   |
| Daemon   | Keep umask, login shell/PATH, private folders, home lock, protected paths and TMPDIR setup in their existing safe order. Claim before common services start. Retain toolchain setup, network namespace setup and per-computer configuration behavior.   | Preserve draining while clients can still hear events, services stop, common shutdown, restart bookkeeping and socket removal. Both startup-stop and normal-stop paths must call it; errors after core creation must also clean up. Track and cancel host sweep/GC tasks that can still use the core.                                |
| Headless | Replace the production `Core::open` bypass with the common construction path. CLI setup and the reusable async `run` entry must not mutate environment settings after threads have started.                                                             | Finish/stop the run, collect its report while the database is available, and always await common cleanup, including timeout, cancellation, report-write failure and other error returns. Then drop core/test references, close the database, and apply existing temporary-data removal rules. Keep CLI exit codes and report schema. |

Host identity, secret stores, network policy and confinement remain host-specific.
Do not put a `HostKind` switch in generic core logic. In particular, daemon
`AgentConfigure` currently updates process-wide network/allowed-host settings
using the last computer's configuration; this plan does not change that policy.

### Service access and the executor

Callers with a core obtain the service from `core.runtime()`. Core-less helpers
receive the particular service/handle they need. Carry those handles into spawned
work and `Drop` guards: for example, `RunningGuard` must remember its `Overlays`
owner and `HarnessGuard` its connector-session owner, rather than recover a global
by dialog ID when dropped.

Move data, synchronization, worker ownership, and all callers of one registry in
the same migration. Temporary compatibility entry points may accept an explicit
owner and delegate; they must not silently use a default production registry or
hold a second copy of the state. Remove them when callers have moved.

Keep `rt::HANDLE` and the existing `rt` helpers as process-wide executor plumbing.
A service worker must capture its service owner and cancellation state, not look
up whichever data folder was initialized last. Shut down these workers without
stopping the shared executor. Tests may share that executor while their services,
files and cancellation remain independent.

A global-static audit is required, not removal of every static in the repository.
`os_lock` slot identity, network configuration,
allowed-host filtering, confinement/protected paths, toolchain host settings, and
OS capability caches remain process policy or host resources for this work. List
these exceptions in the implementation review. Tests that change them keep their
existing locks/subprocess isolation. Read-only constants and stateless caches do
not need to become runtime services.

## Implementation sequence

Each numbered step is a reviewable commit with its own regression checks. Keep
wire messages and existing behavior tests unchanged unless a test is specifically
asserting one of the intended lifecycle changes above.

### 1. Establish lifecycle tests and production admission

Add the runtime type, lifecycle/admission state, path identity checks, and checked
core attachment. Keep current service calls behind it initially. Keep host call
sites compiling through a temporary source-compatible constructor until step 2
switches them to explicit handles. This first commit
must prevent mixed-folder initialization, without claiming test-service isolation
while registries are still global.

Tests: `a_second_runtime_on_another_data_folder_is_refused`,
`a_second_runtime_on_the_same_folder_is_refused`,
`two_concurrent_claims_have_only_one_winner`,
`a_failed_claim_does_not_repoint_or_recover_either_folder`, and
`shutdown_refuses_a_core_from_another_runtime`.
Test the admission state with independently constructed local fixtures; keep the
real production-global admission test in its own process at this stage.

### 2. Route hosts through one lifecycle and guarantee cleanup

Update `Core::new`, desktop setup/`before_exit`, daemon `serve`, headless
`run`/`main_cli`, and exports. Turn `Core::shutdown` into a delegating compatibility
entry point while migrating callers; it must share the same once-only shutdown
state. Use an inner session function plus an outer cleanup path for async error
returns, rather than an async destructor. Make daemon host tasks cancellable.
Keep `Core::open` restricted to the existing test construction path temporarily.

Tests: `shutdown_preserves_resource_dependency_order`,
`concurrent_shutdown_waits_for_the_same_cleanup`,
`a_failed_shutdown_step_does_not_skip_the_remaining_steps`,
`an_incomplete_shutdown_keeps_the_database_unclean`,
`daemon_startup_failure_after_core_creation_cleans_up`,
`headless_success_timeout_and_report_errors_all_clean_up`, and
`shutdown_from_an_async_host_does_not_block_on_its_own_executor`.
Use recording fakes only at existing IO seams to assert order and error paths;
retain real command/mount smoke tests on supported hosts.

### 3. Own workload state and its sampler

Move history/settings, machine slots, queue state and sampler control together
into `Workload`. `Job` and other RAII handles retain their owning service. Replace
sampler `Once`/global shutdown state with owned cancellation and a join handle;
wake both the sampler and queue waiters when stopping. Reject new jobs during
shutdown and resume every paused job. Preserve scheduling, terminal priority,
cross-process slot-lock paths, settings reload, and history merging with other
windows. Do not change the persistent history format or introduce a new scheduler.

Tests: `workload_histories_do_not_share_paths_or_entries`,
`stopping_one_workload_does_not_stop_another`,
`shutdown_resumes_paused_jobs_and_wakes_waiters`, and
`a_fresh_workload_still_samples_after_another_is_stopped`.
Run existing `workload::{history,machine,queue}` tests and command/terminal tests.

### 4. Own chat holds, the full overlay registry, and its workers

First reuse the existing `Holds` type as an owned `Arc<Holds>` and remove the
`HOLDS` default lookup. Give `AgentState` its owner's hold handle and update
run/driver registration, deletion, and client move/import callers. A `forget` in
one runtime must never remove another runtime's holds. Preserve desktop lock
locations and daemon/test behavior without a configured lock folder.

Extract `Overlays` with the complete state inventory above. Move recovery,
scratch cleanup, lookup/open/land/discard operations, locks and watcher entry
points to explicit owners. Keep underlying overlay filesystem/landing algorithms.
Replace saver/recheck `Once`, static sets, detached loops and long uninterruptible
sleeps with owned cancellation and tracked completion. Shutdown must prevent a
late recheck from reopening a view or writing after its data folder is closed.
Update core, app watcher, client remote/move/landing, daemon and headless callers.

Tests: `forgetting_a_chat_leaves_another_owners_holds_intact`,
`the_same_chat_id_in_two_overlay_owners_stays_separate`,
`recovering_one_data_folder_never_touches_the_other`,
`closing_one_owner_does_not_unmount_the_other`,
`shutdown_flushes_children_before_parents`, and
`overlay_workers_stop_before_their_data_folder_is_removed`.
Retain conflict, rollback, interrupted-landing recovery, peer and filesystem
watcher behavior tests. Remove `TEST_DATA` serialization only from converted tests
whose complete mutable dependencies are now owned.

### 5. Own background commands and live connector sessions

Move `ALL`/`NEXT`/`TELLING` into `BackgroundCommands`; pass the owner through command
execution, background-tool handlers, task status, deletion and allowed-host
reconfiguration callbacks. Preserve notification ordering and unread ended-command
reports. Stopping a registry must affect only its processes.

Then move each connector's two `LiveRuns` instances into connector-owned state
held by the runtime. Reuse `LiveRuns<T>` and the existing connector abstraction;
do not add kind-name branches to generic dispatch. Pass owners into harness
callbacks and guards, and continue dropping sessions outside registry locks.
Avoid callback/registry/core reference cycles.

Tests: `stopping_background_commands_leaves_another_owner_running`,
`background_status_is_scoped_to_its_owner`,
`ending_a_harness_leaves_the_same_chat_in_another_owner_running`,
`connector_sessions_drop_outside_the_registry_lock`, and
`shutdown_ends_connectors_and_background_commands_before_unmounting_views`.
Run the existing background, Claude/Codex fake-harness, steering, stop/pause and
allowed-host-reconfiguration suites. Real CLI tests stay opt-in.

### 6. Give tests isolated services and remove bypasses

Provide an explicitly test-only constructor under `cfg(any(test, feature =
"testing"))` that creates owned services without claiming the production host slot
or changing process security/network configuration. Reuse the production service
constructors and core assembler; do not create a second implementation of startup.
Only enable concurrent isolated construction after steps 3–5 have removed the
registry sharing. Independent test services must not run recovery concurrently on
the same directory; fixtures supply distinct roots.

Migrate `TestCore`, raw `Core::open` client/daemon integration helpers, headless
tests, and the original `Core::new` lifecycle test. Replace the public production
bypass with the explicit test-only path. Tests with running services await
teardown before deleting temporary folders; synchronous passive fixtures must
not attempt async cleanup from `Drop`. Keep process-isolated tests for actual
host bootstrap, environment changes, confinement and allowed-host policy. Keep
real VFS mount tests isolated too while their mount location uses the process's
OS slot; do not promise concurrent physical mounts just because service registries
are independent.

Tests: `two_test_cores_can_initialize_and_shutdown_in_one_process`,
`dropping_a_run_uses_its_original_service_owners`,
`a_stopped_test_runtime_cannot_accept_new_work`, and
`closing_one_test_core_preserves_the_other_cores_database_and_services`.
Run under both cargo-test's shared process and cargo-nextest's process model.

### 7. Remove transitional access and update documentation

Audit all remaining calls to global init/close/stop helpers and all
`static`/`Once`/`OnceLock` declarations in migrated modules. Remove compatibility
wrappers and subprocess test shims made obsolete by these moves. Leave the small
production-admission slot and the documented process-policy/executor exceptions.
Update the original backend plan's phase 6, `core.rs`/`rt.rs` module docs, and D2/K18
notes to distinguish the production rule from isolated service tests.

Confirm that `PlanDoc`, the nine `Run` state objects, and approval carries are
still used as implemented; this change does not rebuild their responsibilities.

## Validation and completion

For each migration, run the affected unit/e2e suites before proceeding. At the
integration points run:

- `cargo test -p rovik-core`, plus client, daemon and headless crate tests for
  changed callers; also run the changed crates through cargo-nextest.
- `cargo clippy -p <changed-crate> --tests` with no additional warnings. Format
  only changed Rust files, following the repository's `AGENTS.md`.
- `npm test`, `npm run typecheck`, and `npm run format:check`; desktop host changes
  also need Linux/macOS/Windows CI builds, including real mount/sandbox smoke tests
  where the platform provides them.
- Explicit startup-failure, shutdown-order, two-context, stale-callback, and
  cross-process slot/history tests. Use test clocks/cancellation signals for
  delays; do not wait through product shutdown deadlines.

The prior refactoring's local baseline on `ef60d57` had 146 failing tests because
this environment could not provide the command sandbox; its changed code had the
same failure names. That is historical evidence, not a permanent allowlist. For
this work, establish a fresh base/head comparison and run the relevant suites on
a host that supports the sandbox and mounts. Do not skip tests or weaken security
to obtain green results.

Done means all production hosts use one explicit lifecycle; rejected initialization
cannot mutate an existing runtime; cleanup is ordered, bounded, repeatable and
covers errors; migrated registries have no mutable global fallback; owned workers
cannot outlive their service teardown; and two isolated test cores with identical
chat IDs operate and shut down independently in one process. Existing state
extractions, persisted data and protocol behavior remain compatible.

## Progress

- [ ] 1. Lifecycle/admission state and initialization-refusal tests
- [ ] 2. Host construction, failure cleanup and shared shutdown coordination
- [ ] 3. Owned workload state and sampler
- [ ] 4. Owned chat holds, overlay registry and workers
- [ ] 5. Owned background commands and connector sessions
- [ ] 6. Isolated test services and removal of production construction bypasses
- [ ] 7. Global-access audit, integration validation and documentation
