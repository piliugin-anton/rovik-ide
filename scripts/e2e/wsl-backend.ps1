#requires -Version 7.3
# Strict acceptance on a Windows machine with a provisioned WSL2 distribution.
# Uses an existing Linux checkout at the same revision; installs no packages.
param(
  [Parameter(Mandatory = $true)][string]$Distro,
  [Parameter(Mandatory = $true)][string]$LinuxCheckout
)
$ErrorActionPreference = 'Stop'
$revision = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Run from the Windows repository checkout' }
$root = '/tmp/rovik-wsl-acceptance-' + [guid]::NewGuid().ToString('N')
$setup = @'
set -eu
repo=$1
revision=$2
root=$3
case "$(uname -r)" in *WSL2*|*wsl2*) ;; *) echo 'WSL2 is required' >&2; exit 1;; esac
test -c /dev/fuse
command -v fusermount3
command -v bwrap
cd "$repo"
test "$(git rev-parse HEAD)" = "$revision"
test -z "$(git status --porcelain)"
export ROVIK_MOUNTED_BACKEND=1
cargo test --locked --manifest-path src-tauri/Cargo.toml -p rovik-core --lib mounted_child_views_drain_without_losing_their_dependency_tree -- --ignored
cargo test --locked --manifest-path src-tauri/Cargo.toml -p rovik-client --test mounted_backend -- --ignored
cargo test --locked --manifest-path src-tauri/Cargo.toml -p rovik-agentd --test remote
cargo build --locked --manifest-path src-tauri/Cargo.toml -p rovik-agentd
umask 077
mkdir -p "$root/data"
hash=$(printf %s wsl-acceptance-fixture | sha256sum | cut -d ' ' -f 1)
printf '[tokens]\nwsl-acceptance = "%s"\n' "$hash" > "$root/data/config.toml"
nohup "$repo/src-tauri/target/debug/rovik-agentd" serve --data "$root/data" --socket "$root/agentd.sock" > "$root/daemon.log" 2>&1 < /dev/null &
echo $! > "$root/pid"
'@
try {
  $setup | & wsl.exe -d $Distro --exec bash -c 'tr -d "\r" | bash -s -- "$@"' bash $LinuxCheckout $revision $root
  if ($LASTEXITCODE -ne 0) { throw 'WSL mounted execution/draining contracts failed' }
  $env:ROVIK_WSL_TEST_DISTRO = $Distro
  $env:ROVIK_WSL_TEST_BINARY = "$LinuxCheckout/src-tauri/target/debug/rovik-agentd"
  $env:ROVIK_WSL_TEST_SOCKET = "$root/agentd.sock"
  cargo test --locked --manifest-path src-tauri/Cargo.toml -p rovik-client --test wsl_backend -- --ignored
  if ($LASTEXITCODE -ne 0) { throw 'Native Windows-to-WSL session acceptance failed' }
} finally {
  # Only this invocation's UUID directory and the daemon whose command names it.
  $cleanup = @'
set -eu
root=$1
if [ -f "$root/pid" ]; then
  pid=$(cat "$root/pid")
  if [ -r "/proc/$pid/cmdline" ] && tr '\0' '\n' < "/proc/$pid/cmdline" | grep -Fxq "$root/data"; then
    kill -TERM "$pid"
    i=0
    while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 100 ]; do sleep .1; i=$((i+1)); done
    if kill -0 "$pid" 2>/dev/null; then echo 'Test daemon did not stop; preserving its data' >&2; exit 1; fi
  fi
fi
if [ -f "$root/daemon.log" ]; then cat "$root/daemon.log"; fi
rm -rf -- "$root"
'@
  $cleanup | & wsl.exe -d $Distro --exec bash -c 'tr -d "\r" | bash -s -- "$@"' bash $root
  $cleanupExit = $LASTEXITCODE
  Remove-Item Env:ROVIK_WSL_TEST_DISTRO, Env:ROVIK_WSL_TEST_BINARY, Env:ROVIK_WSL_TEST_SOCKET -ErrorAction SilentlyContinue
  if ($cleanupExit -ne 0) { throw 'WSL acceptance cleanup failed; retained its diagnostic data' }
}
