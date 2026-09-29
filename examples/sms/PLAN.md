# SMS — Stores Management System: implementation plan

The Stores Management System (SMS) runs on a multirotor drone that carries up
to four stores (payload items) on release hooks under its frame. It tells the
operator **what is loaded where**, keeps the vehicle **balanced** as stores
leave, and **drops stores on command** — only when it is safe to. This plan
is the specification the OpenLustre Studio project in this folder implements;
every requirement below names the operator that implements it and the
evidence that verifies it.

> This is a reference design for demonstrating model-based development in
> OpenLustre Studio. It is not a certified flight system.

## 1. System context

```
                  ground control station                      drone
   ┌──────────────────────────────────────┐        ┌──────────────────────────┐
   │ master_arm  release_req  req_kind    │ ─────▶ │                          │
   │ jettison_req  maint_reset            │        │           SMS            │ ──▶ fire1..4 (hook actuators)
   └──────────────────────────────────────┘        │                          │
   flight controller:  wow  alt_m           ─────▶ │                          │ ──▶ kind1..4, status1..4,
   stations 1..4:      present_k  id_k      ─────▶ │                          │     total_mass, roll, pitch,
                                                   └──────────────────────────┘     balanced, next1..4,
                                                                                    phase, inhibit
```

**Stations.** Four hooks on a 2 × 2 grid around the centre of gravity (x
forward, y right, millimetres):

| station | position    | x (mm) | y (mm) |
|---------|-------------|-------:|-------:|
| 1       | front-left  | +150   | −200   |
| 2       | front-right | +150   | +200   |
| 3       | rear-left   | −150   | −200   |
| 4       | rear-right  | −150   | +200   |

Each station reports `present_k` (the hook's load switch: a store hangs on
it) and `id_k`, the code read from the store's identification tag.

**Store catalogue.**

| id | kind          | mass (g) |
|---:|---------------|---------:|
| 0  | `Empty`       | 0        |
| 1  | `MedKit`      | 1200     |
| 2  | `SensorPod`   | 800      |
| 3  | `WaterPack`   | 1500     |
| 4  | `SupplyCrate` | 2000     |
| other | `Unknown`  | —        |

**Other inputs.** `master_arm` (operator's arm switch), `wow` (weight on
skids: the vehicle is on the ground), `alt_m` (height above ground, m),
`release_req` (momentary release button), `req_kind` (which kind of item to
drop), `jettison_req` (momentary emergency jettison), `maint_reset`
(ground crew clears hung-store latches).

**Timing.** The SMS runs at a fixed cycle (e.g. 20 Hz). A release pulse is
`PULSE_CYCLES = 3` cycles; the store must leave within `VERIFY_CYCLES = 5`
cycles after the pulse, or the station is declared hung.

## 2. Requirements

Identifiers are used verbatim as contract clause names, so the proof results
and the evidence report trace straight back here.

### Inventory — what is in the payload

| id | requirement | implemented in | verified by |
|----|-------------|----------------|-------------|
| INV-1 | Each station's store kind is decoded from its id code per the catalogue; an unrecognised code decodes to `Unknown`. | `StationDecode` | proof (`StationDecode_contract`), test `inventory_loading` |
| INV-2 | A station is `Loaded` only when its hook is loaded and its tag names a catalogued kind; `Vacant` when it is empty and untagged; `Mismatch` otherwise (tag without store, store without a readable tag); `Hung` once a release failed. | `StationDecode` | proof, test `inventory_loading` |
| INV-3 | A station's mass is the catalogue mass of the store on its hook, and 0 when the hook is empty. | `StationDecode` | proof |
| INV-4 | The SMS reports the total payload mass and the roll and pitch moments about the centre of gravity (g·mm). | `Balance` | proof (`Balance_contract`), tests |

### Balance — keeping the vehicle balanced

| id | requirement | implemented in | verified by |
|----|-------------|----------------|-------------|
| BAL-1 | The loadout is `balanced` when \|roll\| ≤ `ROLL_LIMIT` (250 000 g·mm) and \|pitch\| ≤ `PITCH_LIMIT` (400 000 g·mm). | `Balance` | proof |
| BAL-2 | A selective release is planned only if the vehicle stays balanced once the planned stores are gone. | `PlanRelease` | proof (`PlanRelease_contract`), test `balanced_release` |
| BAL-3 | Among the stations holding the requested kind, the plan releases the one that leaves the least imbalance (lowest station number on a tie). The plan is shown before release as `next1..4`. | `PlanRelease` | tests `planner_choice`, `balanced_release` |
| BAL-4 | When no single release keeps the balance, the plan releases a lateral pair of the requested kind together (front pair 1+2, else rear pair 3+4) if that keeps it. | `PlanRelease` | tests `pair_release`, `rear_pair` |
| BAL-5 | If some single station of the requested kind could be released keeping the balance, a plan is found (the planner is complete). | `PlanRelease` | proof |
| BAL-6 | A plan never selects a station that is not `Loaded` with the requested kind, and never front and rear stations together. | `PlanRelease` | proof (`PlanRelease_contract`, `SMS_contract`) |

### Release — dropping stores on command

| id | requirement | implemented in | verified by |
|----|-------------|----------------|-------------|
| REL-1 | No release actuator is ever commanded unless the master arm is on and the vehicle is airborne. | `ReleaseSequencer`, `SMS` | proof (`SMS_contract`), test `interlocks` |
| REL-2 | A selective release fires only at or above `MIN_RELEASE_ALT` (15 m) — a pulse stops if the vehicle sinks below it; an emergency jettison may fire at any height. | `ReleaseSequencer` | proof, tests `interlocks`, `planner_choice`, `jettison` |
| REL-3 | A selective release fires one station or one lateral pair, never front and rear together. | `ReleaseSequencer` | proof |
| REL-4 | A release pulse lasts at most `PULSE_CYCLES` consecutive cycles. | `Sequencer` state machine | proof |
| REL-5 | After the pulse, a station whose store did not leave within `VERIFY_CYCLES` is latched `Hung`; a hung station is never fired again by a selective release. Only `maint_reset` on the ground clears the latch. | `ReleaseSequencer` (latch), `PlanRelease` (never plans it) | proof, test `hung_store` |
| REL-6 | A release request is accepted only when nothing inhibits it; the SMS reports why it refuses (`inhibit`): not armed, on the ground, a station mismatch, too low, no such store, would unbalance. | `Inhibit` activation | proof (mode table), tests |
| REL-7 | Emergency jettison (armed, airborne) fires every occupied station at once, hung ones included. | `ReleaseSequencer` | tests `jettison`, `hung_store` |
| REL-8 | Dropping the master arm or landing stops any pulse in the same cycle. | `ReleaseSequencer` | proof (REL-1), tests `interlocks`, `jettison` |
| REL-9 | A request made while a release is in progress is ignored; `phase` shows the release progress (`PhSafe`, `PhReady`, `PhFiring`, `PhJettison`, `PhVerify`). | `Sequencer` state machine | test `balanced_release` |

### Robustness — the code that runs on the vehicle

| id | requirement | implemented in | verified by |
|----|-------------|----------------|-------------|
| RTE-1 | No runtime error in any reachable state, however long the vehicle flies: no integer computation overflows its C type (int32), no division by zero, no array index out of bounds. Counters saturate. | every operator (`ReleaseSequencer`'s phase counter saturates at `PULSE_CYCLES + VERIFY_CYCLES`) | proof (runtime-error checks, in context of `SMS`) |

## 3. Architecture

One root operator, `SMS`, composes four functions and one stateful operator.
Data flows left to right; the only feedback is the hung latch, read one
cycle late (`pre`), so the model is causal.

```
 present_k, id_k ─▶ StationDecode ×4 ─▶ kind_k, status_k, mass_k
                                           │
                                           ▼
                                        Balance ─▶ total_mass, roll, pitch, balanced
                                           │
 req_kind ──────────────────────────▶ PlanRelease ─▶ plan s1..s4, found, have_kind
                                           │
 master_arm, wow, alt_m ─▶ Inhibit (activation: if / elsif / else) ─▶ inhibit
                                           │
 release_req ─▶ edge ∧ inhibit = Clear ─▶ ReleaseSequencer ─▶ fire1..4, phase, hung_k
 jettison_req ─▶ edge ───────────────────▶   └ Sequencer state machine
```

| operator | kind | role |
|----------|------|------|
| `StationDecode` | function | tag code → kind, mass, station status |
| `Balance` | function | masses → total, roll, pitch moments, balanced |
| `Candidate` | function | would removing a moment keep the balance? (and the imbalance left) |
| `Best` | function | pick the better of two candidates |
| `PlanRelease` | function | which station(s) to release for a requested kind |
| `Abs` | function | absolute value |
| `ReleaseSequencer` | operator | latches the plan, drives the pulses, detects hung stores; owns the `Sequencer` state machine |
| `SMS` | operator (root) | wires the above; owns the `Inhibit` activation |

**Types** (`types.json`): `StoreKind`, `StationStatus` (`Vacant`, `Loaded`,
`Mismatch`, `Hung`), `SeqPhase`, `Inhibit` (`Clear`, `NotArmed`,
`OnGround`, `StationFault`, `LowAltitude`, `NoMatchingStore`,
`WouldUnbalance`).

**Constants:** station arms `ARM_X`, `ARM_Y`; catalogue masses;
`ROLL_LIMIT`, `PITCH_LIMIT`, `MIN_RELEASE_ALT`, `PULSE_CYCLES`,
`VERIFY_CYCLES`.

### The release sequencer (state machine `Sequencer`)

```
          ready_ok                fire_req
  Safe ─────────────▶ Ready ─────────────────▶ Firing ── cnt ≥ PULSE_CYCLES−1 ──▶ Verify
   ▲  ◀── not ready_ok ─┘ │ jettison_req                                       │
   │                      └───────────────────▶ Jettison ─ cnt ≥ PULSE_CYCLES−1 ─▶┤
   └────────── not ready_ok (from Firing / Jettison)          clear or timeout ─┘ ▶ Ready
```

`ready_ok = master_arm ∧ ¬wow`. Transitions take effect on the next cycle
(SCADE's weak transitions). The plan is latched when the request is accepted
(`Ready ∧ fire_req`); a jettison latches every occupied station. A station
fires while the machine is in `Firing`/`Jettison` and its latch is set, and
only while `permit = ready_ok ∧ (jett ∨ alt_ok)` holds — so REL-1/2 hold
even mid-pulse. A hung station is never latched by a selective release
because the planner only selects `Loaded` stations; Kind 2 proves REL-5 from
that, and an earlier per-station hung gate on the pulse — which no test could
ever flip (MC/DC) — was removed as dead logic.

### The inhibit decision tree (activation `Inhibit`)

```
if not master_arm          → NotArmed
elsif wow                  → OnGround
elsif any station Mismatch → StationFault      (the balance cannot be trusted)
elsif alt_m < MIN_RELEASE_ALT → LowAltitude
elsif no Loaded store of req_kind → NoMatchingStore
elsif no balanced plan     → WouldUnbalance
else                       → Clear
```

## 4. Verification plan

1. **Static checks** — types, clocks, contract well-formedness: `openlustre
   check`, and live in the Studio.
2. **Formal proof (Kind 2)** — the contracts below, proved for every input
   sequence (`openlustre prove`, Verify dock):
   - `StationDecode_contract` — INV-1..3;
   - `Balance_contract` — INV-4, BAL-1;
   - `PlanRelease_contract` — BAL-2, BAL-5, BAL-6;
   - `SMS_contract` — REL-1..5 as guarantees; a mode table over the flight
     situation (`Grounded`, `Disarmed`, `ArmedAirborne`) whose modes Kind 2
     checks reachable and exhaustive (REL-6).
   Contract realizability is checked as well, and the whole of `SMS` is
   proved free of runtime errors (RTE-1): integer overflow, division by
   zero, index bounds — in context, for every call instance.
3. **Scenarios** — recorded golden traces, each run on the model *and* the
   compiled generated C (`openlustre test run`, Tests dock):
   `inventory_loading`, `balanced_release`, `planner_choice`,
   `pair_release`, `rear_pair`, `hung_store`, `interlocks`, `jettison`.
   Target: every decision and MC/DC condition of the model covered.
4. **Model ≡ code** — the same scenarios in lockstep on the IR simulator and
   the generated C; C-in-the-loop in the Simulation dock.
5. **Traceability** — each generated C equation carries an `@trace` back to
   its diagram element; the generation report fingerprints the sources.
6. **Evidence** — `openlustre evidence sms.wksc --root SMS --prove` gathers
   all of the above into one document.
7. **Flight code** — the C generated for `SMS`, compiled with the platform
   integration (`integration/`) and run on a scripted 50 s mission whose log
   is compared with the recorded one.

`scripts/verify.sh` runs 1–7; CI (`.github/workflows/verify.yml`) runs it on
every push with the OpenLustre Studio pinned in `OPENLUSTRE_VERSION`, and
keeps the generated code and the evidence.

## 5. Implementation steps

| step | work | status |
|------|------|--------|
| 1 | Workspace (`openlustre new`), types and constants | done |
| 2 | Functions `StationDecode`, `Abs`, `Balance`, `Candidate`, `Best`, `PlanRelease` | done |
| 3 | `ReleaseSequencer` with its `Sequencer` state machine | done |
| 4 | `SMS` root with the `Inhibit` activation; set as root | done |
| 5 | Contracts: `StationDecode`, `Balance`, `PlanRelease`, `SMS` | done |
| 6 | Scenarios and golden traces; decision / MC/DC coverage | done |
| 7 | Kind 2 proofs; model ≡ C; evidence report | done |
| 8 | Studio walkthrough, README | done |
| 9 | Runtime errors proved (RTE-1); the phase counter saturates | done |
| 10 | Flight code: cyclic task, platform interface, scripted mission | done |
| 11 | Standalone repository: pinned toolchain, install / verify scripts, CI | done |

**Result.** The evidence report for `SMS` is **PASS** with no gaps: static
checks clean across the 8 operators; 171 of 171 properties proved by Kind 2
v2.2.0 (every guarantee and mode ensure of the 8 contracts, all three flight
modes reachable and exhaustive, and 60 runtime-error checks) and the contract
realizable; 8 of 8 scenarios match their goldens on the model and on the
compiled generated C (182 cycles); if-decisions 58/58 and MC/DC 151/151;
104 of 104 generated equations traced to the model.

The runtime-error proof found one defect, fixed: `ReleaseSequencer`'s phase
counter counted up without bound while the SMS stayed in one phase (an
`int32` overflow after 2³¹ cycles). It now saturates (RTE-1).

The model was built in the Studio through its editing API —
`build/build_sms.py` replays those steps from `build/sms_source.lus` (the
functions and operators, imported with **Import Lustre**) plus the state
machine, activation, contracts and diagram layout — so the project can be
rebuilt from scratch; `build/make_scenarios.py` writes the scenarios from
annotated steps. The workspace files are the source of truth from then on.

## 6. Assumptions and limits

- Sensors are trusted as reported each cycle; a mismatched station is
  reported rather than guessed. Tags are read when the store is hung on.
- Masses are catalogue values; moments use integer grams and millimetres
  (at most 4 × 2000 g × 200 mm — proved not to overflow, RTE-1).
- The proof treats integers as mathematical integers (Kind 2 view); the
  generated C uses `int32_t`. The runtime-error checks prove the two equal:
  no integer value ever leaves `int32` (RTE-1). Sensor inputs are assumed
  within their C types.
- The SMS does not fly the vehicle: the flight controller trims residual
  imbalance within the limits.
