# PNT — a Position, Navigation and Time manager: the implementation plan

A complete OpenLustre Studio project: the PNT core of a vehicle that fuses
GNSS, inertial and barometric sensor reports into one trusted
position/velocity/time solution — a float32 scalar Kalman filter per axis
(predict with the IMU, update with an accepted fix, gain from the
variance), a disciplined clock with bounded holdover, and an integrity
architecture in which no sensor that disagrees with the fused truth can
move that solution: the navigation computer between the sensor plugins
and the consumer.

The architecture follows the shape of [pntOS](https://www.pntos.com/), the
open plugin architecture for PNT sensor fusion:

| pntOS idea | here |
|---|---|
| pluggable sensors behind a standard message interface | every source enters through a **monitor** operator over a fixed report interface (validity, quality, data) — the model's inputs *are* the plugin boundary: GNSS, IMU, barometer |
| a fusion engine | `KalmanChan` × 3: predict with clamped IMU increments, update with accepted fixes at gain k = P/(P+R) |
| integrity safeguards (an added sensor's errors must not propagate) | the innovation gate (horizontal), the vertical gate (GNSS altitude vs barometer), spike clamping at the source, and exclusion: a disagreeing source is never merged — and the alert tree says why |
| adaptability as needs evolve | sources are independent operators with their own contracts; the barometer was added as a third plugin without touching the mode logic |

This file is the specification: requirements with ids (the contract clause
names cite them), architecture, verification plan and results. The model
is `pnt.wksc` (built by `build/build_pnt.py` from `build/pnt_source.lus`),
the proofs and evidence live in `out/` after `scripts/verify.sh`.

## Units and timing

The system runs at **10 Hz** (one model cycle = 100 ms). The navigation
solution is **float32 meters** (single-precision semantics are identical
in the simulator, the generated C and the tests; proofs reason about the
exact reals behind the same expressions — the verification spine's
byte-identical-trace invariant carries the floating-point behaviour).
Time stays in `int32` seconds. Every real literal carries `_f32` so the
arithmetic stays in single precision (a bare real literal is a double).

| quantity | unit | note |
|---|---|---|
| position `x`, `y`, altitude | m (float32) | local tangent plane, origin at boot |
| velocity increments `dvx/dvy/dvz` | m per cycle | IMU delta over the cycle |
| reported velocity `sol_v*` | m/s | increment × 10 |
| time `tow` | s (int32) | GNSS time of week |
| `hdop` | × 100 (int32) | 250 = HDOP 2.5 |
| variances `p_*` | m² | per-axis Kalman variance |

Constants (in `types.json` and the model):

| constant | value | meaning |
|---|---|---|
| `SATS_MIN` / `HDOP_MAX` | 4 / 250 | usable-fix quality floor |
| `VEL_STEP_MAX_M` | 2.0 | clamp on an IMU increment (20 m/s at 10 Hz) |
| `POS_DISAGREE_M` | 10.0 | innovation gate, both horizontal axes |
| `V_DISAGREE_M` | 15.0 | GNSS-altitude vs barometer gate |
| `POS_MAX_M` / `POS_SAT_M` | 10000.0 / 12000.0 | assumed input bound / solution saturation |
| `Q_POS`, `R_GNSS` | 0.05, 4.0 | horizontal process / measurement variance |
| `Q_ALT`, `R_GNSS_Z`, `R_BARO` | 0.02, 9.0, 25.0 | vertical process / GNSS-z / baro measurement variance |
| `P_FLOOR`, `P_CEIL`, `R_MIN` | 0.25, 100.0, 1.0 | variance bounds; no input can make the gain divide by zero |
| `CONVERGE_CYCLES` | 20 | accepted fixes before Fused (2 s) |
| `COAST_BUDGET_CYCLES` | 50 | dead-reckoning budget (5 s) |
| `PROTECT_FUSED_M` / `PROTECT_COAST_M` | 10.0 / 120.0 | protection levels |
| `DRIFT_WINDOW_CYCLES` | 60 | the coast drift bound (60 × 2 m = 120 m) |
| `HOLDOVER_MAX_S` / `TOW_MAX_S` | 10 / 604800 | clock holdover saturation / a week |

**Environment contract (assumptions of the root):** GNSS positions and
altitudes and the barometer stay within ±`POS_MAX_M`; TOW within the week.
Everything else (satellites, HDOP, increments) needs no assumption:
monitors clamp or only compare.

## Architecture

```text
   GNSS report ──► GnssMonitor ─► usable? ─┐ innovation gate (10 m, xy)
                                           ▼
   IMU report ───► ImuMonitor ─► clamped ► KalmanChan ×2 ─► sol_x, sol_y
   Baro report ──► BaroMonitor ─► usable?   │ gain k = P/(P+R)
                                vertical gate (GNSS z vs baro, 15 m)
                                           ▼
                              KalmanChan (alt) ─► sol_alt
   GNSS time ────► ClockDiscipline ─► tow, holdover
                                           ▲
                              Navigator (state machine)
                  Init → Converging → Fused ⇄ Coasting → Hold
                                           ▼
                    Alert (decision tree): Sensor / Timeout / Integrity
```

| operator | kind | requirements |
|---|---|---|
| `PNT` | root node | owns the **Navigator** state machine and the **Alert** decision tree |
| `GnssMonitor` | function | SRC-1, SRC-2 |
| `ImuMonitor` | function | SRC-2, SRC-3 |
| `BaroMonitor` | function | SRC-5 |
| `KalmanChan` | node | KAL-1..7 (one axis of the filter) |
| `ClockDiscipline` | node | TME-1..3 |
| `BoundedF` | function | SRC-6 |

### The Navigator state machine (owned by `PNT`)

Unchanged from v1 in shape:

- **MInit** — no fix yet: `PvtInvalid`. On a usable fix and a healthy IMU
  → **MConverging**.
- **MConverging** — `PvtAcquiring`: `CONVERGE_CYCLES` accepted fixes →
  **MFused**; fix or IMU lost → MInit.
- **MFused** — `PvtFused`: full PVT. IMU lost → **MHold**; GNSS unusable
  or failing the gate → **MCoasting**.
- **MCoasting** — `PvtCoasting`: dead reckoning, `cnt` counting to the
  budget. GNSS re-accepted → MFused; budget spent or IMU lost → **MHold**.
- **MHold** — `PvtHold`: the filter pauses (outputs frozen), alerts on.
  Both sources healthy → MConverging.

### The Kalman channels and the bootstrap rule

Each axis runs the classic scalar filter: predict (`pos += step`,
`P += Q` bounded to `[P_FLOOR, P_CEIL]`), update with gain
`k = P/(P+R)` when a measurement is accepted, variance `(1−k)P` after.
Three rules make it a *navigating* filter rather than a textbook one:

- **The gates decide, the filter follows.** `meas_ok` (horizontal) needs
  a usable, time-tagged fix that agrees with the prediction; the
  vertical channel takes GNSS altitude when accepted (checked against the
  barometer) and the barometer otherwise, so the vertical keeps being
  measured through a GNSS outage.
- **Bootstrap snaps.** While the chain is untrusted (Init, Acquiring,
  recovery), the variance is held at `P_CEIL` — every bootstrap
  measurement comes in at high gain and the state snaps to the fix. A
  stale estimate with a small variance would otherwise converge far too
  slowly through the innovation gate (found the hard way, see Results).
- **Hold pauses the filter.** In `PvtHold` position and variance freeze
  inside `KalmanChan` itself, so `sol` is one continuous chain and
  leaving Hold never jumps.

### Integrity (the pntOS safeguard, proved)

- The **innovation gate** (10 m, both horizontal axes) is the measurement
  gate: a valid-looking fix 300 m off is never merged (PVT-2).
- The **vertical gate**: GNSS altitude is fed to the vertical channel
  only within 15 m of the barometer; without a baro there is nothing to
  check against and the gate passes — a baro failure degrades coverage,
  never blocks navigation.
- IMU increments are clamped to ±20 m at the monitor — a 2 km spike
  moves the solution by 20 m (SRC-3).
- After the coast budget the solution freezes rather than drifts
  (PVT-3, PVT-4), and the alert tree names the cause (AL-1..4).

### Time

`ClockDiscipline` steers a free-running second counter (10 cycles = 1 s)
to `gnss_tow_s` whenever the fix is usable and time-tagged — effective on
the boot cycle too. In holdover it free-runs and `holdover_s` counts
(saturating at 10 s); `tow` saturates at `TOW_MAX_S`, so no counter can
overflow, however long it runs.

## Requirements

**Sources (monitors = the plugin boundary)**

- **SRC-1** A GNSS report is usable exactly when it is valid, a 3D fix,
  `sats ≥ SATS_MIN` and `hdop ≤ HDOP_MAX`.
- **SRC-2** Health tells the truth (all monitors): usable ⇒ `SrcOk`;
  unusable with a signal ⇒ `SrcSuspect`; no signal ⇒ `SrcFault`.
- **SRC-3** An IMU increment is clamped to ±`VEL_STEP_MAX_M` (2 m per
  cycle = 20 m/s) on every axis before it can touch the solution.
- **SRC-5** The barometer reports usability and health like any plugin;
  its failure removes a cross-check, never a blocker.
- **SRC-6** `BoundedF` returns a value within `[lo, hi]`, and `x` itself
  when already in range.

**Integrity**

- **INT-1** The innovation gate agrees exactly when the fix is within
  `POS_DISAGREE_M` of the predicted solution on both horizontal axes.
- **INT-2** `integrity_ok` is exactly the alert tree's verdict: no
  divergence raised.
- **INT-3** The vertical gate is the GNSS-altitude-vs-barometer check.

**Navigation (the Kalman channels)**

- **KAL-1** The gain is a proper gain: `0 ≤ k ≤ 1`, always; the variance
  stays in `[0, P_CEIL]`.
- **KAL-2** An update is a convex blend: the new position lies between
  the prediction and the measurement.
- **KAL-3** Updates shrink the variance; predict-only cycles carry it.
- **KAL-4** Hold freezes both; bootstrap forces high gain (`k ≥ 0.5`).
- **KAL-5** Without a measurement the position moves at most
  ±`VEL_STEP_MAX_M` per cycle.
- **KAL-6** Positions stay within ±`POS_SAT_M` forever.
- **NAV-1/ALT-1** At the root: an accepted fix blends convexly (never
  past the fix, never farther from it); the vertical tracks the baro
  convexly when it is the only source.

**Time**

- **TME-1..3** While synced, `tow = gnss_tow_s` and holdover is 0;
  holdover ≤ `HOLDOVER_MAX_S`; `tow` stays in `[0, TOW_MAX_S]` forever.

**The solution (root contract)**

- **PVT-1** `anchored` ⇒ the update blended convexly toward the fix on
  both axes, time synced, holdover 0.
- **PVT-2** (the safeguard) while Coasting without an accepted fix, the
  solution moves only by the clamped IMU increments.
- **PVT-3** while Coasting, the coast counter never exceeds
  `COAST_BUDGET_CYCLES`; combined with PVT-2's proved per-cycle step
  bound (`VEL_STEP_MAX_M`), the solution stays within
  `PROTECT_COAST_M` (50 × 2 m + entry ≤ 120 m) — the ingredients are
  proved, the multiplication is arithmetic.
- **PVT-4** while in Hold, position and velocity outputs are frozen.
- **PVT-5** Fused means it: `status = PvtFused ⇒ anchored the cycle
  before`.
- **AL-1..4** Each alert names a true cause (sensor lost / budget spent /
  diverging source / none).

**Runtime errors (RTE-1)** — every integer operation fits `int32` (the
clock and counters saturate); no float operation can divide by zero (the
variance is floored and R clamped above `R_MIN`), however long it runs.

## Verification plan

1. `openlustre check pnt.wksc` — types, clocks, contract well-formedness.
2. `openlustre test run pnt.wksc --scenarios scenarios` — every scenario
   on the model **and** on the compiled generated C (`--backend both`,
   byte-identical traces — this carries the float32 semantics), with
   decision/MC/DC coverage.
3. `openlustre prove pnt.wksc` — every guarantee, mode ensure and mode
   coverage check, plus runtime-error checks, with Kind 2 (exact reals).
4. `openlustre evidence pnt.wksc …` — one document gathering all of it.
5. `integration/` — the generated C in a 10 Hz cyclic task flying a
   scripted 62 s mission, compared line by line with
   `expected_mission.txt`.

### Scenarios

| scenario | exercises |
|---|---|
| `cold_start` | no fix → first fix (filter snaps at k≈0.96) → fused |
| `gnss_dropout` | a 3 s outage inside the budget: coast, fusion again |
| `integrity_reject` | a valid fix 300 m off: rejected, timeout, snap-back recovery |
| `imu_fault` | self-test failure: hold with the filter paused; recovery |
| `clock_holdover` | 25 s outage: holdover bounded, tow free-runs, resync |
| `spike_reject` | a 2000 m increment: clamped to 20 m |
| `degraded_fix` | too few satellites / HDOP over: `SrcSuspect`, not usable |
| `acquisition_lost` | fix lost during convergence: back to Invalid |
| `time_loss` | fixes without time: not measurable, clock free-runs |
| `cross_axis` | y-only drift: the gate fails on one axis alone |
| `baro_fault` | self-test-only, valid-line-only and full baro loss; both plugins dark |
| `vertical_lie` | GNSS altitude lies up and down; the vertical rides the baro |
| `receiver_quirks` | the monitor's decision table, boundary by boundary |
| `combined_faults` | faults that only appear together, each timed to a specific guard |
| `week_rollover` | TOW saturates exactly at the end of the week |

## Results

`scripts/verify.sh` (2026-10-22, v2, OpenLustre Studio at
`OPENLUSTRE_VERSION`, Kind 2 v2.2.0 / Z3 4.13.4):

| section | result |
|---|---|
| static checks (types, clocks, contracts) | pass, 7 operators |
| scenarios, model and compiled C (`--backend both`) | 30 passed (15 scenarios × 2 backends), 2210 cycles byte-identical (float32 included) |
| structural coverage | if-decisions 50/50 · MC/DC 107/107 |
| Kind 2 proof | **114 of 114 properties hold** (contracts, mode coverage, realizability) |
| runtime errors | 5 of 5 checks hold (clock/counter arithmetic; no float division can reach zero) |
| model ≡ generated code | 15 of 15 scenarios, 2210 cycles compared |
| traceability | 80 of 80 equations traced, 3 files fingerprinted |
| evidence report (`out/evidence`) | **PASS** |
| scripted mission | matches `integration/expected_mission.txt` |

Development found and fixed real defects, each first seen as a proof
failure or a trace that read wrong:

- the disciplined clock initialized `tow` to 0 on the boot cycle even
  when the first fix already carried time;
- the Hold freeze applied only to the published solution, so leaving
  Hold jumped the position;
- the Kalman prior started overconfident (`P_FLOOR`), so the first fix
  converged slowly and the system could reach Fused with a residual
  larger than the innovation gate — latching into a coast it could not
  leave. The bootstrap rule (variance at `P_CEIL` while untrusted) makes
  every acquisition snap, and the same failure cannot recur;
- the float conversion initially carried v1's clamp over unchanged
  (20 m per cycle instead of 2 m — 200 m/s!), which silently made the
  coast protection bound false; the stalled Kind 2 proof was what
  exposed it.
