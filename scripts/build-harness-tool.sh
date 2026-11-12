#!/usr/bin/env bash
# Build outside verification; the pinned image owns this executable at runtime.
set -euo pipefail
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$repo_dir/src-tauri/target}"
target_dir="$(mkdir -p -- "$target_dir" && cd -- "$target_dir" && pwd)"
CARGO_TARGET_DIR="$target_dir" RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
  cargo build --locked --release --target x86_64-unknown-linux-gnu \
  --manifest-path "$repo_dir/src-tauri/Cargo.toml" -p rovik-harness --bin rovik-harness-tool
install -m 0755 "$target_dir/x86_64-unknown-linux-gnu/release/rovik-harness-tool" \
  "$repo_dir/src-tauri/crates/rovik-harness/fixtures/native/rovik-harness-tool"
