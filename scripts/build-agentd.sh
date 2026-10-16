#!/usr/bin/env bash
# Static builds of the daemon, rovik-agentd, for Linux servers (B01): one
# per CPU, each a single file that runs on any distribution, old or new,
# since it brings its own C library (musl). Any computer that has Rust can
# make them, with zig and cargo-zigbuild:
#
#   pip install cargo-zigbuild        # brings zig too
#   scripts/build-agentd.sh           # x86_64 and aarch64
#   scripts/build-agentd.sh aarch64   # or only the CPUs named
#
# Each build is written to src-tauri/target/agentd/ as
# rovik-agentd-<version>-<cpu>, the name a release gives it, and its path
# is printed. ROVIK_AGENTD_BINARY can name one for a deploy.
set -euo pipefail

cd "$(dirname "$0")/../src-tauri"

if ! command -v cargo-zigbuild >/dev/null 2>&1; then
  echo "build-agentd: cargo-zigbuild is missing: pip install cargo-zigbuild" >&2
  exit 1
fi

# `path+file:///…/rovik-agentd#rovik-agentd@0.1.0`, or `…#0.1.0`.
id=$(cargo pkgid -p rovik-agentd)
version=${id##*[#@]}

cpus=("$@")
if [ ${#cpus[@]} -eq 0 ]; then
  cpus=(x86_64 aarch64)
fi

out=target/agentd
mkdir -p "$out"
for cpu in "${cpus[@]}"; do
  case "$cpu" in
    x86_64 | aarch64) ;;
    *)
      echo "build-agentd: $cpu isn't a CPU the daemon is built for: x86_64 or aarch64" >&2
      exit 2
      ;;
  esac
  target="$cpu-unknown-linux-musl"
  if command -v rustup >/dev/null 2>&1; then
    rustup target add "$target" >/dev/null
  fi
  cargo zigbuild --locked --profile release-daemon -p rovik-agentd --target "$target" >&2
  file="$out/rovik-agentd-$version-$cpu"
  cp "target/$target/release-daemon/rovik-agentd" "$file"
  echo "$PWD/$file"
done
