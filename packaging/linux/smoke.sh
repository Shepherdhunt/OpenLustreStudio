#!/bin/sh
# Install each Linux download the way a user would, check the installed copy
# works (packaging/smoke-lib.sh), then uninstall.
#
#   packaging/linux/smoke.sh dist        (CI; the .deb part needs sudo)

set -eu
DIST=$(cd "${1:-dist}" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

. "$(dirname "$0")/../smoke-lib.sh"

# 1. The tarball and its installer, for one user.
tar -C "$WORK" -xzf "$DIST"/openlustre-studio-*-linux-x86_64.tar.gz
BUNDLE=$(echo "$WORK"/openlustre-studio-*-linux-x86_64)
HOME="$WORK/user" "$BUNDLE/install.sh"
test -f "$WORK/user/.local/share/applications/openlustre-studio-pms.desktop"
check_installed "$WORK/user/.local/bin/openlustre" "tarball, per-user install" "$WORK/user/.local/share/openlustre-studio/examples"
HOME="$WORK/user" "$BUNDLE/install.sh" --uninstall
test ! -e "$WORK/user/.local/bin/openlustre"

# 2. The .deb, through apt.
if command -v apt-get > /dev/null 2>&1 && sudo -n true 2> /dev/null; then
    sudo apt-get install -y -q --no-install-recommends "$DIST"/openlustre-studio_*_amd64.deb > /dev/null
    test -f /usr/share/applications/openlustre-studio.desktop
    check_installed /usr/bin/openlustre ".deb" /opt/openlustre-studio/examples
    sudo apt-get remove -y -q openlustre-studio > /dev/null
    test ! -e /usr/bin/openlustre
else
    echo "(skipping the .deb: needs apt and sudo)"
fi
echo "smoke: Linux downloads install, run, prove and uninstall"
