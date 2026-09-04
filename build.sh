#!/usr/bin/env bash
# Builds a release bundle. Output lands in src-tauri/target/release/bundle/.
# Run ./setup.sh first if you haven't already.
set -euo pipefail

cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi

command -v cargo >/dev/null 2>&1 || { echo "error: cargo not found — run ./setup.sh first." >&2; exit 1; }
[ -d node_modules ] || { echo "error: node_modules missing — run ./setup.sh first." >&2; exit 1; }

npm run tauri build

echo
echo "Build artifacts:"
find src-tauri/target/release/bundle -maxdepth 2 -type f 2>/dev/null
