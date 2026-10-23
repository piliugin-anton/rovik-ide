# Fix the five review findings in the shared-toolchains implementation

These are five fixes to the shared-toolchains implementation, found when reviewing plan 2026-10-23.

1. Crate entries read from `Cargo.lock` are checked before they become file names.
2. A verify round that failed on its environment now reports the toolchain installs it attempted. It reruns the checks when one succeeded.
3. A refused install is remembered for a while instead of running `sudo` again every round.
4. The core tests that set the process-wide toolchain state take one shared test lock.
5. The `serve.rs` permission test skips its non-writable case when run as root.

## Steps

1. **Check crate names and versions before they become paths**
   - Files: `src-tauri/crates/rovik-core/src/toolchains.rs`
   - Add `fn crate_id_ok(name: &str, version: &str) -> bool`.
     - The name must be non-empty `[A-Za-z0-9_-]+`, as crates.io allows.
     - The version must be non-empty `[A-Za-z0-9.+-]+`, start with a digit, and contain no `..`.
   - `locked_crates` skips any entry that fails it, and logs it once with `::log::warn!`.
   - Every caller of `crate_file`, plus the destination built in `seed_crates` and `publish_crates`, uses only entries from `locked_crates`. So this one filter covers all three; no other function changes.
   - The same function reads the checksum. It must be exactly 64 hex characters, or the entry is skipped.

2. **Report the toolchain installs in a verify round that failed on its environment**
   - Files: `src-tauri/crates/rovik-core/src/agent_run/prepare.rs`
   - In `Run::environment_round`, keep the outcome of `find_programs(root, &installs)` as `toolchains` (it is `_toolchains` today).
   - The record's `environment` object gains `"toolchains": [{ "toolchain": want.describe(), "error": <message or null> }]`.
     - The `meta` closure takes the toolchain list as a second argument. The first `update_meta` (before `find_programs`) passes an empty list.
     - Right after `let toolchains = find_programs(root, &installs).await;`, call `self.update_meta(record_id, meta(&done, &toolchains)).await?` once. That way a round with no installs (nothing `needed`, or Prepare off) still records the outcome.
     - Every later `meta(...)` call in the install loop passes `&toolchains`.
   - Add `fn environment_outcome(done: &[InstallResult], toolchains: &[(Want, Result<(), String>)]) -> bool`. It returns true ("repaired") when any install ran or any toolchain installed. A newly installed toolchain makes the checks run again, like a finished install does.
   - Add `fn environment_note(failure: &str, toolchains: &[(Want, Result<(), String>)]) -> String` for the "continue" case.
     - With toolchain refusals, it starts with `toolchains_line(toolchains)`, then says the version the project needs couldn't be installed and that an administrator can add it.
     - Without any, it is today's text, unchanged.
   - `environment_round` uses both helpers in place of its inline `if !done.is_empty()` and `format!`.

3. **Remember refused installs instead of retrying every round**
   - Files: `src-tauri/crates/rovik-core/src/toolchains.rs`
   - Add these private items:
     - `static REFUSED: Mutex<Option<HashMap<String, (Instant, String)>>>`, keyed by `want.words().join(" ")`;
     - `const RETRY_AFTER: Duration = Duration::from_secs(15 * 60)`.
   - In `ensure`:
     - Before calling the installer, a key refused less than `RETRY_AFTER` ago returns its remembered `Err` without running it.
     - After a call, an `Err` is stored with `Instant::now()`, and an `Ok` removes the key.
   - `set` and `unset` clear `REFUSED`, so a daemon's settings change (or a test) starts fresh.
   - Callers don't change. `find_programs` (prepare.rs) and `toolchain_note` (tools.rs) still get the error text, now without another `sudo` run.

4. **One lock for the core tests that set the toolchain state**
   - Files: `src-tauri/crates/rovik-core/src/toolchains.rs`, `src-tauri/crates/rovik-core/src/command_tool.rs`, `src-tauri/crates/rovik-core/src/agent_run/prepare.rs`, `src-tauri/crates/rovik-core/src/agent_run/tools.rs`
   - In `toolchains.rs`, add a `#[cfg(test)] pub(crate) struct TestHost`.
     - It holds a guard of `static TEST_LOCK: Mutex<()>`, taken poison-tolerantly (`unwrap_or_else(|e| e.into_inner())`).
     - It saves `CARGO_HOME` and calls `unset()`.
     - `TestHost::set(&self, shared: Shared)` sets the state.
     - `Drop` calls `unset()` and puts `CARGO_HOME` back.
     - It replaces `tests::Guard` in `toolchains.rs`. The four tests there that use `Guard` take a `TestHost` instead, and `only_the_pinned_crates_are_copied_into_the_cache` sets `CARGO_HOME` only while holding it.
   - The other test sites change in their own files:
     - `command_tool.rs` `a_cloud_hosts_toolchains_are_in_every_commands_environment` creates a `TestHost` first and uses `host.set(…)`, removing its bare `set`/`unset` calls.
     - `prepare.rs` `tests::Host` becomes a thin wrapper holding a `TestHost`, which also keeps the temporary folder, so the `std::mem::forget(dir)` goes.
     - `tools.rs` `a_missing_toolchain_program_is_installed_for_the_project` does the same as `command_tool.rs`. It gets `#[allow(clippy::await_holding_lock)]`, because the guard lives across `.await` in a current-thread `#[tokio::test]`.
       - While converting it, fix its refused phase: remove `dir.0.join("toolchains/rust/cargo/bin").join(program)`, the file the installer wrote, instead of `dir.0.join("rust/cargo/bin").join(program)` (tools.rs:2604), which doesn't exist and makes the `unwrap` panic.

5. **The `serve.rs` permission test as root**
   - Files: `src-tauri/crates/rovik-agentd/src/serve.rs`
   - In `tests::the_shared_caches_are_used_only_where_this_daemon_may_write_them`, run the last block (folders at mode 0500, then expect `caches == None`) only when `unsafe { libc::geteuid() } != 0`.
   - Its comment then says what the code does: root can write anywhere, so that case only holds for other users.

## Decisions

- Validation lives in `locked_crates`, so the parser never returns anything unsafe to join into a path. Checking at each `join` would repeat the check three times.
- A refused install is remembered in memory for 15 minutes. That stops the `sudo` run on every round, and still picks up an administrator's fix (sudoers, `user_install`, a new allowed version) without restarting the daemon. Changed settings take effect at the daemon's next start anyway, and `set` clears the memory.
- The toolchain outcomes go into the verify record's metadata and into the agent's note. The verification record UI (`src/components/VerificationRecord.tsx`) is unchanged, because it only shows `environment.installs`. Showing toolchains there would be a separate UI change.
- A toolchain installed in an environment round counts as a repair, so the checks run again at once, as they do after an install.
- The test lock is a plain `std::sync::Mutex<()>` in `toolchains.rs` under `#[cfg(test)]`, not in `crate::testing`. Only this crate's tests set `SHARED`, and `rovik-agentd`'s tests never do.

## Validation

- Run `cd src-tauri && cargo test -p rovik-core -p rovik-agentd`, then `cargo clippy --workspace --all-targets`.
- New and updated tests:
  - `toolchains.rs`:
    - `locked_crates` drops entries named `../../x` and `a/b`, versions `1.0/../x`, and a 63-character checksum, and keeps a normal one.
    - `seed_crates` with such a lockfile writes nothing outside the cache folder.
  - `toolchains.rs`: `ensure` with an installer that counts its calls and returns `Err`:
    - a second `ensure` doesn't call it and returns the same error;
    - after `unset` plus `set`, it calls it again;
    - an `Ok` after an `Err` clears the memory;
    - after an `Err` is remembered, `set` with a new installer that returns `Ok` calls it on the next `ensure`, which returns `Ok`.
  - `prepare.rs`:
    - `environment_outcome` is true for a successful toolchain with no installs, false for refusals only, and true when an install ran;
    - `environment_note` with a refusal starts with "Toolchains:" and names the version;
    - without one, it equals today's text;
    - when the project needed a toolchain and has no installs, the round's record has `environment.toolchains`. Test this through `environment_round` with the existing `TestCore` run fixtures, or through the `meta` value that function builds.
  - The existing tests that set the shared state now use `TestHost`. Run `cargo test -p rovik-core --lib -- --test-threads=16` a few times to show they no longer interfere.
  - `serve.rs`: the test passes as a normal user and as root (`sudo -E cargo test -p rovik-agentd --bin rovik-agentd the_shared_caches`).

## Risks

- Other core tests that run commands (any `run_blocking`) don't take the test lock. While a toolchain test holds a host, they get `RUSTUP_HOME` and a `PATH` prefix pointing into that test's temporary folder. None of them runs cargo or rustup, so it's harmless today. A future test that does would need the lock too.
- Crate names are kept to crates.io's character set. A registry crate with other characters would only miss the shared cache and download as usual.
- This sandbox currently has no `cargo` or `git`, so the plan was checked by reading the code. The commands above must be run on a machine that has them.
