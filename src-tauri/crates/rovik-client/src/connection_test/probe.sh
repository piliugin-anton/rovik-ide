# What a server offers Rovik (H04): one `key=value` line each, read by
# `rovik_client::probe`. POSIX sh, as busybox runs it too: it's sent on
# stdin to `sh -s`, so the user's login shell never parses it. It asks
# nothing of the network.
say() { printf '%s=%s\n' "$1" "$2"; }
say os "$(uname -s)"
say arch "$(uname -m)"
say uid "$(id -u)"
say user "$(id -un)"
say home "$HOME"
say disk_kb "$(df -Pk "$HOME" 2>/dev/null | awk 'NR == 2 { print $4 }')"
say git "$(git --version 2>/dev/null)"
# As the browser tools look: the browser Rovik pins on servers (D19), then
# Chromium, then Chrome.
chrome=
for found in /opt/rovik/browser/*/chrome; do
  if [ -x "$found" ]; then chrome=$found; fi
done
if [ -z "$chrome" ]; then
  for name in chromium chromium-browser google-chrome google-chrome-stable; do
    if found=$(command -v "$name" 2>/dev/null); then chrome=$found; break; fi
  done
fi
say chrome "$chrome"
# A Chrome stuck as it starts (waiting on a D-Bus that never answers)
# outlives the TERM: the KILL 5 s later ends it, and the probe goes on.
if [ -n "$chrome" ]; then
  if timeout -k 5 30 "$chrome" --headless=new --disable-gpu --no-first-run --dump-dom about:blank >/dev/null 2>&1; then
    say chrome_headless yes
  else
    say chrome_headless no
  fi
fi
# Trying is the surest answer; without unshare, the kernel's switches as
# the daemon reads them.
if command -v unshare >/dev/null 2>&1; then
  if unshare -Ur true 2>/dev/null; then say userns yes; else say userns no; fi
else
  max=$(cat /proc/sys/user/max_user_namespaces 2>/dev/null)
  clone=$(cat /proc/sys/kernel/unprivileged_userns_clone 2>/dev/null)
  apparmor=$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)
  if [ "${max:-0}" -gt 0 ] 2>/dev/null && [ "$clone" != 0 ] && [ "$apparmor" != 1 ]; then
    say userns yes
  else
    say userns no
  fi
fi
# bubblewrap sandboxes the agent's commands; Rovik's install adds it.
say bwrap "$(command -v bwrap 2>/dev/null)"
# pasta gives each user's commands a network of their own on a server
# several people share; Rovik's install adds it. WSL, one person's
# computer, needs none.
say pasta "$(command -v pasta 2>/dev/null)"
case "$(cat /proc/sys/kernel/osrelease 2>/dev/null)" in
  *[Mm]icrosoft*) say wsl yes ;;
esac
# Where it can't, the kernel's Landlock does, more lightly: Linux 6.2 or
# later, with Landlock among its security modules (maybe, where the list
# can't be read). Without either, the commands are refused.
release=$(uname -r)
major=${release%%.*}
rest=${release#*.}
minor=${rest%%[!0-9]*}
recent=no
if [ "${major:-0}" -gt 6 ] 2>/dev/null || { [ "${major:-0}" -eq 6 ] && [ "${minor:-0}" -ge 2 ]; } 2>/dev/null; then
  recent=yes
fi
if [ "$recent" = no ]; then
  say landlock no
elif [ -r /sys/kernel/security/lsm ]; then
  if grep -qw landlock /sys/kernel/security/lsm; then say landlock yes; else say landlock no; fi
else
  say landlock maybe
fi
if [ -e /dev/fuse ]; then say fuse yes; else say fuse no; fi
say fusermount "$(command -v fusermount3 2>/dev/null || command -v fusermount 2>/dev/null)"
if [ -d /run/systemd/system ]; then say systemd yes; else say systemd no; fi
if command -v sudo >/dev/null 2>&1; then
  if sudo -n true 2>/dev/null; then
    say sudo yes
  elif LC_ALL=C sudo -n true 2>&1 | grep -qi password; then
    say sudo password
  else
    say sudo no
  fi
else
  say sudo none
fi
if [ -f /etc/rovik/agentd.toml ]; then
  say rovik "$(/opt/rovik/bin/rovik-agentd version 2>/dev/null)"
  say rovik_admins "$(sed -n 's/^[[:space:]]*admins[[:space:]]*=[[:space:]]*//p' /etc/rovik/agentd.toml)"
fi
if [ -S "$HOME/.rovik/agentd.sock" ]; then say socket yes; else say socket no; fi
# Other users' daemons, through /proc as the daemon finds them: their
# processes are named rovik-agentd.
me=$(id -u)
for status in $(grep -ls '^Name:[[:space:]]*rovik-agentd$' /proc/[0-9]*/status); do
  uid=$(awk '/^Uid:/ { print $2 }' "$status" 2>/dev/null)
  if [ -n "$uid" ] && [ "$uid" != "$me" ]; then
    name=$(awk -F: -v uid="$uid" '$3 == uid { print $1; exit }' /etc/passwd)
    say daemon "${name:-$uid}"
  fi
done
