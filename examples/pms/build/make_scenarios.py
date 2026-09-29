#!/usr/bin/env python3
"""Write the PMS test scenarios (../scenarios/*.csv) from annotated steps.

Each scenario is a list of steps; a step changes some inputs (the rest keep
their previous values) and repeats for `n` cycles. The comments say what the
step exercises (requirement ids from ../PLAN.md). Goldens are then recorded
with `openlustre test record` and reviewed.
"""

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "..", "scenarios")

COLUMNS = ["master_arm", "wow", "alt_m", "release_req", "req_kind", "jettison_req", "maint_reset",
           "present1", "id1", "present2", "id2", "present3", "id3", "present4", "id4"]

GROUND = dict(master_arm=False, wow=True, alt_m=0, release_req=False, req_kind="MedKit",
              jettison_req=False, maint_reset=False)


def loadout(*stations):
    """(present, id) per station."""
    out = {}
    for k, (present, code) in enumerate(stations, start=1):
        out[f"present{k}"] = present
        out[f"id{k}"] = code
    return out


def gone(*ks):
    """The stores on stations `ks` have left: hook empty, no tag."""
    out = {}
    for k in ks:
        out[f"present{k}"] = False
        out[f"id{k}"] = 0
    return out


def render(v):
    return ("true" if v else "false") if isinstance(v, bool) else str(v)


def write(name, start, steps):
    state = dict(start)
    rows = []
    for step in steps:
        n = step.pop("n", 1) if isinstance(step, dict) else 1
        state.update(step)
        rows.extend([dict(state)] * n)
    with open(os.path.join(OUT, name + ".csv"), "w") as f:
        f.write(",".join(COLUMNS) + "\n")
        for r in rows:
            f.write(",".join(render(r[c]) for c in COLUMNS) + "\n")
    print(f"{name}: {len(rows)} cycles")


MEDKIT, SENSORPOD, WATERPACK, SUPPLYCRATE = 1, 2, 3, 4
EMPTY = (False, 0)

# INV-1..4, BAL-1: loading on the ground, one station at a time. Tags are
# often read before the store is hung: that is a Mismatch until it is.
write("inventory_loading", {**GROUND, **loadout(EMPTY, EMPTY, EMPTY, EMPTY)}, [
    {},                                              # all Vacant, 0 g, balanced
    {"present1": True},                              # hook 1 loaded, tag not read: Mismatch
    {"id1": MEDKIT},                                 # MedKit 1200 g, roll -240 000
    {"id2": MEDKIT},                                 # tag on 2 before its store: Mismatch
    {"present2": True},                              # front pair: roll 0, pitch 360 000
    {"id3": WATERPACK, "req_kind": "WaterPack"},     # tag without a store on 3: Mismatch
    {"present3": True},                              # WaterPack on 3: roll -300 000, unbalanced
    {"present4": True, "id4": 9},                    # unknown tag: Unknown, Mismatch, 0 g
    {"present4": False},                             # store off again, the tag stays
    {"id4": WATERPACK},                              # WaterPack tag, no store yet
    {"present4": True},                              # balanced again: roll 0, pitch -90 000
    {"master_arm": True},                            # armed on the ground: OnGround
    {"release_req": True, "n": 2},                   # REL-1: no release on the ground
    {"release_req": False},
    {"id2": SENSORPOD},                              # retag 2 as a SensorPod (800 g)
    {"id1": SUPPLYCRATE},                            # 1 as a SupplyCrate (2000 g)
    {"id2": SUPPLYCRATE},                            # crates in front: pitch 150 000
    gone(3, 4),                                      # rear unloaded: pitch 600 000 —
    {"master_arm": False},                           #   unbalanced by pitch alone
])

# BAL-2, BAL-3, REL-9: selective releases that keep the vehicle balanced;
# the planner refuses one that would not, and the order it allows next.
SYM = loadout((True, MEDKIT), (True, MEDKIT), (True, WATERPACK), (True, WATERPACK))
write("balanced_release", {**GROUND, **SYM}, [
    {},                                              # symmetric loadout, balanced
    {"wow": False, "alt_m": 5},                      # airborne, disarmed: NotArmed
    {"master_arm": True},                            # armed, too low: LowAltitude
    {"alt_m": 20},                                   # Clear: MedKit from 1 (tie → lowest)
    {"release_req": True, "n": 2},                   # accepted once, though held
    {"release_req": False},                          # Firing: fire1
    gone(1),                                         # store 1 leaves on the last pulse
    {"n": 2},                                        # Verify sees it gone → Ready
    {"release_req": True},                           # MedKit from 2 would leave it
    {"release_req": False},                          #   rear-heavy: WouldUnbalance
    {"req_kind": "WaterPack"},                       # WaterPack: 4, not 3 (roll)
    {"release_req": True},
    {"release_req": False},                          # fire4
    gone(4),
    {"n": 3},
    {"req_kind": "MedKit"},                          # MedKit from 2 would now roll
    {"release_req": True},                           #   too far: WouldUnbalance
    {"release_req": False, "req_kind": "WaterPack"}, # so WaterPack from 3 first
    {"release_req": True},
    {"release_req": False},                          # fire3
    gone(3),
    {"n": 3},
    {"req_kind": "MedKit"},                          # now MedKit from 2 is fine
    {"release_req": True},
    {"release_req": False},                          # fire2
    gone(2),
    {"n": 3},                                        # empty, balanced
    {"release_req": True, "req_kind": "SensorPod"},  # nothing left: NoMatchingStore
    {"release_req": False},
    {"master_arm": False},                           # disarm → Safe
])

# BAL-3: the planner releases the store that leaves the least imbalance —
# not simply the lowest station. (next1..4 show its choice every cycle.)
CHOICE = loadout((True, MEDKIT), (True, SENSORPOD), (True, MEDKIT), (True, WATERPACK))
write("planner_choice", {**GROUND, **CHOICE}, [
    {},                                              # MedKit: 3 leaves 295 000, 1 leaves 505 000
    {"id3": WATERPACK},                              # 3 is a WaterPack: only 1 carries a MedKit
    {"id2": WATERPACK, "id3": MEDKIT, "id4": SENSORPOD},  # now 1 is the better MedKit
    {"id2": SENSORPOD, "id4": WATERPACK},            # back to the first loadout
    {"wow": False, "alt_m": 30, "master_arm": True},
    {},                                              # Ready
    {"release_req": True},                           # release MedKit: station 3
    {"release_req": False, "n": 2},                  # fire3, fire3
    {"alt_m": 12},                                   # altitude lost: the pulse stops (REL-2)
    gone(3),                                         # the store fell on the second pulse
    {"alt_m": 30, "n": 2},                           # Verify → Ready
    {"master_arm": False, "wow": True, "alt_m": 0},
])

# BAL-4, BAL-6: two SupplyCrates in front — either alone would roll the
# vehicle over the limit, so the pair goes together.
PAIR = loadout((True, SUPPLYCRATE), (True, SUPPLYCRATE), (True, SENSORPOD), (True, SENSORPOD))
write("pair_release", {**GROUND, **PAIR}, [
    {},
    {"wow": False, "alt_m": 30, "master_arm": True},
    {},                                              # Ready
    {"release_req": True},                           # MedKit: NoMatchingStore
    {"release_req": False, "req_kind": "SupplyCrate"},  # SupplyCrate: front pair
    {"release_req": True},
    {"release_req": False},                          # fire1 and fire2 together
    gone(1, 2),
    {"n": 3},
    {"req_kind": "SensorPod"},                       # SensorPod from 3 (tie → lowest)
    {"release_req": True},
    {"release_req": False},
    gone(3),
    {"n": 3},
    {"release_req": True},                           # and the last one, from 4
    {"release_req": False},
    gone(4),
    {"n": 3},
    {"master_arm": False, "wow": True, "alt_m": 0},  # land
])

# BAL-4: the rear pair, when neither WaterPack may go alone.
write("rear_pair", {**GROUND, **SYM}, [
    {"wow": False, "alt_m": 30, "master_arm": True, "req_kind": "SupplyCrate"},
    {},                                              # Ready; no SupplyCrate aboard
    {"req_kind": "WaterPack"},                       # WaterPack: the rear pair together
    {"release_req": True},
    {"release_req": False},                          # fire3 and fire4
    gone(3, 4),
    {"n": 3},
    {"master_arm": False},
])

# REL-5, REL-7: a store that does not leave is latched Hung and skipped;
# jettison retries it; only ground maintenance clears the latch.
PODS = loadout((True, SENSORPOD), (True, SENSORPOD), (True, SENSORPOD), (True, SENSORPOD))
write("hung_store", {**GROUND, **PODS, "req_kind": "SensorPod"}, [
    {},
    {"wow": False, "alt_m": 25, "master_arm": True},
    {},                                              # Ready
    {"release_req": True},                           # SensorPod from 1
    {"release_req": False, "n": 3},                  # fire1 ×3 — the store stays
    {"n": 5},                                        # Verify times out → Hung
    {"n": 2},                                        # status1 = Hung
    {"maint_reset": True},                           # reset in flight: ignored
    {"maint_reset": False},
    {"release_req": True},                           # next SensorPod: 2, not 1
    {"release_req": False},
    gone(2),
    {"n": 4},
    {"jettison_req": True},                          # jettison: 1 (hung), 3 and 4
    {"jettison_req": False},
    gone(1, 3, 4),
    {"n": 3},
    {"master_arm": False, "wow": True, "alt_m": 0},  # land; 1 still reads Hung
    {"maint_reset": True},                           # ground crew clears the latch
    {"maint_reset": False, "n": 2},
])

# REL-1, REL-2, REL-6, REL-8, REL-9: every interlock refuses in turn; losing
# the master arm cuts a pulse in the same cycle; a request made while a
# release is in progress is ignored.
write("interlocks", {**GROUND, **SYM}, [
    {"release_req": True},                           # NotArmed
    {"release_req": False},
    {"master_arm": True},                            # OnGround
    {"release_req": True},
    {"release_req": False, "wow": False, "alt_m": 8},  # LowAltitude
    {"release_req": True},
    {"release_req": False},
    {"alt_m": 30},                                   # Clear
    {"present4": False},                             # tag without a store: StationFault
    {"release_req": True},
    {"release_req": False, "present4": True},        # fault cleared
    {"release_req": True},                           # accepted: MedKit from 1
    {"release_req": False},                          # fire1 …
    {"master_arm": False},                           # … cut at once (REL-8)
    {},                                              # Safe
    {"master_arm": True},                            # re-armed
    {"n": 2},                                        # Ready
    {"release_req": True},                           # accepted again: 1
    {"release_req": False},                          # fire1
    {"release_req": True},                           # ignored while firing (REL-9)
    {"release_req": False},                          # fire1 (third pulse)
    gone(1),
    {"n": 3},
    {"master_arm": False},
])

# REL-7, REL-2, REL-8: emergency jettison below the release altitude fires
# every occupied station at once; it needs the arm, and losing it stops it.
MIXED = loadout((True, MEDKIT), (True, SUPPLYCRATE), EMPTY, (True, WATERPACK))
write("jettison", {**GROUND, **MIXED}, [
    {"wow": False, "alt_m": 8},                      # airborne, disarmed
    {"jettison_req": True},                          # refused: not armed (Safe)
    {"jettison_req": False, "master_arm": True},     # armed
    {},                                              # Ready; low: selective inhibited
    {"release_req": True},                           # refused: LowAltitude
    {"release_req": False, "jettison_req": True, "n": 2},  # jettison, held down
    {"jettison_req": False},                         # fire1, fire2, fire4 (not 3)
    {"master_arm": False},                           # arm lost: the pulse stops (REL-8)
    {"master_arm": True},                            # → Safe; re-armed
    gone(1, 2, 4),                                   # the two pulses were enough
    {"n": 3},                                        # Ready, nothing aboard
    {"master_arm": False},
])
