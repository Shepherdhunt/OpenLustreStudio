#!/bin/sh
# Open the PMS in OpenLustre Studio (in the browser).
#
#   scripts/studio.sh
set -eu
cd "$(dirname "$0")/.."
. scripts/env.sh
exec "$OPENLUSTRE" studio launch .
