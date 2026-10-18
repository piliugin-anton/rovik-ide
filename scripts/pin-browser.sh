#!/usr/bin/env bash
# The builds of a Chrome for Testing version, as rovik-core's
# pinned_browser.rs pins them (D19): each platform's archive downloaded
# from Google, and its size and SHA-256 printed as the `BUILDS` entries
# want them. Set `VERSION` there to the version, and replace each build's
# `size` and `sha256` with what this prints.
#
#   scripts/pin-browser.sh 155.0.8059.39
#
# Pick a version eoka's CI runs (crates/eoka/CLAUDE.md) with builds for
# every platform, and run eoka's browser suite against it before pinning.
set -euo pipefail

version=${1:?usage: scripts/pin-browser.sh VERSION}
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

for platform in linux64 linux-arm64 mac-arm64 mac-x64 win64; do
  url="https://storage.googleapis.com/chrome-for-testing-public/$version/$platform/chrome-$platform.zip"
  curl -sS --fail --retry 3 -o "$scratch/archive.zip" "$url"
  size=$(wc -c <"$scratch/archive.zip" | tr -d ' ')
  sha256=$(sha256sum "$scratch/archive.zip" 2>/dev/null || shasum -a 256 "$scratch/archive.zip")
  printf '%-12s size: %s,\n             sha256: "%s",\n' \
    "$platform" "$(echo "$size" | sed -E ':a;s/^([0-9]+)([0-9]{3})/\1_\2/;ta')" "${sha256%% *}"
  rm -f "$scratch/archive.zip"
done
