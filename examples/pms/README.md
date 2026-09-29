# PMS — a Payload Management System for a drone

A complete OpenLustre Studio project: the payload management system of a
multirotor drone (soon to be other types of drones) that carries up to four stores (payload items) on release
hooks. It **identifies** what is loaded on each hook, keeps the vehicle
**balanced** as payloads leave, and **drops** payloads on command, only when
it is safe to. [PLAN.md](PLAN.md) is the specification: requirements with
ids, architecture, verification plan and results.

![The PMS root operator in OpenLustre Studio — commands and flight state top left, the four stations below, station decoding, balance and release planning, the Inhibit decision tree and the release sequencer, outputs grouped by release, plan, balance and inventory.](docs/screenshots/13-pms-diagram.png)

## Built with OpenLustre Studio

The PMS is modelled, tested, proved and turned into flight code with
[OpenLustre Studio](https://github.com/Shepherdhunt/OpenLustreStudio) — the
way a SCADE project is built with SCADE Suite. `OPENLUSTRE_VERSION` pins the
version it is built with (a commit or a release tag), like a SCADE project's
tool version; `scripts/install-openlustre.sh` builds that version into
`.openlustre/` (needs [Rust](https://rustup.rs) and git) together with its
prover, Kind 2 v2.2.0 and Z3.

```sh
scripts/install-openlustre.sh   # once, and after changing OPENLUSTRE_VERSION
scripts/studio.sh               # open the PMS in the Studio (browser)
scripts/verify.sh               # check, test, flight code, proof, evidence
```

An `openlustre` already on `PATH` (or `$OPENLUSTRE`) works as well; then
`openlustre studio launch .` opens the project.

**One toolchain.** For now the PMS is built entirely with OpenLustre
Studio: the model, its simulation and tests, the proofs, the generated flight
code and the evidence all come from the one version pinned in
`OPENLUSTRE_VERSION`, and no other modelling tool is needed or used. Working
with other environments — exchanging the model with Ansys SCADE or
MATLAB/Simulink — is planned for later and is not part of this project yet.

## The model

| operator | kind | what it does |
|----------|------|--------------|
| `PMS` | root operator | wires it all; owns the **Inhibit** decision tree |
| `StationDecode` | function | tag code → store kind, mass, station status |
| `Balance` | function | total mass, roll and pitch moments, balanced? |
| `PlanRelease` (+ `Candidate`, `Best`, `Abs`) | functions | which hook(s) to open for the requested kind |
| `ReleaseSequencer` | operator | latches the plan, pulses the hooks, catches hung stores; owns the **Sequencer** state machine |

Types (`StoreKind`, `StationStatus`, `SeqPhase`, `Inhibit`) and constants
(station arms, catalogue masses, balance limits, timing) are in
`types.json`. Every operator has a contract whose clause names are the
requirement ids of the plan.

![The Sequencer state machine: Safe, Ready, Firing, Jettison and Verify.](docs/screenshots/14-pms-sequencer.png)

![The Inhibit decision tree: not armed, on the ground, station fault, too low, no such store, would unbalance, else clear.](docs/screenshots/15-pms-inhibit.png)

## How it keeps the vehicle balanced

The hooks sit on a 2 × 2 grid around the centre of gravity. The PMS knows
each store's mass from its tag, so it knows the roll and pitch moments of
the loadout. When the operator asks for a kind of item, the planner looks at
every hook holding one and releases the one that leaves the smallest
imbalance within the limits. If no single hook will do, it releases a
lateral pair together (front or rear). If nothing keeps the vehicle
balanced it refuses (`inhibit = WouldUnbalance`), and the operator can drop
something else first. `next1..4` show which hooks the next release would
open. Kind 2 proves that every plan keeps the vehicle within the limits, and
that a plan is found whenever a single safe release exists.

## No runtime errors

The PMS computes masses and moments in `int32` (grams, g·mm). `Prove` also
proves it free of runtime errors (requirement RTE-1): every sum, difference
and product in `Balance`, `PlanRelease` and each of the six `Candidate`
instances fits `int32`, and so does every negation in `Abs` — proved for the
values the stations can actually report, not for any `int32`. The check found
one real defect: the sequencer's phase counter `cnt` counted up forever while
the PMS sat in one phase, and would have overflowed after 2³¹ cycles (about
eight months at 100 Hz). It now saturates at `PULSE_CYCLES + VERIFY_CYCLES`,
above every bound it is compared with, and all 60 checks hold.

## Check, test, prove

`scripts/verify.sh` runs all of it — and so does CI
(`.github/workflows/verify.yml`) on every push. Step by step:

```sh
openlustre check pms.wksc
openlustre test run pms.wksc --scenarios scenarios
#   16 passed (8 scenarios on the model and the compiled C); MC/DC 151/151
openlustre prove pms.wksc --timeout 600
#   prove: 171 of 171 hold
#   prove: runtime errors — 60 of 60 checks hold (overflow, division by zero, bounds, conversion)
openlustre evidence pms.wksc --scenarios scenarios --prove --timeout 600 --out out/evidence
#   evidence for `PMS`: PASS
```

| scenario | exercises |
|----------|-----------|
| `inventory_loading` | tags read before stores, unknown tags, masses and moments, pitch-only imbalance, no release on the ground |
| `balanced_release` | a release that keeps balance, one refused as rear-heavy, the order the planner then allows |
| `planner_choice` | the least-imbalance choice (not the lowest hook); a pulse stopped by losing altitude |
| `pair_release`, `rear_pair` | lateral pairs released together |
| `hung_store` | a store that does not leave: latched, skipped, retried by jettison, reset on the ground only |
| `interlocks` | every refusal reason; disarming mid-pulse; a request during a release ignored |
| `jettison` | emergency jettison below the release altitude; losing the arm stops it |

![The Verify dock: 171 of 171 properties proved by Kind 2 v2.2.0 with Z3 — the mode coverage of PMS_contract, then the Runtime errors group: 60 of 60 checks hold in the context of PMS, each named by its call path (Balance#1, Balance#1 › Abs#1, PlanRelease#1, …) with what must hold (m1 + m2 + m3 + m4 fits int32).](docs/screenshots/16-pms-verify.png)

![The Evidence Report for PMS: PASS — static checks, contract, Kind 2 proof (171 of 171, among them 60 of 60 runtime-error checks; realizable), 8 of 8 scenarios, if-decisions 58/58 and MC/DC 151/151, compiled C matches the model on 182 cycles, 104 of 104 equations traced.](docs/screenshots/17-pms-evidence.png)

## The flight code

```sh
openlustre emit-clite pms.wksc --root PMS --out out/code
```

generates the PMS as C11 (`out/code/clite/openlustre_generated.{h,c}`): one
step function over explicit state — `PMS_init(&state)` once, then
`PMS_step(&state, &in, &out)` every 10 ms — with no allocation, no globals
and no library calls, every equation traced back to the diagram
(`trace.json`, `generation_report.md`). `integration/` puts it on a vehicle:

| file | what |
|------|------|
| `pms_platform.h` | what the flight computer provides: sample the inputs, drive the hooks, `PMS_PERIOD_MS` |
| `pms_task.c` | the 100 Hz cyclic task: read, step, write (`--realtime` paces it on the clock) |
| `pms_clock.c` | the period clock for macOS, Linux and Windows (the RTOS timer replaces it on the vehicle) |
| `mission_sim.c` | a desktop platform: a simulated vehicle, hooks (one of them jammed) and operator flying a 50 s mission |
| `expected_mission.txt` | the mission's log, which `scripts/verify.sh` and CI compare against |

```
t=  2.00 s  #200   operator: release MedKit -> refused: OnGround (alt 0 m)
t=  5.00 s  #500   operator: release MedKit -> refused: LowAltitude (alt 6 m)
t= 15.00 s  #1500  operator: release MedKit -> accepted, plan 1---
t= 15.01 s  #1501  hooks: FIRE 1---
t= 15.02 s  #1502  payload: 3500 g, roll 100000 g*mm, pitch -165000 g*mm, balanced
t= 20.09 s  #2009  station 4: Hung
t= 25.00 s  #2500  operator: release WaterPack -> refused: WouldUnbalance (alt 30 m)
t= 30.01 s  #3001  hooks: FIRE -234
```

On real hardware, replace `mission_sim.c` with drivers for the hook
sensors, tag readers, altimeter and release actuators, and run `pms_task.c`'s
loop as the RTOS task that owns the PMS.

## Files

| path | what |
|------|------|
| `pms.wksc`, `types.json` | the workspace (operators, contracts, state machine, activation, layout; types and constants) |
| `*.lus` | each operator's Lustre, as Build writes it (the prover's view) |
| `scenarios/` | input vectors and recorded golden traces |
| `PLAN.md` | the implementation plan |
| `integration/` | the platform around the generated code, and the scripted mission |
| `OPENLUSTRE_VERSION` | the OpenLustre Studio version the project is built with |
| `scripts/` | install the toolchain, open the Studio, verify everything |
| `.github/workflows/verify.yml` | CI: the same verification on every push, generated code and evidence kept |
| `build/` | how the project was built in the Studio (`build_pms.py` replays it through the editing API from `pms_source.lus`) and how the scenarios were written (`make_scenarios.py`) |
| `docs/screenshots/` | the pictures in this file |
| `out/` | generated: flight code, evidence, the mission log (not committed) |

This project started as `examples/sms` in the OpenLustreStudio repository,
where a copy stays as a regression test of the tool.
