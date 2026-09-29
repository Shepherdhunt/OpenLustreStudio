#!/bin/sh
# Install the OpenLustre Studio this project is built with, and its prover.
#
#   scripts/install-openlustre.sh
#
# The version is pinned in OPENLUSTRE_VERSION (a commit, a tag such as
# v0.2.0, or a branch) — the project's toolchain, like a SCADE project's
# SCADE version. `openlustre` goes into .openlustre/bin (project-local, not
# committed); Kind 2 and Z3 into .openlustre/tools. scripts/verify.sh and
# scripts/studio.sh use them from there.
#
# Needs a Rust toolchain (https://rustup.rs) to build OpenLustre Studio, and
# git. Set OPENLUSTRE_GIT to build from a fork or a local clone.

set -eu
cd "$(dirname "$0")/.."
GIT="${OPENLUSTRE_GIT:-https://github.com/Shepherdhunt/OpenLustreStudio}"
REF=$(tr -d ' \r\n' < OPENLUSTRE_VERSION)
ROOT="$PWD/.openlustre"

case "$REF" in
    v[0-9]*) SELECT="--tag $REF" ;;
    *[!0-9a-f]*) SELECT="--branch $REF" ;;
    *) SELECT="--rev $REF" ;;
esac

if [ -x "$ROOT/bin/openlustre" ] && [ "$(cat "$ROOT/installed" 2>/dev/null)" = "$REF" ]; then
    echo "install: openlustre $REF already in $ROOT/bin"
else
    echo "install: building openlustre $REF from $GIT"
    # shellcheck disable=SC2086
    cargo install --git "$GIT" $SELECT --locked --force --root "$ROOT" ol_cli
    echo "$REF" > "$ROOT/installed"
fi

export OPENLUSTRE_TOOLS="$ROOT/tools"
if "$ROOT/bin/openlustre" kind2 doctor > /dev/null 2>&1; then
    echo "install: Kind 2 ready in $OPENLUSTRE_TOOLS"
else
    "$ROOT/bin/openlustre" kind2 install
fi
"$ROOT/bin/openlustre" --version
