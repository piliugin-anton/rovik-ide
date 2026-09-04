#!/usr/bin/env bash
# One-time setup: system packages Tauri needs to build/run + a Rust
# toolchain (via rustup, not your distro's package — distro rustc is
# usually too old for current Tauri) + npm dependencies.
#
# Usage: ./setup.sh
set -euo pipefail

MIN_RUST_MINOR=77 # Tauri 2's current MSRV is 1.77.x-ish; rustup will keep this current anyway.

log() { printf '\n\033[1;34m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

# --- 1. System packages -----------------------------------------------------

install_system_deps() {
  if command -v apt-get >/dev/null 2>&1; then
    log "Installing system packages via apt (needs sudo)…"
    sudo apt-get update -qq
    sudo apt-get install -y -qq \
      build-essential curl wget file \
      libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev \
      libgtk-3-dev librsvg2-dev \
      libayatana-appindicator3-dev \
      libssl-dev libsoup-3.0-dev libxdo-dev \
      pkg-config

  elif command -v dnf >/dev/null 2>&1; then
    log "Installing system packages via dnf (needs sudo)…"
    sudo dnf install -y \
      gcc gcc-c++ make curl wget file \
      webkit2gtk4.1-devel gtk3-devel librsvg2-devel \
      libappindicator-gtk3-devel \
      openssl-devel libsoup3-devel \
      NetworkManager-libnm-devel

  elif command -v pacman >/dev/null 2>&1; then
    log "Installing system packages via pacman (needs sudo)…"
    sudo pacman -Sy --needed --noconfirm \
      base-devel curl wget file \
      webkit2gtk-4.1 gtk3 librsvg \
      libappindicator-gtk3 \
      openssl

  else
    warn "Unrecognized package manager — skipping system package install."
    warn "Install Tauri's Linux prerequisites manually:"
    warn "  https://v2.tauri.app/start/prerequisites/#linux"
  fi
}

# --- 2. Rust toolchain -------------------------------------------------------

rust_is_new_enough() {
  command -v rustc >/dev/null 2>&1 || return 1
  local ver minor
  ver="$(rustc --version | awk '{print $2}')" # e.g. 1.82.0
  minor="$(echo "$ver" | cut -d. -f2)"
  [ "$minor" -ge "$MIN_RUST_MINOR" ]
}

install_rust() {
  if rust_is_new_enough; then
    log "Rust toolchain OK ($(rustc --version))."
    return
  fi

  if command -v rustc >/dev/null 2>&1; then
    warn "Found rustc ($(rustc --version)) but it's older than needed."
  fi

  if [ -x "$HOME/.cargo/bin/rustc" ]; then
    log "rustup-managed toolchain exists, updating…"
    "$HOME/.cargo/bin/rustup" update stable
  else
    log "Installing Rust via rustup…"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  fi

  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"

  rust_is_new_enough || die "Rust toolchain still too old after install — check $HOME/.cargo/bin is on your PATH."
  log "Rust toolchain ready ($(rustc --version))."
}

# --- 3. Node deps -------------------------------------------------------------

install_node_deps() {
  command -v node >/dev/null 2>&1 || die "Node.js not found. Install Node 18+ (https://nodejs.org) and re-run."
  command -v npm >/dev/null 2>&1 || die "npm not found alongside node — check your Node install."
  log "Node $(node --version), npm $(npm --version)."

  log "Installing npm dependencies…"
  npm install
}

# --- main ---------------------------------------------------------------------

cd "$(dirname "$0")"

install_system_deps
install_rust
install_node_deps

log "Setup done. Run ./dev.sh to start the app, or ./build.sh for a release bundle."
