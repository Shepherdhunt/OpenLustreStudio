#!/bin/sh
# Refresh examples/pms from the PayloadManagementSystem repository.
#
#   tools/sync-pms.sh <path/to/a/PayloadManagementSystem/checkout>
#
# The PMS is developed in its own repository (it pins the OpenLustre Studio
# it is built with and verifies itself there). examples/pms is a snapshot
# of it: the sample shipped in every download, and a regression test of the
# tool (tests/pms_example.rs, CI). Copies exactly the checkout's tracked
# files, so the snapshot never picks up build outputs.

set -eu
SRC="${1:?usage: tools/sync-pms.sh <PayloadManagementSystem checkout>}"
cd "$(dirname "$0")/.."
[ -f "$SRC/pms.wksc" ] || { echo "sync-pms: $SRC is not a PMS checkout (no pms.wksc)" >&2; exit 1; }
rm -rf examples/pms
mkdir -p examples/pms
(cd "$SRC" && git ls-files -z) | (cd "$SRC" && xargs -0 tar -cf - --) | tar -xf - -C examples/pms
echo "sync-pms: examples/pms = $(git -C "$SRC" log --oneline -1)"
