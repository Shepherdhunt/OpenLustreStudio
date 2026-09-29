#!/bin/sh
# Export examples/sms as a repository of its own, with its history.
#
#   tools/export-sms.sh <url-of-a-new-empty-repository> [branch]
#   tools/export-sms.sh --dir <folder>            (a local repository instead)
#
# The SMS is an OpenLustre Studio project like any other: it pins the tool
# version it is built with (OPENLUSTRE_VERSION), installs it
# (scripts/install-openlustre.sh), verifies itself (scripts/verify.sh) and
# runs that in its own CI (.github/workflows/verify.yml). `git subtree split`
# rewrites the history of examples/sms into a branch whose root is the
# project folder, so the new repository keeps every commit that shaped it.
# examples/sms stays here as a regression test of the tool.

set -eu
cd "$(dirname "$0")/.."
BRANCH_LOCAL=sms-export
git subtree split --prefix=examples/sms -b "$BRANCH_LOCAL" > /dev/null
echo "export: branch $BRANCH_LOCAL holds examples/sms as a project root ($(git rev-list --count "$BRANCH_LOCAL") commits)"
if [ "${1:-}" = "--dir" ]; then
    DIR="$2"
    git init -q "$DIR"
    git -C "$DIR" pull -q "$PWD" "$BRANCH_LOCAL"
    echo "export: $DIR is the SMS repository"
else
    URL="${1:?usage: tools/export-sms.sh <repository-url> [branch] | --dir <folder>}"
    git push "$URL" "$BRANCH_LOCAL:${2:-main}"
    echo "export: pushed to $URL (${2:-main})"
fi
