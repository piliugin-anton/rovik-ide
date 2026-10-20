#!/bin/sh
# Lets bubblewrap make the sandbox Rovik runs the agent's commands in, where
# AppArmor restricts unprivileged user namespaces (Ubuntu 23.10 and later):
# adds the `rovik-bwrap` profile, which gives /usr/bin/bwrap that one
# permission (`userns`), and loads it. Nothing where the restriction is off,
# where AppArmor's tools are missing, or where another profile already covers
# bwrap (Ubuntu's own `bwrap-userns-restrict`, say). Run as root: by the .deb
# and .rpm after installing, by the app's offer to set the sandbox up, and by
# `rovik-agentd admin bubblewrap` on a cloud host.
restricted=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
[ -r "$restricted" ] && [ "$(cat "$restricted")" = 1 ] || exit 0
command -v apparmor_parser >/dev/null 2>&1 || exit 0
profile=/etc/apparmor.d/rovik-bwrap
for other in /etc/apparmor.d/*; do
    [ -f "$other" ] && [ "$other" != "$profile" ] || continue
    if grep -qE "^[[:space:]]*profile[[:space:]].*/usr/bin/bwrap" "$other"; then
        exit 0
    fi
done
cat > "$profile" <<PROFILE
# Rovik IDE: lets bubblewrap make the user namespace the agent's commands are
# sandboxed in. Added by Rovik; removed with it.
abi <abi/4.0>,
include <tunables/global>

profile rovik-bwrap /usr/bin/bwrap flags=(unconfined) {
  userns,

  # Site-specific additions and overrides. See local/README for details.
  include if exists <local/rovik-bwrap>
}
PROFILE
apparmor_parser -r "$profile"
