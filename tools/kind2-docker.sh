#!/bin/sh
# Run Kind 2 from its Docker image, for platforms without a native release.
# Point OpenLustre at this script:
#
#   export OPENLUSTRE_KIND2=/path/to/tools/kind2-docker.sh
#
# The directory of every existing file argument is mounted at the same path
# inside the container, so the absolute paths OpenLustre passes keep working.
# The image carries its own SMT solvers; the --z3_bin path OpenLustre may add
# for a host solver is dropped. Override the image with KIND2_IMAGE.

set -eu
IMAGE="${KIND2_IMAGE:-kind2/kind2:dev}"

q() { printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"; }

mounts=""
args=""
skip=0
for a in "$@"; do
    if [ "$skip" = 1 ]; then skip=0; continue; fi
    case "$a" in
        # A host solver path means nothing inside the container.
        --z3_bin|--cvc5_bin|--yices2_bin) skip=1; continue ;;
    esac
    if [ -e "$a" ]; then
        dir=$(cd "$(dirname "$a")" && pwd)
        mounts="$mounts -v $(q "$dir:$dir")"
    fi
    args="$args $(q "$a")"
done

# --version must answer without a TTY or mounts.
eval exec docker run --rm -i $mounts "$IMAGE" $args
