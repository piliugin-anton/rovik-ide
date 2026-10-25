# Rovik's fork of russh

This is [russh](https://github.com/Eugeny/russh) 0.64.1, Apache-2.0
(`LICENSE`), as published on crates.io, with two fixes to how the channels
of a connection share it: a channel that isn't read now holds up only
itself. The workspace uses it in place of the crates.io release through
`[patch.crates-io]` in `src-tauri/Cargo.toml`.

## Why

The app reaches a cloud host over one SSH connection, with a channel for
each stream on it: the daemon's WebSocket, each upload, each connection
to a forwarded port (`rovik-client/src/ssh.rs`). In russh 0.64.1, one
channel nobody reads stops the others:

- Coming in, on a client: the loop that reads the connection gives each
  channel's messages to a queue of `channel_buffer_size` (100) messages,
  and when that is full it waits there for room. While one channel went
  unread (a page's connection the browser keeps open without reading on
  it), no other channel got its data and no new channel opened, until the
  unread one was read. Its window opened as its data came, not as it was
  read, so nothing told the host to stop.
- Going out, on a client and a server: when the peer opened a channel's
  window, russh set what the channel's writers may take to what was left
  of it, which handed out again what they had taken and not yet sent.
  What they then wrote went past the window and waited, and while it
  waited, the session took nothing more to send on any channel
  ([#789](https://github.com/Eugeny/russh/issues/789)). With the first
  fix a window really closes on a channel nobody reads, which made a
  russh server stop like that.

Both are fixed as OpenSSH works: a channel's window opens as its reader
takes its data, so what isn't read yet waits on the sender and holds up
only its own channel, and the session's loop never waits for a reader.

## What differs from 0.64.1

Coming in, on a client:

- `src/channels/channel_ref.rs`: `ChannelRef::deliver` gives a message to
  the channel's queue without waiting: what it has no room for waits in
  the `ChannelRef`, in order, until the reader takes from the queue
  (`flush`). `offer`, for `WindowAdjusted`, which only informs: never
  ahead of what waits. `hand_over`: what still waits when the session lets
  the channel go (it closed, or the connection ended) goes to the reader
  on a task of its own.
- `src/channels/mod.rs`, `src/channels/io/rx.rs`: a client channel's
  reader (`ChannelReadHalf::taken`) tells the session each message it
  takes, with `wait` or as a stream, and its data bytes; when the reader
  is dropped, what its queue still held counts as taken.
- `src/client/mod.rs`, `src/client/encrypted.rs`: the session delivers
  each channel's messages rather than waiting for room. A channel's data
  makes its window smaller (`Session::received`); what its reader takes
  lets in what waits, and opens the window by it (`Session::taken` and
  `Session::read`): once less than half of `Config::window_size` is left,
  never past it. `Handler::adjust_window` is called then, as before. Data
  whose reader is gone is dropped and opens the window as if read, as
  before.
- `src/session.rs`, `src/lib_inner.rs`: the window's count,
  `Encrypted::received` and `Encrypted::taken`, and `ChannelParams::taken`.

Going out, on a client and a server:

- `src/channels/mod.rs`: `WindowSizeRef::open` adds what the peer opened
  to what writers may take, never past what the session has left to send
  in; `src/client/encrypted.rs` and `src/server/encrypted.rs` call it on
  CHANNEL_WINDOW_ADJUST. A channel written both through its
  `ChannelWriteHalf` and through `Handle::data` can still go past the
  window, as before.

Not changed: a server's incoming channel data, whose window still opens
as it comes, and which still waits for room in a full queue.

Tests: unit tests beside the changes (`channel_ref.rs`,
`channels/mod.rs`, `session.rs`), and
`an_ssh_channel_the_app_doesnt_read_holds_up_none_of_the_others` in
`crates/rovik-agentd/tests/client.rs`, over a connection to a russh
server.

Dropped from the published package: `Cargo.toml.orig` (the normalized
`Cargo.toml` is kept), `Cargo.lock` and `.cargo_vcs_info.json` (upstream
commit e5d80a07dc55). Added: `LICENSE`, the Apache-2.0 text the package
names but doesn't carry, and `.gitignore`.

## Outside the workspace

`src-tauri/Cargo.toml` excludes it from the workspace's members: it's
built as the dependency it is, so its tests and examples, and what they
need (ratatui, clap, russh-sftp, termion), stay out of the workspace's
builds, and CI doesn't run them. Run them when this folder changes:

```sh
cd src-tauri/crates/russh
cargo test --lib --tests
```

That makes a `Cargo.lock` and `target` here, which Git ignores.
`tests/auth_agent_req_channel_reply.rs` compares russh with the system's
`sshd`, and fails where that can't start, as it does for 0.64.1.

## Updating and dropping the fork

The fixes are meant to go upstream. The commit that vendored 0.64.1 is the
base: `git diff <that commit> --relative=src-tauri/crates/russh -- . ':!FORK.md' ':!.gitignore'`,
run in `src-tauri/crates/russh`, is a patch for russh's v0.64.1. When a
russh release has both fixes, set `russh` in `rovik-client` (and in
`rovik-agentd`'s dev-dependencies) to it, remove its `[patch.crates-io]`
entry and the `exclude`, and delete this folder;
`an_ssh_channel_the_app_doesnt_read_holds_up_none_of_the_others` says
whether a release has them.

Until then, to move to a newer russh: unpack the new release over this
folder (keeping this file, `LICENSE` and `.gitignore`), redo the changes
listed above, and run the tests above and
`cargo nextest run -p rovik-client -p rovik-agentd`.
