#!/usr/bin/env python3
"""Build the PMS workspace in OpenLustre Studio, the way an engineer would.

Replays the modelling steps of ../PLAN.md against a running Studio server,
through the same HTTP endpoints the Studio's dialogs use:

  1. `openlustre new` — an empty workspace (pms.wksc, types.json, scenarios/);
  2. Import Lustre   — types, constants, functions and operators from
                       pms_source.lus;
  3. State machine   — `Sequencer`, owned by ReleaseSequencer;
  4. Activation      — `Inhibit`, owned by PMS (if / elsif / else);
  5. Contracts       — one per operator, clause names = requirement ids;
  6. Build           — every operator (its Lustre projection is written);
  7. Root            — PMS.

    python3 build_pms.py [path/to/openlustre] [workspace-dir]

`openlustre` defaults to $OPENLUSTRE, then the project-local install
(scripts/install-openlustre.sh), then PATH. Refuses to overwrite an existing
workspace. The workspace files are the
source of truth afterwards: edit them in the Studio, not here.
"""

import json
import os
import shutil
import subprocess
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
def find_openlustre():
    local = os.path.join(HERE, "..", ".openlustre", "bin", "openlustre")
    return os.environ.get("OPENLUSTRE") or (local if os.path.exists(local) else shutil.which("openlustre"))


OL = sys.argv[1] if len(sys.argv) > 1 else find_openlustre()
if not OL:
    raise SystemExit("openlustre not found: run scripts/install-openlustre.sh, or pass its path")
WS = sys.argv[2] if len(sys.argv) > 2 else os.path.join(HERE, "..")


def post(port, path, body):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}", data=json.dumps(body).encode(), method="POST"
    )
    try:
        with urllib.request.urlopen(req) as r:
            text = r.read().decode()
    except urllib.error.HTTPError as e:
        text = e.read().decode()
        raise SystemExit(f"{path} failed ({e.code}): {text}")
    out = json.loads(text) if text else {}
    if isinstance(out, dict) and (out.get("ok") is False or out.get("error")):
        raise SystemExit(f"{path} failed: {text}")
    return out


def ports(*pairs):
    return [{"name": n, "type": t} for n, t in pairs]


def eqs(*pairs):
    return [{"lhs": l, "body": b} for l, b in pairs]


def clauses(*pairs):
    return [{"name": n, "expr": e} for n, e in pairs]


# --- The Sequencer state machine (REL-4, REL-9) ------------------------------

def state(name, phase, pulse, jett, transitions):
    return {
        "name": name,
        "equations": eqs(("phase", phase), ("pulse", pulse), ("jett", jett)),
        "transitions": [{"guard": g, "target": t} for g, t in transitions],
    }


SEQUENCER = {
    "name": "Sequencer",
    "operator": "ReleaseSequencer",
    "inputs": ports(("ready_ok", "bool"), ("fire_req", "bool"), ("jettison_req", "bool"),
                    ("cnt", "int32"), ("clear", "bool"), ("timeout", "bool")),
    "outputs": ports(("phase", "SeqPhase"), ("pulse", "bool"), ("jett", "bool")),
    "initial_state": "Safe",
    "states": [
        state("Safe", "PhSafe", "false", "false", [("ready_ok", "Ready")]),
        state("Ready", "PhReady", "false", "false",
              [("not ready_ok", "Safe"), ("jettison_req", "Jettison"), ("fire_req", "Firing")]),
        state("Firing", "PhFiring", "true", "false",
              [("not ready_ok", "Safe"), ("cnt >= PULSE_CYCLES - 1", "Verify")]),
        state("Jettison", "PhJettison", "true", "true",
              [("not ready_ok", "Safe"), ("cnt >= PULSE_CYCLES - 1", "Verify")]),
        state("Verify", "PhVerify", "false", "false", [("clear", "Ready"), ("timeout", "Ready")]),
    ],
}

# --- The Inhibit decision tree (REL-6) ---------------------------------------

INHIBIT = {
    "name": "Inhibit",
    "operator": "PMS",
    "outputs": ports(("inhibit", "Inhibit")),
    "branches": [
        {"name": name, "condition": cond, "equations": eqs(("inhibit", value))}
        for name, cond, value in [
            ("Disarmed", "not master_arm", "NotArmed"),
            ("Grounded", "wow", "OnGround"),
            ("Fault", "fault_any", "StationFault"),
            ("TooLow", "not alt_ok", "LowAltitude"),
            ("NoStore", "not have_kind", "NoMatchingStore"),
            ("Unbalanced", "not found", "WouldUnbalance"),
        ]
    ],
    "else": {"equations": eqs(("inhibit", "Clear"))},
}

# --- Contracts: clause names are the requirement ids of PLAN.md --------------

MASS_RANGE = " and ".join(f"m{k} >= 0 and m{k} <= MASS_SUPPLYCRATE" for k in range(1, 5))


def within(r, p):
    return (f"{r} <= ROLL_LIMIT and - ({r}) <= ROLL_LIMIT and "
            f"{p} <= PITCH_LIMIT and - ({p}) <= PITCH_LIMIT")


# Moment contribution of station k (roll, pitch): 1 front-left, 2 front-right,
# 3 rear-left, 4 rear-right.
SIGN = {1: ("-", ""), 2: ("", ""), 3: ("-", "-"), 4: ("", "-")}

CONTRACTS = [
    {
        "name": "StationDecode_contract", "operator": "StationDecode",
        "guarantees": clauses(
            ("INV_1_catalogue",
             "(id = 0 => kind = Empty) and (id = 1 => kind = MedKit) and (id = 2 => kind = SensorPod) "
             "and (id = 3 => kind = WaterPack) and (id = 4 => kind = SupplyCrate) "
             "and ((id < 0 or id > 4) => kind = Unknown)"),
            ("INV_2_loaded", "status = Loaded => present and not hung and kind <> Empty and kind <> Unknown"),
            ("INV_2_vacant", "status = Vacant => not present and kind = Empty"),
            ("INV_2_hung", "hung => status = Hung"),
            ("INV_3_mass",
             "(not present => mass = 0) and (status = Loaded => mass >= MASS_SENSORPOD and mass <= MASS_SUPPLYCRATE) "
             "and mass >= 0 and mass <= MASS_SUPPLYCRATE"),
        ),
    },
    {
        "name": "Abs_contract", "operator": "Abs",
        "guarantees": clauses(("nonnegative", "y >= 0"), ("magnitude", "y = x or y = - x")),
    },
    {
        "name": "Balance_contract", "operator": "Balance",
        "assumptions": clauses(("masses_in_catalogue_range", MASS_RANGE)),
        "guarantees": clauses(
            ("INV_4_total", "total = m1 + m2 + m3 + m4 and total >= 0 and total <= 4 * MASS_SUPPLYCRATE"),
            ("INV_4_symmetric", "(m1 = m2 and m3 = m4) => roll = 0"),
            ("BAL_1_limits", f"balanced = ({within('roll', 'pitch')})"),
        ),
    },
    {
        "name": "Candidate_contract", "operator": "Candidate",
        "guarantees": clauses(
            ("only_eligible", "ok => eligible"),
            ("keeps_limits", f"ok => ({within('roll - droll', 'pitch - dpitch')})"),
            ("score_nonnegative", "score >= 0"),
        ),
    },
    {
        "name": "Best_contract", "operator": "Best",
        "guarantees": clauses(
            ("feasible_if_either", "ok = (oka or okb)"),
            ("picks_a_feasible_one", "(ok and i = ia => oka) and (ok and i <> ia => okb and i = ib)"),
            ("none_is_zero", "not ok => i = 0"),
        ),
    },
    {
        "name": "PlanRelease_contract", "operator": "PlanRelease",
        "assumptions": clauses(("masses_in_catalogue_range", MASS_RANGE)),
        "ghosts": [
            {"name": "dr", "type": "int32",
             "expr": "ARM_Y * ((if s2 then m2 else 0) + (if s4 then m4 else 0) "
                     "- (if s1 then m1 else 0) - (if s3 then m3 else 0))"},
            {"name": "dp", "type": "int32",
             "expr": "ARM_X * ((if s1 then m1 else 0) + (if s2 then m2 else 0) "
                     "- (if s3 then m3 else 0) - (if s4 then m4 else 0))"},
        ] + [
            {"name": f"fits{k}", "type": "bool",
             "expr": f"l{k} and k{k} = req and "
                     + within(f"roll - {SIGN[k][0]}ARM_Y * m{k}", f"pitch - {SIGN[k][1]}ARM_X * m{k}")}
            for k in range(1, 5)
        ],
        "guarantees": clauses(
            ("BAL_2_stays_balanced", f"found => ({within('roll - dr', 'pitch - dp')})"),
            ("BAL_5_complete", "fits1 or fits2 or fits3 or fits4 => found"),
            ("BAL_6_requested_only",
             " and ".join(f"(s{k} => l{k} and k{k} = req)" for k in range(1, 5))),
            ("BAL_6_lateral", "not ((s1 or s2) and (s3 or s4))"),
            ("found_iff_selected", "found = (s1 or s2 or s3 or s4)"),
            ("have_kind_def",
             "have_kind = (l1 and k1 = req or l2 and k2 = req or l3 and k3 = req or l4 and k4 = req)"),
        ),
    },
    {
        "name": "ReleaseSequencer_contract", "operator": "ReleaseSequencer",
        "guarantees": clauses(
            ("REL_1_ready", "(fire1 or fire2 or fire3 or fire4) => ready_ok"),
            ("REL_2_altitude", "(fire1 or fire2 or fire3 or fire4) and phase <> PhJettison => alt_ok"),
            ("REL_9_fires_only_when_releasing",
             "(fire1 or fire2 or fire3 or fire4) => phase = PhFiring or phase = PhJettison"),
        ),
    },
    {
        "name": "PMS_contract", "operator": "PMS",
        "ghosts": [
            {"name": "fire_any", "type": "bool", "expr": "fire1 or fire2 or fire3 or fire4"},
        ] + [
            {"name": f"run{k}", "type": "int32", "expr": f"if fire{k} then (1 -> pre run{k} + 1) else 0"}
            for k in range(1, 5)
        ],
        "guarantees": clauses(
            ("REL_1_armed_airborne", "fire_any => master_arm and not wow"),
            ("REL_2_min_altitude", "fire_any and phase <> PhJettison => alt_m >= MIN_RELEASE_ALT"),
            ("REL_3_lateral_pairs", "phase <> PhJettison => not ((fire1 or fire2) and (fire3 or fire4))"),
            ("REL_4_pulse_bounded",
             " and ".join(f"run{k} <= PULSE_CYCLES" for k in range(1, 5))),
            ("REL_5_no_refire_hung",
             "phase <> PhJettison => " + " and ".join(
                 f"not (fire{k} and status{k} = Hung)" for k in range(1, 5))),
            ("REL_6_clear_means_safe", "inhibit = Clear => master_arm and not wow and alt_m >= MIN_RELEASE_ALT"),
            ("BAL_6_next_is_loaded_request",
             " and ".join(f"(next{k} => status{k} = Loaded and kind{k} = req_kind)" for k in range(1, 5))),
            ("BAL_6_next_lateral", "not ((next1 or next2) and (next3 or next4))"),
            ("BAL_1_reported", f"balanced = ({within('roll', 'pitch')})"),
        ),
        "modes": [
            {"name": "Grounded", "requires": ["wow"],
             "ensures": ["not fire_any", "inhibit = NotArmed or inhibit = OnGround"]},
            {"name": "Disarmed", "requires": ["not wow", "not master_arm"],
             "ensures": ["not fire_any", "inhibit = NotArmed"]},
            {"name": "ArmedAirborne", "requires": ["not wow", "master_arm"],
             "ensures": ["inhibit <> NotArmed and inhibit <> OnGround"]},
        ],
    },
]


# --- The root diagram, arranged by hand (what dragging blocks saves) --------
#
# Commands and flight state top-left, the four stations below them; station
# decoding, then balance and planning; the inhibit decision tree and the
# release sequencer; outputs grouped release / plan / balance / inventory.

def column(x, top, pitch, ids):
    return {i: {"x": x, "y": top + k * pitch} for k, i in enumerate(ids)}


PMS_LAYOUT = {
    **column(16, 16, 44, ["master_arm", "wow", "alt_m", "release_req", "jettison_req", "req_kind", "maint_reset"]),
    **column(16, 380, 60, ["present1", "id1"]),
    **column(16, 500, 60, ["present2", "id2"]),
    **column(16, 620, 60, ["present3", "id3"]),
    **column(16, 740, 60, ["present4", "id4"]),
    "eq10": {"x": 250, "y": 30}, "eq11": {"x": 250, "y": 96},
    "eq13": {"x": 250, "y": 148}, "eq14": {"x": 250, "y": 192},
    **{f"eq{k - 1}": {"x": 250, "y": 320 + 120 * k} for k in range(1, 5)},
    **{f"eq{k + 3}": {"x": 400, "y": 262 + 120 * k} for k in range(1, 5)},
    **{f"m{k}": {"x": 560, "y": 290 + 120 * k} for k in range(1, 5)},
    "eq8": {"x": 780, "y": 400}, "eq12": {"x": 780, "y": 500}, "eq9": {"x": 780, "y": 600},
    "found": {"x": 920, "y": 640}, "have_kind": {"x": 920, "y": 690},
    "act:Inhibit": {"x": 1100, "y": 24}, "eq15": {"x": 1100, "y": 170}, "eq16": {"x": 1100, "y": 230},
    **column(1100, 500, 44, ["hung1", "hung2", "hung3", "hung4"]),
    **column(1420, 24, 44, ["inhibit", "phase", "fire1", "fire2", "fire3", "fire4"]),
    **column(1420, 310, 44, ["next1", "next2", "next3", "next4"]),
    **column(1420, 510, 44, ["total_mass", "roll", "pitch", "balanced"]),
    **column(1420, 700, 44, ["kind1", "status1", "kind2", "status2", "kind3", "status3", "kind4", "status4"]),
}


# The Sequencer chart: the release flow left to right, the two pulse states
# above and below, back-arcs to Safe around the outside.
SEQUENCER_LAYOUT = {
    "Safe": {"x": 80, "y": 200}, "Ready": {"x": 280, "y": 200},
    "Firing": {"x": 480, "y": 80}, "Jettison": {"x": 480, "y": 320},
    "Verify": {"x": 680, "y": 200},
}


def main():
    wksc = os.path.join(WS, "pms.wksc")
    if os.path.exists(wksc):
        raise SystemExit(f"{wksc} exists — build into a fresh folder")
    subprocess.run([OL, "new", WS, "--empty"], check=True)
    # `new` names the workspace file after the folder.
    made = [f for f in os.listdir(WS) if f.endswith(".wksc")]
    if made != ["pms.wksc"]:
        os.rename(os.path.join(WS, made[0]), wksc)

    server = subprocess.Popen(
        [OL, "studio", "serve", wksc, "--port", "0"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    try:
        port = None
        for line in server.stdout:
            if "http://127.0.0.1:" in line:
                digits = line.split("http://127.0.0.1:")[1]
                port = int("".join(c for c in digits[:6] if c.isdigit()) or 0)
                break
        if port is None:
            raise SystemExit("the Studio server did not start")

        post(port, "/api/edit/set_project_name", {"name": "PMS"})
        with open(os.path.join(HERE, "pms_source.lus")) as f:
            post(port, "/api/edit/import_lustre", {"lustre": f.read()})
        post(port, "/api/edit/add_state_machine", SEQUENCER)
        post(port, "/api/edit/add_activation", INHIBIT)
        for c in CONTRACTS:
            post(port, "/api/edit/add_contract", c)
        # Build every operator (writes its Lustre projection next to the
        # model, as Build Model does in the Studio), the root last.
        for op in ["StationDecode", "Abs", "Balance", "Candidate", "Best",
                   "PlanRelease", "ReleaseSequencer", "PMS"]:
            post(port, "/api/build", {"node": op})
        post(port, "/api/edit/set_main", {"main": "PMS"})
        post(port, "/api/edit/set_layout", {"node": "PMS", "positions": PMS_LAYOUT, "grid": 8})
        post(port, "/api/edit/set_fsm_layout", {"machine": "Sequencer", "positions": SEQUENCER_LAYOUT})
        print(f"built {wksc}")
    finally:
        server.terminate()
        server.wait()


if __name__ == "__main__":
    main()
