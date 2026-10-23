#!/bin/sh
# Install OpenLustre Studio from this folder (the unpacked macOS download).
#
#   ./install.sh                  for everyone: /Applications/OpenLustre Studio,
#                                 /usr/local/bin/openlustre — asks for your Mac
#                                 password, like other installers, then starts
#                                 the Studio
#   ./install.sh --user           just for you: ~/Applications/OpenLustre Studio,
#                                 ~/.local/bin/openlustre (no password)
#   ./install.sh --uninstall      remove it again (--user for the per-user one)
#
# The folder holds two apps: "OpenLustre Studio" and "PMS Sample". Each opens
# the Studio in your browser, with its log in a Terminal window (close the
# window to stop the Studio). Kind 2 and Z3 come bundled. The .pkg download
# installs the same thing with the macOS installer.

set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
SYSTEM=1
UNINSTALL=0
for arg in "$@"; do
    case "$arg" in
        --system) SYSTEM=1 ;;
        --user) SYSTEM=0 ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
        *) echo "install: unknown option $arg (see --help)" >&2; exit 2 ;;
    esac
done
# sudo is used only for the copy into a shared location; the script itself
# keeps running as you, so it can start the Studio as you at the end.
SUDO=""
if [ "$SYSTEM" = 1 ]; then
    APPS=/Applications
    BIN=/usr/local/bin
    if [ "$(id -u)" != 0 ]; then
        SUDO=sudo
        echo "Installing for everyone into /Applications — you may be asked for your password."
    fi
else
    APPS="$HOME/Applications"
    BIN="$HOME/.local/bin"
fi
FOLDER="$APPS/OpenLustre Studio"
APP="$FOLDER/OpenLustre Studio.app"

if [ "$UNINSTALL" = 1 ]; then
    $SUDO rm -rf "$FOLDER"
    $SUDO rm -f "$BIN/openlustre"
    echo "uninstalled OpenLustre Studio from $FOLDER"
    echo "(your projects and ~/OpenLustre are left alone)"
    exit 0
fi

[ -d "$HERE/OpenLustre Studio/OpenLustre Studio.app" ] || { echo "install: run this from the unpacked download" >&2; exit 1; }
$SUDO mkdir -p "$APPS" "$BIN"
$SUDO rm -rf "$FOLDER"
$SUDO cp -R "$HERE/OpenLustre Studio" "$APPS/"
# A downloaded archive is quarantined; this copy is the one you chose to
# install, so clear the flag and it opens with no developer-verification bar.
$SUDO xattr -dr com.apple.quarantine "$FOLDER" 2> /dev/null || true
$SUDO chmod -R a+rX "$FOLDER"
OL="$APP/Contents/Resources/openlustre"
$SUDO ln -sf "$OL" "$BIN/openlustre"
VERSION=$(cat "$APP/Contents/Resources/VERSION")

echo "installed OpenLustre Studio $VERSION in $FOLDER"
echo "  apps:     OpenLustre Studio, PMS Sample (in $FOLDER)"
echo "  command:  $BIN/openlustre"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "            ($BIN is not on your PATH yet: add it to your shell profile)";; esac
echo "  prover:   $("$OL" kind2 doctor > /dev/null 2>&1 && echo "Kind 2 ready" || echo "run 'openlustre kind2 doctor' to check")"
if [ "$SYSTEM" = 1 ] && [ -d "$HOME/Applications/OpenLustre Studio" ]; then
    echo "  note:     found an older copy in ~/Applications/OpenLustre Studio from a"
    echo "            previous install; delete it with:  rm -rf ~/Applications/OpenLustre\ Studio"
fi
echo "  cleanup:  you can delete this download folder (and eject the .dmg) now."

# Start the installed copy itself — by full path, so a quarantined copy that
# is still in Downloads or on the mounted .dmg can never be picked instead.
if [ "$(id -u)" = 0 ]; then
    echo "not auto-starting under sudo; open it from Applications, or run:"
    echo "  open \"$APP\""
else
    echo ""
    echo "Starting the Studio: a Terminal window opens with its log, then the"
    echo "Studio opens in your browser. To start it later: open \"$APP\""
    open "$APP"
fi
