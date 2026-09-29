#!/bin/sh
# Install OpenLustre Studio from this folder (the unpacked Linux download).
#
#   ./install.sh                    for you: ~/.local/share/openlustre-studio,
#                                   ~/.local/bin/openlustre, app-menu entries
#   sudo ./install.sh --system      for everyone: /opt/openlustre-studio,
#                                   /usr/local/bin/openlustre, app-menu entries
#   ./install.sh --uninstall        remove it again (add --system for the
#                                   system-wide install)
#
# The app menu gets "OpenLustre Studio" and "OpenLustre Studio — PMS sample";
# each opens the Studio in your browser, with its log in a terminal window
# (close the window to stop the Studio). Kind 2 and Z3 come bundled.
# On Ubuntu/Debian the .deb download installs the same thing with apt.

set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
SYSTEM=0
UNINSTALL=0
for arg in "$@"; do
    case "$arg" in
        --system) SYSTEM=1 ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
        *) echo "install: unknown option $arg (see --help)" >&2; exit 2 ;;
    esac
done

if [ "$SYSTEM" = 1 ]; then
    APP=/opt/openlustre-studio
    BIN=/usr/local/bin
    MENU=/usr/share/applications
    ICONS=/usr/share/icons/hicolor/scalable/apps
else
    DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
    APP="$DATA/openlustre-studio"
    BIN="$HOME/.local/bin"
    MENU="$DATA/applications"
    ICONS="$DATA/icons/hicolor/scalable/apps"
fi

refresh_menu() {
    command -v update-desktop-database > /dev/null 2>&1 && update-desktop-database "$MENU" 2> /dev/null || true
}

if [ "$UNINSTALL" = 1 ]; then
    rm -rf "$APP"
    rm -f "$BIN/openlustre" "$MENU/openlustre-studio.desktop" "$MENU/openlustre-studio-pms.desktop" \
          "$ICONS/openlustre-studio.svg"
    refresh_menu
    echo "uninstalled OpenLustre Studio from $APP"
    echo "(your projects and ~/OpenLustre are left alone)"
    exit 0
fi

[ -x "$HERE/openlustre" ] || { echo "install: run this from the unpacked download (no openlustre next to it)" >&2; exit 1; }

rm -rf "$APP"
mkdir -p "$APP" "$BIN" "$MENU" "$ICONS"
cp -R "$HERE/openlustre" "$HERE/tools" "$HERE/examples" "$HERE/README.md" "$HERE/LICENSE" "$HERE/VERSION" "$APP/"
ln -sf "$APP/openlustre" "$BIN/openlustre"
cp "$HERE/openlustre-studio.svg" "$ICONS/"

entry() {  # file name, title, extra arguments
    cat > "$MENU/$1" <<EOF
[Desktop Entry]
Type=Application
Name=$2
Comment=Graphical Lustre / CoCoSpec modelling workbench (SCADE-style)
Exec=$APP/openlustre studio launch$3
Icon=openlustre-studio
Terminal=true
Categories=Development;IDE;
EOF
}
entry openlustre-studio.desktop "OpenLustre Studio" ""
entry openlustre-studio-pms.desktop "OpenLustre Studio — PMS sample" " --sample pms"
refresh_menu

echo "installed OpenLustre Studio $(cat "$APP/VERSION") in $APP"
echo "  command:  $BIN/openlustre"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "            ($BIN is not on your PATH yet: add it, or log out and in)";; esac
echo "  app menu: OpenLustre Studio, OpenLustre Studio — PMS sample"
echo "  prover:   $("$APP/openlustre" kind2 doctor > /dev/null 2>&1 && echo "Kind 2 ready" || echo "run 'openlustre kind2 doctor' to check")"
echo "start it:   openlustre studio launch   (or: openlustre studio launch --sample pms)"
