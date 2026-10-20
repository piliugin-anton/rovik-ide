#!/bin/sh
# Removes the AppArmor profile bwrap-apparmor.sh added, when Rovik is
# removed for good: the .deb's postrm (`remove`, `purge`) or the .rpm's
# %postun (0 left). An upgrade keeps it: the .rpm runs the old package's
# %postun after the new one's %post.
case "$1" in
    remove | purge | 0) ;;
    *) exit 0 ;;
esac
profile=/etc/apparmor.d/rovik-bwrap
[ -f "$profile" ] || exit 0
if command -v apparmor_parser >/dev/null 2>&1; then
    apparmor_parser -R "$profile" 2>/dev/null || true
fi
rm -f "$profile"
