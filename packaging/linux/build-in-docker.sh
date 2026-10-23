#!/bin/sh
# Build the Ubuntu (x86_64) downloads of OpenLustre Studio into dist/ from
# any machine with Docker — the .deb must be built against the oldest
# supported Ubuntu's glibc (2.35, Ubuntu 22.04), and a container of exactly
# that Ubuntu guarantees it without a VM.
#
#   packaging/linux/build-in-docker.sh <version>
#
# Same outputs as packaging/linux/build.sh:
#   openlustre-studio-<version>-linux-x86_64.tar.gz   unpack, ./install.sh
#   openlustre-studio_<version>_amd64.deb             sudo apt install ./….deb
#
# On an Apple Silicon Mac the container runs the arm64 Ubuntu image and the
# x86_64 program is CROSS-COMPILED in it (native compile speed, and no
# emulated-JVM hang in Kind 2's install smoke, which stage.sh skips because
# the host no longer matches the packaged platform). On an x86_64 host the
# container is amd64 and everything builds and checks natively. Needs a git
# checkout (stage.sh copies the examples with git ls-files) and network
# (Rust toolchain, cross packages, Kind 2, Z3).

set -eu
VERSION="${1:?usage: packaging/linux/build-in-docker.sh <version>}"
ROOT=$(cd "$(dirname "$0")/../.." && pwd)

exec docker run --rm -v "$ROOT":/src -w /src ubuntu:22.04 bash -c '
    set -e
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq > /dev/null
    apt-get install -y -qq ca-certificates curl build-essential pkg-config python3 git \
        gcc-x86-64-linux-gnu libc6-dev-amd64-cross > /dev/null
    curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable > /dev/null 2>&1
    . "$HOME/.cargo/env"
    if [ "$(uname -m)" = aarch64 ]; then
        # Apple Silicon host: the container is arm64; cross-compile amd64.
        rustup target add x86_64-unknown-linux-gnu
        cargo build --release --locked -p ol_cli   # arm64 helper: fetches Kind 2 / Z3
        export HOST_OPENLUSTRE="$PWD/target/release/openlustre"
        export CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu
        export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
    fi
    packaging/linux/build.sh '"$VERSION"'
    chown -R '"$(id -u):$(id -g)"' /src/dist
'
