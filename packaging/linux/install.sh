#!/bin/sh
# Per-user install of OpenLustre Studio on Linux: copies the binary to
# ~/.local/bin and registers an application-menu shortcut that runs
# `openlustre studio launch` — the same double-click experience the Windows
# installer provides.
#
#   ./packaging/linux/install.sh [path/to/openlustre]
#
# With no argument, uses target/release/openlustre (build first with
# `cargo build --release -p ol_cli`).

set -eu

BIN="${1:-target/release/openlustre}"
if [ ! -x "$BIN" ]; then
    echo "error: $BIN not found or not executable." >&2
    echo "build it first: cargo build --release -p ol_cli" >&2
    exit 1
fi

BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
mkdir -p "$BIN_DIR" "$APP_DIR"

install -m 755 "$BIN" "$BIN_DIR/openlustre"

cat > "$APP_DIR/openlustre-studio.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=OpenLustre Studio
Comment=Graphical Lustre/CoCoSpec modeling IDE
Exec=$BIN_DIR/openlustre studio launch
Terminal=true
Categories=Development;IDE;
EOF

command -v update-desktop-database >/dev/null 2>&1 && \
    update-desktop-database "$APP_DIR" || true

echo "installed: $BIN_DIR/openlustre"
echo "shortcut:  $APP_DIR/openlustre-studio.desktop"
echo "launch:    openlustre studio launch   (or use the application menu)"
