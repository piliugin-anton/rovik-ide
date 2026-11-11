#!/usr/bin/env bash
# Install Ubuntu CI dependencies without the runner's Azure mirror indirection.
set -euo pipefail

suite=${1:?Pass the Ubuntu release codename, then the packages to install}
shift
if [[ ! $suite =~ ^[a-z]+$ || $# == 0 ]]; then
  echo 'Expected an Ubuntu release codename and at least one package.' >&2
  exit 2
fi

sources=$(mktemp --suffix=.list)
trap 'rm -f "$sources"' EXIT
cat > "$sources" <<SOURCES
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu $suite main restricted universe multiverse
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu $suite-updates main restricted universe multiverse
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu $suite-backports main restricted universe multiverse
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://security.ubuntu.com/ubuntu $suite-security main restricted universe multiverse
SOURCES

# Use the same sources for update and install, and refuse stale package indexes.
options=(-o "Dir::Etc::sourcelist=$sources" -o 'Dir::Etc::sourceparts=-'
  -o Acquire::Retries=3 -o Acquire::https::Timeout=30)
apt-get "${options[@]}" -o APT::Update::Error-Mode=any update
apt-get "${options[@]}" install -y "$@"
