#!/bin/sh
# Install each macOS download for this Mac's CPU the way a user would, check
# the installed copy works (packaging/smoke-lib.sh), then uninstall. The
# other CPU's download is unpacked and run under Rosetta when it is there.
#
#   packaging/macos/smoke.sh dist        (CI; the .pkg part needs sudo)

set -eu
DIST=$(cd "${1:-dist}" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
. "$(dirname "$0")/../smoke-lib.sh"
case "$(uname -m)" in arm64) HERE_CPU=arm64; OTHER=x86_64 ;; *) HERE_CPU=x86_64; OTHER=arm64 ;; esac
RES_REL="OpenLustre Studio/OpenLustre Studio.app/Contents/Resources"

# 1. The .pkg, through the macOS installer.
sudo installer -pkg "$DIST"/openlustre-studio-*-macos-$HERE_CPU.pkg -target /
test -d "/Applications/OpenLustre Studio/PMS Sample.app"
test -x "/Applications/OpenLustre Studio/OpenLustre Studio.app/Contents/MacOS/OpenLustreStudio"
check_installed /usr/local/bin/openlustre ".pkg ($HERE_CPU)" "/Applications/$RES_REL/examples"
sudo "/Applications/$RES_REL/uninstall.sh"
test ! -e "/Applications/OpenLustre Studio"
test ! -e /usr/local/bin/openlustre

# 2. The tarball and its installer, for one user.
tar -C "$WORK" -xzf "$DIST"/openlustre-studio-*-macos-$HERE_CPU.tar.gz
BUNDLE=$(echo "$WORK"/openlustre-studio-*-macos-$HERE_CPU)
HOME="$WORK/user" "$BUNDLE/install.sh"
check_installed "$WORK/user/.local/bin/openlustre" "tarball, per-user install ($HERE_CPU)" "$WORK/user/Applications/$RES_REL/examples"
HOME="$WORK/user" "$BUNDLE/install.sh" --uninstall
test ! -e "$WORK/user/.local/bin/openlustre"

# 3. The other CPU's download: runs under Rosetta, if installed.
tar -C "$WORK" -xzf "$DIST"/openlustre-studio-*-macos-$OTHER.tar.gz
OTHER_OL=$(echo "$WORK"/openlustre-studio-*-macos-$OTHER)/"$RES_REL/openlustre"
file "$OTHER_OL"
if [ "$OTHER" = x86_64 ] && arch -x86_64 /usr/bin/true 2> /dev/null; then
    arch -x86_64 "$OTHER_OL" --version
    HOME="$WORK" env -u OPENLUSTRE_TOOLS arch -x86_64 "$OTHER_OL" kind2 doctor
else
    echo "(the $OTHER download is built but cannot run on this Mac)"
fi
echo "smoke: macOS downloads install, run, prove and uninstall"
