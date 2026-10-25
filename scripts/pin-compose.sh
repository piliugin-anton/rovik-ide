#!/usr/bin/env bash
# The builds of a Docker Compose release, as rovik-core's pinned_compose.rs
# pins them (D21): each Linux program downloaded from the release, checked
# against the release's checksums.txt, and its SHA-256 printed as the
# `BUILDS` entries want it. Set `VERSION` there to the version, and replace
# each build's `sha256` with what this prints.
#
#   scripts/pin-compose.sh 5.6.1
#
# Before pinning, start a project's services with it against the Podman of
# the oldest distribution docs/cloud-hosts.md supports.
set -euo pipefail

version=${1:?usage: scripts/pin-compose.sh VERSION}
release="https://github.com/docker/compose/releases/download/v$version"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

curl -sS --fail --retry 3 -L -o "$scratch/checksums.txt" "$release/checksums.txt"
for cpu in x86_64 aarch64; do
  asset="docker-compose-linux-$cpu"
  curl -sS --fail --retry 3 -L -o "$scratch/$asset" "$release/$asset"
  sha256=$(sha256sum "$scratch/$asset" 2>/dev/null || shasum -a 256 "$scratch/$asset")
  sha256=${sha256%% *}
  published=$(awk -v asset="*$asset" '$2 == asset { print $1 }' "$scratch/checksums.txt")
  if [ "$sha256" != "$published" ]; then
    echo "$asset hashes to $sha256, but checksums.txt says $published" >&2
    exit 1
  fi
  printf '%-8s sha256: "%s",\n' "$cpu" "$sha256"
  rm -f "$scratch/$asset"
done
