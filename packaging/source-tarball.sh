#!/bin/sh
# A shareable source tarball of OpenLustre Studio — the same tree the GitHub
# page shows, ready to hand to someone or park on a file share:
#
#   openlustre-studio-<version>-src.tar.gz
#     └── openlustre-studio-<version>-src/
#
# Untar it anywhere with a Rust toolchain and:
#
#   cargo build --release -p ol_cli          # → target/release/openlustre
#   target/release/openlustre studio launch --sample pms
#
# Usage: packaging/source-tarball.sh [version]
# With no argument the version is <cargo version>[-dev.<git sha>]. In a git
# checkout the WORKING TREE is packaged (tracked files plus unignored new
# ones, at their current content — uncommitted changes ship too; ignored
# files like target/ and dist/ never do). Outside git the same tree is
# copied with the standard exclusions.

set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

BASE=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ "${1:-}" ]; then
    VERSION="$1"
elif git rev-parse --git-dir > /dev/null 2>&1; then
    SHA=$(git rev-parse --short HEAD)
    VERSION="$BASE-dev.$SHA"
else
    VERSION="$BASE"
fi

NAME="openlustre-studio-$VERSION-src"
DIST="$ROOT/dist"
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/ol-src.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/$NAME" "$DIST"

if git rev-parse --git-dir > /dev/null 2>&1; then
    # The working tree: tracked files (with uncommitted edits) plus new
    # unignored ones; .gitignore keeps build output out.
    git ls-files --cached --others --exclude-standard -z \
        | tar --null -cf - -T - | tar -xf - -C "$STAGE/$NAME"
    METHOD="git working tree (tracked + unignored, edits included)"
else
    # The same set by exclusion: sources, docs, packaging, examples, tests.
    tar -cf - --exclude ./.git --exclude ./target --exclude ./dist \
        --exclude ./node_modules --exclude './.DS_Store' \
        --exclude '*/.DS_Store' --exclude './examples/*/out' \
        --exclude './examples/*/build' --exclude './.claude' . \
        | tar -xf - -C "$STAGE/$NAME"
    METHOD="copy (excluded .git, target, dist, node_modules)"
fi

echo "$VERSION" > "$STAGE/$NAME/VERSION"

tar -C "$STAGE" -czf "$DIST/$NAME.tar.gz" "$NAME"
echo "packaged: $DIST/$NAME.tar.gz ($METHOD)"
ls -l "$DIST/$NAME.tar.gz"
