#!/bin/sh
# Build and verify the PMS, end to end — what CI runs on every push.
#
#   scripts/verify.sh            everything, the Kind 2 proof included
#   scripts/verify.sh --quick    without Kind 2 (checks, tests, code, demo)
#
# 1. static checks (types, clocks, contracts);
# 2. the scenarios on the model and on the compiled generated C, with
#    decision and MC/DC coverage;
# 3. the flight code: C generated for the PMS root into out/code, compiled
#    with the integration in integration/, and the scripted mission run
#    and compared with integration/expected_mission.txt;
# 4. the evidence report (out/evidence): with the Kind 2 proof of every
#    contract and of the absence of runtime errors; exits non-zero unless
#    the verdict is PASS.
#
# Uses the OpenLustre Studio of scripts/install-openlustre.sh (or
# $OPENLUSTRE, or `openlustre` on PATH).

set -eu
cd "$(dirname "$0")/.."
. scripts/env.sh
QUICK=0
[ "${1:-}" = "--quick" ] && QUICK=1
TIMEOUT="${PROVE_TIMEOUT:-600}"
CC="${CC:-cc}"
mkdir -p out

echo "== 1. check"
"$OPENLUSTRE" check pms.wksc

echo "== 2. scenarios on the model and the generated C"
"$OPENLUSTRE" test run pms.wksc --scenarios scenarios

echo "== 3. flight code"
rm -rf out/code
"$OPENLUSTRE" emit-clite pms.wksc --root PMS --out out/code
"$CC" -std=c11 -O2 -Wall -Wextra -Wno-unused-but-set-variable -Wno-unused-variable \
    -I out/code/clite -I integration \
    out/code/clite/openlustre_generated.c integration/pms_task.c integration/mission_sim.c \
    -o out/pms_mission
./out/pms_mission > out/mission.txt
if diff -u integration/expected_mission.txt out/mission.txt; then
    echo "mission: matches integration/expected_mission.txt"
else
    echo "!! the mission log changed (see the diff above)"
    exit 1
fi

if [ "$QUICK" = 1 ]; then
    echo "== 4. evidence (without the proof: --quick)"
    "$OPENLUSTRE" evidence pms.wksc --scenarios scenarios --out out/evidence || true
    exit 0
fi
echo "== 4. evidence, with the Kind 2 proof"
"$OPENLUSTRE" kind2 doctor
"$OPENLUSTRE" evidence pms.wksc --scenarios scenarios --prove --timeout "$TIMEOUT" --out out/evidence
