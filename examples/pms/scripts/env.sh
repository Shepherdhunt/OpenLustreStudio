# Sourced by the other scripts: find `openlustre` and the prover.
# Order: $OPENLUSTRE, the project-local install (.openlustre/bin), PATH.
if [ -z "${OPENLUSTRE:-}" ]; then
    if [ -x .openlustre/bin/openlustre ]; then
        OPENLUSTRE="$PWD/.openlustre/bin/openlustre"
    elif command -v openlustre > /dev/null 2>&1; then
        OPENLUSTRE=$(command -v openlustre)
    else
        echo "openlustre not found: run scripts/install-openlustre.sh (or set OPENLUSTRE)" >&2
        exit 2
    fi
fi
if [ -z "${OPENLUSTRE_TOOLS:-}" ] && [ -d .openlustre/tools ]; then
    OPENLUSTRE_TOOLS="$PWD/.openlustre/tools"
    export OPENLUSTRE_TOOLS
fi
