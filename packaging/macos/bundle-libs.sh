#!/bin/bash
# Make a staged macOS download self-contained: every program in it may load
# only macOS's own libraries (/usr/lib, /System) and libraries shipped in it.
#
# Kind 2's Intel build loads ZeroMQ from Homebrew
# (/usr/local/opt/zeromq/lib/libzmq.5.dylib), which a Mac without Homebrew's
# zeromq does not have, so Kind 2 would not start. This builds that library
# from source for the download's CPU and macOS 12 (the same version and ABI
# as Homebrew's; Kind 2 uses no draft API and no CURVE), puts it in
# tools/lib and points kind2 at it. Then it checks every program.
#
#   packaging/macos/bundle-libs.sh <Resources dir> <aarch64|x86_64>

set -euo pipefail
RES="$1"
ARCH="$2"
case "$ARCH" in aarch64) CPU=arm64 ;; *) CPU="$ARCH" ;; esac
ZMQ_VERSION=4.3.5
KIND2="$RES/tools/bin/kind2"
LIB="$RES/tools/lib"
IN_BUNDLE=@executable_path/../lib

deps() { otool -L "$1" | tail -n +2 | awk '{print $1}'; }

zmq=$(deps "$KIND2" | grep '/libzmq\.5\.dylib$' || true)
if [ -n "$zmq" ] && [ "$zmq" != "$IN_BUNDLE/libzmq.5.dylib" ]; then
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    curl -sSfL -o "$WORK/zmq.tar.gz" \
        "https://github.com/zeromq/libzmq/releases/download/v$ZMQ_VERSION/zeromq-$ZMQ_VERSION.tar.gz"
    tar -C "$WORK" -xzf "$WORK/zmq.tar.gz"
    # The CPU goes in the compiler commands (libtool drops -arch from
    # LDFLAGS); only the library is built.
    (
        cd "$WORK/zeromq-$ZMQ_VERSION"
        export MACOSX_DEPLOYMENT_TARGET=12.0
        ./configure --host="$ARCH-apple-darwin" --disable-static \
            --without-libsodium --disable-curve --without-docs --disable-Werror \
            CC="clang -arch $CPU" CXX="clang++ -arch $CPU" \
            > "$WORK/configure.log" 2>&1 || { tail -40 "$WORK/configure.log"; exit 1; }
        make -j"$(sysctl -n hw.ncpu)" src/libzmq.la > "$WORK/make.log" 2>&1 || { tail -40 "$WORK/make.log"; exit 1; }
    )
    built="$WORK/zeromq-$ZMQ_VERSION/src/.libs/libzmq.5.dylib"
    [ "$(lipo -archs "$built")" = "$CPU" ] || { echo "bundle-libs: libzmq was built for $(lipo -archs "$built"), not $CPU" >&2; exit 1; }
    missing=$(comm -23 <(nm -u "$KIND2" | grep -o '_zmq_[a-z0-9_]*' | sort -u) \
                       <(nm -gU "$built" | grep -o '_zmq_[a-z0-9_]*' | sort -u))
    [ -z "$missing" ] || { echo "bundle-libs: libzmq lacks what kind2 calls: $missing" >&2; exit 1; }
    mkdir -p "$LIB"
    cp "$built" "$LIB/"
    chmod 644 "$LIB/libzmq.5.dylib"
    install_name_tool -id "$IN_BUNDLE/libzmq.5.dylib" "$LIB/libzmq.5.dylib"
    install_name_tool -change "$zmq" "$IN_BUNDLE/libzmq.5.dylib" "$KIND2"
    codesign --force --sign - "$LIB/libzmq.5.dylib"
    codesign --force --sign - "$KIND2"
    echo "bundle-libs: kind2 loads ZeroMQ $ZMQ_VERSION from tools/lib (was $zmq)"
fi

# Every program and library in the download loads only macOS's libraries
# and its own.
bad=0
for f in "$RES/openlustre" "$RES"/tools/bin/* "$RES"/tools/lib/*; do
    [ -f "$f" ] || continue
    case "$(file -b "$f")" in *Mach-O*) ;; *) continue ;; esac
    for d in $(deps "$f"); do
        case "$d" in
            /usr/lib/* | /System/*) ;;
            "$IN_BUNDLE"/*)
                [ -e "$LIB/${d#"$IN_BUNDLE"/}" ] || { echo "bundle-libs: ${f#"$RES"/} loads $d, which is missing" >&2; bad=1; } ;;
            *) echo "bundle-libs: ${f#"$RES"/} loads $d, which is neither part of macOS nor of the download" >&2; bad=1 ;;
        esac
    done
done
[ "$bad" = 0 ]
echo "bundle-libs: $RES loads only macOS's libraries and its own"
