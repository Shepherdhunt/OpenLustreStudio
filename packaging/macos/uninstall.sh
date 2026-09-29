#!/bin/sh
# Remove OpenLustre Studio installed from the .pkg:
#
#   sudo "/Applications/OpenLustre Studio/OpenLustre Studio.app/Contents/Resources/uninstall.sh"
#
# Your projects and ~/OpenLustre are left alone.
set -eu
rm -rf "/Applications/OpenLustre Studio"
[ -L /usr/local/bin/openlustre ] && rm -f /usr/local/bin/openlustre
pkgutil --forget io.github.shepherdhunt.openlustre-studio > /dev/null 2>&1 || true
echo "uninstalled OpenLustre Studio"
