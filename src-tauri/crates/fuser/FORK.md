# fuser message transport

Based on the published `fuser` 0.18.0 crate (MIT, see LICENSE.md), copied from
crates.io. The workspace patches that version to this directory. It is excluded
from workspace membership so upstream examples and their development dependencies
do not affect application builds.

The implementation changes are in `src/channel.rs`, `src/session.rs`,
`src/lib.rs` and the manifest. `CHANGELOG.md` records the added feature.
Trailing whitespace in those files and `deny.toml` has been removed. `custom-transport` adds `Transport` and
`Session::from_transport` for macFUSE FSKit channels, which cannot provide a device
file descriptor. Replies and notifications use the same transport as requests.
The caller owns mount/unmount and must close the channel to wake blocked reads.
Transport errors use OS error codes, preserving fuser's retry and ENODEV handling.

The feature is enabled only for Rovik's macOS build. Without it the existing
`AsFd` API and device path remain available. With it, `Session::try_as_fd` replaces
`AsFd` and returns EOPNOTSUPP for message channels. Device cloning and passthrough
backing registration are unsupported for message channels; wrapping a raw backing
ID still requires a device channel, as does the underlying Linux facility.

Tests (a Linux host needs access to /dev/fuse and fusermount3):

```sh
cargo test --manifest-path src-tauri/crates/fuser/Cargo.toml --lib
cargo test --manifest-path src-tauri/crates/fuser/Cargo.toml --lib --features custom-transport
cargo clippy --manifest-path src-tauri/crates/fuser/Cargo.toml --tests --features custom-transport
```

On macOS also enable `macos-no-mount,macfuse-4-compat`, as the mounted CI workflow
does. Its separate core tests cover the MFMount adapter, followed by an actual
FSKit busy-mount and saved-edit regression.

Unused private ABI structs for unsupported CUSE, ioctl retry vectors and retrieve
notifications are omitted; supported request/reply layouts are unchanged.

Remove this fork when upstream exposes a message transport that covers requests,
replies, notifications, initialization and shutdown. Move the regression tests to
that API, restore the registry dependency, and remove the workspace patch.
