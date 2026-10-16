#!/usr/bin/env bash
# Sets Rovik's version everywhere it's written down:
#
# - src-tauri/Cargo.toml's [workspace.package], which every crate takes,
#   and the app's bundle too, as tauri.conf.json names no version;
# - src-tauri/Cargo.lock;
# - package.json and package-lock.json.
#
#   scripts/set-version.sh 1.2.3
#
# The release workflow runs it before it commits and tags the release. A
# Windows installer takes only numbers, so a pre-release is 1.2.3-4, not
# 1.2.3-beta.
set -euo pipefail

version=${1:-}
if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9]+)?$ ]]; then
  echo "usage: scripts/set-version.sh MAJOR.MINOR.PATCH[-N]" >&2
  echo "set-version: '$version' isn't a version a Windows installer takes" >&2
  exit 2
fi

cd "$(dirname "$0")/.."

manifest=src-tauri/Cargo.toml
sed -i.bak "/^\[workspace\.package\]/,/^\[/ s/^version = \".*\"/version = \"$version\"/" "$manifest"
rm -f "$manifest.bak"
if ! sed -n '/^\[workspace\.package\]/,/^\[/p' "$manifest" | grep -qx "version = \"$version\""; then
  echo "set-version: $manifest has no version under [workspace.package]" >&2
  exit 1
fi

# The lock file's entries for the workspace's own crates, nothing else.
(cd src-tauri && cargo update --workspace --quiet)

npm version "$version" --no-git-tag-version --allow-same-version >/dev/null

echo "Rovik's version is $version."
