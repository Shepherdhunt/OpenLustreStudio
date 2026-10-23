#!/bin/sh
# Pack every OS's installers into ONE shareable tarball:
#
#   OpenLustreStudio-<v>-installers.tar.gz
#   └── OpenLustreStudio-<v>/
#       ├── INSTALL.txt          what to run on which OS
#       ├── macos/               .dmg (drag & drop), .pkg, .tar.gz  (arm64/x86_64)
#       ├── windows-x64/         -Setup.exe, portable .zip
#       └── ubuntu-x64/          .deb, .tar.gz
#
#   packaging/bundle.sh <version> [dist-dir]      (default dist-dir: dist/)
#
# Whatever installers the directory holds are packed; an OS folder with
# nothing to put in it gets a WHERE-TO-GET file instead, so a build on one
# machine (say, a Mac: macOS + Ubuntu-in-Docker) still produces one
# shareable file — the missing pieces come from the repository's GitHub
# release or the package workflow's bundle artifact, which is built by CI
# on all three OSes and assembles the complete tarball with this script.

set -eu
VERSION="${1:?usage: packaging/bundle.sh <version> [dist-dir]}"
DIST="${2:-dist}"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SRC="$ROOT/$DIST"
OUT="OpenLustreStudio-$VERSION"
TAR="$ROOT/$DIST/$OUT-installers.tar.gz"

[ -d "$SRC" ] || { echo "bundle: no $SRC directory" >&2; exit 2; }
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/ol-bundle.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
B="$STAGE/$OUT"
mkdir -p "$B/macos" "$B/windows-x64" "$B/ubuntu-x64"

copy() { # copy <dest-dir> <glob>... — every dist/ file matching a glob
    dest=$1
    shift
    for pat in "$@"; do
        for f in $SRC/$pat; do
            [ -f "$f" ] || continue
            cp "$f" "$dest/"
        done
    done
}

copy "$B/macos" '*-macos-*.dmg' '*-macos-*.pkg' '*-macos-*.tar.gz'
copy "$B/windows-x64" '*Setup.exe' '*windows-x86_64.zip'
copy "$B/ubuntu-x64" '*_amd64.deb' '*-linux-x86_64.tar.gz'

CI_HINT="Get it from this repository's GitHub Releases, or Actions > package \
> OpenLustreStudio-$VERSION-installers-tarball (built by CI on all three OSes)."
[ -n "$(ls -A "$B/macos" 2>/dev/null)" ] || printf '%s\n' \
    "The macOS installers are not in this bundle." "$CI_HINT" > "$B/macos/WHERE-TO-GET.txt"
[ -n "$(ls -A "$B/windows-x64" 2>/dev/null)" ] || printf '%s\n' \
    "The Windows installer is not in this bundle (its build needs Inno Setup on Windows)." "$CI_HINT" > "$B/windows-x64/WHERE-TO-GET.txt"
[ -n "$(ls -A "$B/ubuntu-x64" 2>/dev/null)" ] || printf '%s\n' \
    "The Ubuntu installers are not in this bundle." "$CI_HINT" > "$B/ubuntu-x64/WHERE-TO-GET.txt"

cat > "$B/INSTALL.txt" <<EOF
OpenLustre Studio $VERSION — one download, every OS
====================================================

Pick your folder. Every install ships the Studio, the block library and
the PMS sample; macOS and Ubuntu also bundle the Kind 2 prover (on Windows
it runs through WSL or Docker — see windows/README-windows.txt after
installing). Not signed/notarized: macOS may ask you to right-click >
Open the first time.

macos/   (macOS 13+; arm64 = Apple Silicon M1-M4, x86_64 = Intel)
  .dmg     The drag-and-drop install: open it, drag "OpenLustre Studio"
           onto the Applications folder in the same window. (Optional:
           run install.sh beside it for the \`openlustre\` terminal command.)
  .pkg     Double-click installer; also links \`openlustre\` into
           /usr/local/bin.
  .tar.gz  Unpack, ./install.sh (per-user, no admin password).

windows-x64/   (Windows 10/11)
  -Setup.exe   The setup wizard: installs for you or everyone, Start Menu
               shortcuts, optional PATH entry. Uninstall from Settings > Apps.
  .zip         Portable: unzip anywhere and run.

ubuntu-x64/    (Ubuntu 22.04+ / Debian; other distros: use the source —
               see the project README)
  .deb        sudo apt install ./openlustre-studio_*_amd64.deb
               (application menu entries + \`openlustre\` on PATH).
  .tar.gz     Unpack, ./install.sh (per-user).

Start it: "OpenLustre Studio" (or "PMS Sample") from your Applications /
Start Menu / application menu; the Studio opens in your browser. From a
terminal: openlustre studio launch --sample pms
EOF

tar -C "$STAGE" -czf "$TAR" "$OUT"
echo "bundle: $TAR"
for d in macos windows-x64 ubuntu-x64; do
    echo "  $d:" $(ls "$B/$d" | sed 's/^/    /' | tr '\n' ' ')
done
ls -lh "$TAR"
