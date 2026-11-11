#!/usr/bin/env bash
# Install Ubuntu CI dependencies without the runner's Azure mirror indirection.
set -euo pipefail

suite=${1:?Pass the Ubuntu release codename, then the packages to install}
shift
if [[ ! $suite =~ ^[a-z]+$ || $# == 0 ]]; then
  echo 'Expected an Ubuntu release codename and at least one package.' >&2
  exit 2
fi

# Fail over before APT's per-index retries if Ubuntu's DNS is unavailable.
# Every source still requires Ubuntu's archive signing key below.
reachable() {
  curl --fail --silent --show-error --location --connect-timeout 5 --max-time 15 \
    --retry 1 --output /dev/null "$1/dists/$2/InRelease"
}
archive=https://archive.ubuntu.com/ubuntu
security=https://security.ubuntu.com/ubuntu
if ! reachable "$archive" "$suite" || ! reachable "$security" "$suite-security"; then
  echo 'Ubuntu repositories unavailable; trying the kernel.org Ubuntu mirror.' >&2
  archive=https://mirrors.edge.kernel.org/ubuntu
  security=$archive
  reachable "$archive" "$suite"
  reachable "$security" "$suite-security"
fi

sources_dir=$(mktemp -d)
trap 'rm -rf "$sources_dir"' EXIT
sources=$sources_dir/ubuntu.list
mkdir "$sources_dir/parts"
cat > "$sources" <<SOURCES
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] $archive $suite main restricted universe multiverse
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] $archive $suite-updates main restricted universe multiverse
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] $archive $suite-backports main restricted universe multiverse
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] $security $suite-security main restricted universe multiverse
SOURCES

# Use the same sources for update and install, and refuse stale package indexes.
options=(-o "Dir::Etc::sourcelist=$sources" -o "Dir::Etc::sourceparts=$sources_dir/parts"
  -o Acquire::Retries=3 -o Acquire::https::Timeout=30)
apt-get "${options[@]}" -o APT::Update::Error-Mode=any update
apt-get "${options[@]}" install -y "$@"
