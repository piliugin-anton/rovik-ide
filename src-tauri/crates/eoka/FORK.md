# Rovik's fork of eoka

This is [eoka](https://github.com/shrimp-software/eoka) 0.7.2, MIT
(`LICENSE`), as published on crates.io, with a pipe transport and a hook on
Chrome's command added, and Chrome's reason kept when a navigation fails.
The workspace uses it in place of the crates.io release through
`[patch.crates-io]` in `src-tauri/Cargo.toml`.

## Why

eoka starts Chrome with `--remote-debugging-port`, so DevTools listens on
a `127.0.0.1` port with no password. On a server several people share,
any of them can connect to that port and drive a browser that runs as
you, and read your files through it (docs/design/cloud-agent-dev-plan.md,
D18 and K52). With `StealthConfig::pipe`, eoka starts Chrome with
`--remote-debugging-pipe` instead: CDP travels over two pipes that only
this process holds, and no port is opened. Rovik's browser tools use it
on Linux and macOS (`rovik-core/src/browser_tools.rs`).

A page Chrome couldn't open used to fail with "Navigation failed" alone; it
now says Chrome's reason (`net::ERR_CONNECTION_REFUSED`), which tells the
agent what went wrong.

On a team server each user's commands run in a network of their own (D20),
where the dev servers the agent starts listen. `StealthConfig::command_hook`
lets Rovik start the agent's Chrome in that network too (`Network::join`, a
`pre_exec`), so `localhost` in the browser is the user's own, as in their
commands.

## What differs from 0.7.2

- `src/cdp/transport/pipe.rs` (new): the pipes, Chrome's fds 3 and 4,
  and NUL-delimited message framing.
- `src/cdp/transport/link.rs` (new): the transport's writer, reader and
  shutdown over either the WebSocket or the pipe.
- `src/cdp/transport.rs`, `reader.rs`, `process.rs`: the transport takes
  either link; `launch_chrome_with_pipe`.
- `src/browser.rs`, `src/browser/launch.rs`: `StealthConfig::pipe`,
  readiness from the first `Browser.getVersion` reply, and
  `Browser::process_id`. With `pipe`, a launch fails rather than open a
  port: one with `--remote-debugging-port` in `extra_args`, and one on a
  profile a running Chrome holds, which it would otherwise attach to over
  that Chrome's port.
- `src/browser/launch.rs`: an explicit `/snap/bin/<app>` path is kept as
  given. Resolving it gives `/usr/bin/snap`, which starts the app only
  under the link's name.
- `src/stealth/patcher.rs`: on macOS the patched copy is signed ad hoc
  without Chrome's entitlements and hardened-runtime flags, as eoka did
  before 0.7.0. Google Chrome's entitlements include restricted ones that
  macOS honours only under Google's signature, so it killed the copy as it
  started (SIGKILL, nothing on stderr).
- `src/lib.rs`: the `pipe` field.
- `src/action/perform.rs`: a navigation Chrome can't complete fails with
  Chrome's `errorText` (`Error::Navigation("net::ERR_CONNECTION_REFUSED")`),
  as `check_nav_result` gives it, not with "Navigation failed".
- `src/lib.rs`, `src/browser/launch.rs`, `src/cdp/transport/process.rs`:
  `StealthConfig::command_hook` and `CommandHook`, which prepare the command
  that starts Chrome, over a pipe or a port, before it's spawned.
- Tests: the transport tests run over both links; `tests/pipe.rs` checks
  a real Chrome over the pipe, and the hook with a stand-in for Chrome;
  `tests/actions.rs` checks a refused navigation's reason with a real Chrome.
- `CHANGELOG.md` and `README.md` describe the pipe, the navigation's reason
  and the hook; `CLAUDE.md` and `AGENTS.md` name the pipe in the
  architecture and `CommandHook` in the exported types.

Dropped from the published package: `Cargo.toml` as normalized by
`cargo publish` (`Cargo.toml.orig` is now `Cargo.toml`), `Cargo.lock`
(the workspace's lock file applies) and `.cargo_vcs_info.json`.

## Updating and dropping the fork

The changes are meant to go upstream. The commit that vendored 0.7.2 is the
base: `git diff <that commit> --relative=src-tauri/crates/eoka -- . ':!FORK.md'`,
run in `src-tauri/crates/eoka`, is a patch for eoka's v0.7.2. When an eoka
release has a pipe transport, the navigation's reason and a hook on
Chrome's command, set `rovik-core`'s `eoka` to that release, remove the
`[patch.crates-io]` entry and `crates/eoka` from the workspace members,
delete this folder, and adapt `browser_tools.rs` to the release's options
if their names differ.

Until then, to move to a newer eoka: unpack the new release over this
folder (keeping this file), redo the changes listed above, and run
`cargo test -p eoka` and the ignored browser tests
(`cargo test -p eoka --test pipe --test actions -- --ignored`) with Chrome
installed.
