#!/bin/sh
# Prove every example with Kind 2 and build its evidence report.
#
#   tools/prove-examples.sh [path/to/openlustre] [out-dir]
#
# Each example is a folder under examples/: a Studio workspace (*.wksc) or
# a model in model/*.json, with its test scenarios in scenarios/. Fails when a property does not hold
# or an evidence verdict is FAIL. Kind 2 must be set up first
# (`openlustre kind2 install`, then `openlustre kind2 doctor`).

set -eu
OL="${1:-target/debug/openlustre}"
OUT="${2:-evidence}"
TIMEOUT="${PROVE_TIMEOUT:-600}"
mkdir -p "$OUT"

status=0
for dir in examples/*/; do
    name=$(basename "$dir")
    for model in "$dir"*.wksc "$dir"model/*.json; do
        [ -f "$model" ] || continue
        echo "== $name: proving $(basename "$model")"
        if ! "$OL" prove "$model" --timeout "$TIMEOUT"; then
            echo "!! $name: not every property holds"
            status=1
        fi
        echo "== $name: evidence"
        mkdir -p "$OUT/$name"
        if ! "$OL" evidence "$model" --scenarios "${dir}scenarios" --prove --timeout "$TIMEOUT" --out "$OUT/$name"; then
            echo "!! $name: evidence verdict FAIL"
            status=1
        fi
    done
done
exit $status
