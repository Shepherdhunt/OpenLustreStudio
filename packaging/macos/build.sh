#!/bin/sh
# Build the macOS downloads of OpenLustre Studio into dist/, for one CPU:
#
#   openlustre-studio-<version>-macos-<arch>.pkg      double-click to install
#   openlustre-studio-<version>-macos-<arch>.tar.gz   unpack, ./install.sh
#
#   packaging/macos/build.sh <version> <aarch64|x86_64>
#
# Both install an "OpenLustre Studio" folder in Applications with two apps —
# "OpenLustre Studio" and "PMS Sample" — and put `openlustre` on the PATH.
# The program, Kind 2 + Z3 for that CPU and the samples live inside
# OpenLustre Studio.app. Runs on a Mac (Apple Silicon builds both CPUs).
# Not signed or notarized: see packaging/macos/README-macos.txt.

set -eu
VERSION="${1:?usage: packaging/macos/build.sh <version> <aarch64|x86_64>}"
ARCH="${2:?usage: packaging/macos/build.sh <version> <aarch64|x86_64>}"
case "$ARCH" in aarch64) LABEL=arm64 ;; x86_64) LABEL=x86_64 ;; *) echo "arch: aarch64 or x86_64" >&2; exit 2 ;; esac
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
NAME="openlustre-studio-$VERSION-macos-$LABEL"
DIST="$ROOT/dist"
ID=io.github.shepherdhunt.openlustre-studio
mkdir -p "$DIST"

# The program for this CPU, and one that runs here to fetch the prover.
TARGET="$ARCH-apple-darwin"
rustup target add "$TARGET" > /dev/null
cargo build --release --locked -p ol_cli --target "$TARGET"
cargo build --release --locked -p ol_cli
export HOST_OPENLUSTRE="$ROOT/target/release/openlustre"

WORK="$DIST/work-$LABEL"
rm -rf "$WORK"
FOLDER="$WORK/OpenLustre Studio"
APP="$FOLDER/OpenLustre Studio.app"
RES="$APP/Contents/Resources"
packaging/stage.sh "target/$TARGET/release/openlustre" "macos-$ARCH" "$RES" "$VERSION"

plist() {  # app, executable, identifier, name
    cat > "$1/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>$2</string>
  <key>CFBundleIdentifier</key><string>$3</string>
  <key>CFBundleName</key><string>$4</string>
  <key>CFBundleDisplayName</key><string>$4</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
EOF
}

# The main app: its window is a Terminal with the Studio's log (close it to
# stop the Studio); the Studio itself opens in the browser.
mkdir -p "$APP/Contents/MacOS"
plist "$APP" OpenLustreStudio "$ID" "OpenLustre Studio"
cat > "$APP/Contents/MacOS/OpenLustreStudio" <<'EOF'
#!/bin/sh
exec open -a Terminal "$(dirname "$0")/../Resources/OpenLustre Studio.command"
EOF
cat > "$RES/OpenLustre Studio.command" <<'EOF'
#!/bin/sh
exec "$(dirname "$0")/openlustre" studio launch
EOF

# The PMS sample: the same program, opened on the sample.
SAMPLE="$FOLDER/PMS Sample.app"
mkdir -p "$SAMPLE/Contents/MacOS" "$SAMPLE/Contents/Resources"
plist "$SAMPLE" PMSSample "$ID.pms-sample" "PMS Sample"
cat > "$SAMPLE/Contents/MacOS/PMSSample" <<'EOF'
#!/bin/sh
exec open -a Terminal "$(dirname "$0")/../Resources/PMS Sample.command"
EOF
cat > "$SAMPLE/Contents/Resources/PMS Sample.command" <<'EOF'
#!/bin/sh
exec "$(dirname "$0")/../../../OpenLustre Studio.app/Contents/Resources/openlustre" studio launch --sample pms
EOF
chmod 755 "$APP/Contents/MacOS/OpenLustreStudio" "$RES/OpenLustre Studio.command" \
          "$SAMPLE/Contents/MacOS/PMSSample" "$SAMPLE/Contents/Resources/PMS Sample.command"
cp packaging/macos/uninstall.sh "$RES/uninstall.sh"
chmod 755 "$RES/uninstall.sh"

# The tarball: the folder plus the per-user installer.
TAR="$WORK/tar/$NAME"
mkdir -p "$TAR"
cp -R "$FOLDER" "$TAR/"
cp packaging/macos/install.sh packaging/macos/README-macos.txt "$TAR/"
chmod 755 "$TAR/install.sh"
tar -C "$WORK/tar" -czf "$DIST/$NAME.tar.gz" "$NAME"

# The .pkg: installs the folder into /Applications and links the command.
PKGROOT="$WORK/pkgroot"
mkdir -p "$PKGROOT/Applications" "$WORK/scripts"
cp -R "$FOLDER" "$PKGROOT/Applications/"
cat > "$WORK/scripts/postinstall" <<'EOF'
#!/bin/sh
mkdir -p /usr/local/bin
ln -sf "/Applications/OpenLustre Studio/OpenLustre Studio.app/Contents/Resources/openlustre" /usr/local/bin/openlustre
exit 0
EOF
chmod 755 "$WORK/scripts/postinstall"
# Install where it says, even if a copy of the app exists elsewhere.
pkgbuild --analyze --root "$PKGROOT" "$WORK/components.plist"
i=0
while /usr/libexec/PlistBuddy -c "Print :$i" "$WORK/components.plist" > /dev/null 2>&1; do
    /usr/libexec/PlistBuddy -c "Set :$i:BundleIsRelocatable false" "$WORK/components.plist"
    i=$((i + 1))
done
pkgbuild --root "$PKGROOT" --component-plist "$WORK/components.plist" --scripts "$WORK/scripts" \
    --identifier "$ID" --version "$VERSION" --install-location / "$WORK/component.pkg"
productbuild --package "$WORK/component.pkg" "$DIST/$NAME.pkg"

rm -rf "$WORK"
ls -l "$DIST"
