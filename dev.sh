#!/usr/bin/env bash
# Runs the app in dev mode (hot-reloading frontend, opens a native window).
# Run ./setup.sh first if you haven't already.
set -euo pipefail

cd "$(dirname "$0")"

# Pick up a rustup-installed toolchain if it's not already on PATH.
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

command -v cargo >/dev/null 2>&1 || { echo "error: cargo not found — run ./setup.sh first." >&2; exit 1; }
[ -d node_modules ] || { echo "error: node_modules missing — run ./setup.sh first." >&2; exit 1; }

exec npm run tauri dev
