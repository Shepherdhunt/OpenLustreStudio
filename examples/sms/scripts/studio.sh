#!/bin/sh
# Open the SMS in OpenLustre Studio (in the browser).
#
#   scripts/studio.sh
set -eu
cd "$(dirname "$0")/.."
. scripts/env.sh
exec "$OPENLUSTRE" studio launch .
