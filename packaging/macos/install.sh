#!/bin/sh
# Install OpenLustre Studio from this folder (the unpacked macOS download).
#
#   ./install.sh                  for you: ~/Applications/OpenLustre Studio,
#                                 ~/.local/bin/openlustre
#   sudo ./install.sh --system    for everyone: /Applications/OpenLustre Studio,
#                                 /usr/local/bin/openlustre
#   ./install.sh --uninstall      remove it again (add --system for the
#                                 system-wide install)
#
# The folder holds two apps: "OpenLustre Studio" and "PMS Sample". Each opens
# the Studio in your browser, with its log in a Terminal window (close the
# window to stop the Studio). Kind 2 and Z3 come bundled. The .pkg download
# installs the same thing with the macOS installer.

set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
SYSTEM=0
UNINSTALL=0
for arg in "$@"; do
    case "$arg" in
        --system) SYSTEM=1 ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
        *) echo "install: unknown option $arg (see --help)" >&2; exit 2 ;;
    esac
done
if [ "$SYSTEM" = 1 ]; then
    APPS=/Applications
    BIN=/usr/local/bin
else
    APPS="$HOME/Applications"
    BIN="$HOME/.local/bin"
fi
FOLDER="$APPS/OpenLustre Studio"

if [ "$UNINSTALL" = 1 ]; then
    rm -rf "$FOLDER"
    rm -f "$BIN/openlustre"
    echo "uninstalled OpenLustre Studio from $FOLDER"
    echo "(your projects and ~/OpenLustre are left alone)"
    exit 0
fi

[ -d "$HERE/OpenLustre Studio/OpenLustre Studio.app" ] || { echo "install: run this from the unpacked download" >&2; exit 1; }
mkdir -p "$APPS" "$BIN"
rm -rf "$FOLDER"
cp -R "$HERE/OpenLustre Studio" "$APPS/"
# A downloaded archive is quarantined; this copy is the one you chose to install.
xattr -dr com.apple.quarantine "$FOLDER" 2> /dev/null || true
OL="$FOLDER/OpenLustre Studio.app/Contents/Resources/openlustre"
ln -sf "$OL" "$BIN/openlustre"

echo "installed OpenLustre Studio $(cat "$FOLDER/OpenLustre Studio.app/Contents/Resources/VERSION") in $FOLDER"
echo "  apps:     OpenLustre Studio, PMS Sample (in $APPS/OpenLustre Studio)"
echo "  command:  $BIN/openlustre"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "            ($BIN is not on your PATH yet: add it to your shell profile)";; esac
echo "  prover:   $("$OL" kind2 doctor > /dev/null 2>&1 && echo "Kind 2 ready" || echo "run 'openlustre kind2 doctor' to check")"
