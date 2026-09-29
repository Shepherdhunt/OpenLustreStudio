#!/bin/sh
# Stage what every Linux and macOS download carries:
#
#   openlustre                    the program (Studio, CLI, code generator)
#   tools/bin/kind2, tools/bin/z3 the prover, for the download's platform
#   tools/kind2-docker.sh         Kind 2 through Docker, as a fallback
#   examples/pms, examples/release_logic   the samples (`studio launch --sample`)
#   README.md, LICENSE, VERSION
#
#   packaging/stage.sh <openlustre> <os-arch> <dir> <version>
#
# <os-arch> is the download's platform (linux-x86_64, macos-aarch64,
# macos-x86_64). Kind 2 and Z3 are fetched by `openlustre kind2 install
# --platform`; set HOST_OPENLUSTRE to a binary that runs here when
# <openlustre> is built for another platform. Examples are copied from git
# (tracked files only), so no build output is ever shipped.

set -eu
BIN="$1"
PLATFORM="$2"
DIR="$3"
VERSION="$4"
ROOT=$(cd "$(dirname "$0")/.." && pwd)

rm -rf "$DIR"
mkdir -p "$DIR/tools" "$DIR/examples"
cp "$BIN" "$DIR/openlustre"
chmod 755 "$DIR/openlustre"

"${HOST_OPENLUSTRE:-$BIN}" kind2 install --dir "$DIR/tools" --platform "$PLATFORM"
cp "$ROOT/tools/kind2-docker.sh" "$DIR/tools/"
chmod 755 "$DIR/tools/kind2-docker.sh"

for ex in pms release_logic; do
    mkdir -p "$DIR/examples/$ex"
    (cd "$ROOT/examples/$ex" && git ls-files -z -- . ':!:.github' | xargs -0 tar -cf - --) \
        | tar -xf - -C "$DIR/examples/$ex"
done

cp "$ROOT/README.md" "$ROOT/LICENSE" "$DIR/"
echo "$VERSION" > "$DIR/VERSION"
echo "stage: $DIR ($PLATFORM, $VERSION)"
