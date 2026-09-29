# Checks shared by the Linux and macOS smoke tests (sourced; WORK is a
# scratch folder): an installed copy runs, finds its bundled Kind 2, opens
# and checks the PMS sample, runs its scenarios on the model and the
# compiled C, proves a sample, and serves the Studio.

check_installed() {  # openlustre command, label, installed examples folder
    OL="$1"
    echo "== $2: $("$OL" --version)"
    # A clean home: the Kind 2 found must be the bundled one.
    mkdir -p "$WORK/home"
    HOME="$WORK/home" env -u OPENLUSTRE_TOOLS -u OPENLUSTRE_KIND2 -u OPENLUSTRE_Z3 "$OL" kind2 doctor | tee "$WORK/doctor.txt"
    grep -q "via bundled" "$WORK/doctor.txt"
    HOME="$WORK/home" "$OL" studio launch --sample pms --no-open --port 8471 > "$WORK/serve.log" 2>&1 &
    PID=$!
    for _ in $(seq 1 50); do
        curl -fs http://127.0.0.1:8471/api/inspect > /dev/null 2>&1 && break
        sleep 0.2
    done
    curl -fs http://127.0.0.1:8471/ | grep -q "OpenLustre Studio" || { cat "$WORK/serve.log"; exit 1; }
    kill $PID
    PMS="$WORK/home/OpenLustre/samples/pms"
    "$OL" check "$PMS/pms.wksc"
    "$OL" test run "$PMS/pms.wksc" --scenarios "$PMS/scenarios" | tail -3
    # The installed (read-only) sample proves in place.
    HOME="$WORK/home" env -u OPENLUSTRE_TOOLS "$OL" prove "$3/release_logic/model/release_logic.json" --timeout 120 | tail -1
    rm -rf "$WORK/home"
}
