#!/bin/sh
# Build the Linux (x86_64) downloads of OpenLustre Studio into dist/:
#
#   openlustre-studio-<version>-linux-x86_64.tar.gz   unpack, ./install.sh
#   openlustre-studio_<version>_amd64.deb             sudo apt install ./….deb
#
#   packaging/linux/build.sh <version>
#
# Both carry the program, Kind 2 + Z3, and the samples. Build on the oldest
# Ubuntu to support (the binary needs its glibc or newer; the bundled Z3
# needs glibc 2.35, Ubuntu 22.04).

set -eu
VERSION="${1:?usage: packaging/linux/build.sh <version>}"
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
NAME="openlustre-studio-$VERSION-linux-x86_64"
DIST="$ROOT/dist"
mkdir -p "$DIST"

cargo build --release --locked -p ol_cli

# The tarball: the staged folder plus its installer.
STAGE="$DIST/stage/$NAME"
packaging/stage.sh target/release/openlustre linux-x86_64 "$STAGE" "$VERSION"
cp packaging/linux/install.sh packaging/linux/openlustre-studio.svg "$STAGE/"
chmod 755 "$STAGE/install.sh"
tar -C "$DIST/stage" -czf "$DIST/$NAME.tar.gz" "$NAME"

# The .deb: the same files under /opt, the command on PATH, menu entries.
DEBVER=$(echo "$VERSION" | sed 's/-/~/')
PKG="$DIST/deb/openlustre-studio"
rm -rf "$DIST/deb"
mkdir -p "$PKG/DEBIAN" "$PKG/opt/openlustre-studio" "$PKG/usr/bin" "$PKG/usr/share/applications" \
         "$PKG/usr/share/icons/hicolor/scalable/apps" "$PKG/usr/share/doc/openlustre-studio"
cp -R "$STAGE/openlustre" "$STAGE/tools" "$STAGE/examples" "$STAGE/README.md" "$STAGE/LICENSE" "$STAGE/VERSION" \
      "$PKG/opt/openlustre-studio/"
ln -s ../../opt/openlustre-studio/openlustre "$PKG/usr/bin/openlustre"
cp packaging/linux/openlustre-studio.svg "$PKG/usr/share/icons/hicolor/scalable/apps/"
cp LICENSE "$PKG/usr/share/doc/openlustre-studio/copyright"
entry() {
    cat > "$PKG/usr/share/applications/$1" <<EOF
[Desktop Entry]
Type=Application
Name=$2
Comment=Graphical Lustre / CoCoSpec modelling workbench (SCADE-style)
Exec=/usr/bin/openlustre studio launch$3
Icon=openlustre-studio
Terminal=true
Categories=Development;IDE;
EOF
}
entry openlustre-studio.desktop "OpenLustre Studio" ""
entry openlustre-studio-pms.desktop "OpenLustre Studio — PMS sample" " --sample pms"
SIZE=$(du -sk "$PKG" | cut -f1)
cat > "$PKG/DEBIAN/control" <<EOF
Package: openlustre-studio
Version: $DEBVER
Section: devel
Priority: optional
Architecture: amd64
Depends: libc6 (>= 2.35)
Recommends: gcc | clang, xdg-utils
Installed-Size: $SIZE
Maintainer: OpenLustre Studio <https://github.com/Shepherdhunt/OpenLustreStudio/issues>
Homepage: https://github.com/Shepherdhunt/OpenLustreStudio
Description: Graphical Lustre / CoCoSpec modelling workbench (SCADE-style)
 Model synchronous dataflow software graphically (operators, state machines,
 activations, contracts), simulate it, generate C, test the model and the C
 against each other with MC/DC coverage, and prove it with the bundled Kind 2
 prover. For demonstration and prototyping; not a qualified tool.
 Start it from the application menu or with: openlustre studio launch
EOF
cat > "$PKG/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
command -v update-desktop-database > /dev/null 2>&1 && update-desktop-database -q /usr/share/applications || true
EOF
cp "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/postrm"
chmod 755 "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/postrm"
dpkg-deb --root-owner-group -Zxz --build "$PKG" "$DIST/openlustre-studio_${DEBVER}_amd64.deb"

rm -rf "$DIST/stage" "$DIST/deb"
ls -l "$DIST"
