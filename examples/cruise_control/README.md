# Cruise control — worked example

A small automotive **cruise control** modeled as a state machine, taken all the
way through the OpenLustre loop: design → check → simulate → generate C-Lite →
run the compiled code. Open `cruise_control.wksc` in the Studio
(`openlustre studio launch examples/cruise_control/cruise_control.wksc`).

## The model

`CruiseControl` is driven by an owned **Off ⇄ On** state machine.

**Inputs**

| Input | Type | Meaning |
|---|---|---|
| `speed` | int32 | current road speed |
| `set_cruise_on` | bool | engage cruise (capture the current speed as the set-point) |
| `brake` | bool | brake pressed — disengage |
| `turn_cruise_off` | bool | switch cruise off — disengage |
| `increase_by_one` | bool | while engaged, raise the set-point by 1 |

**Outputs**

| Output | Type | Meaning |
|---|---|---|
| `cruise_active` | bool | is cruise engaged |
| `target_speed` | int32 | the maintained set-point |

**States**

- **Off** — `cruise_active = false`; `target_speed = speed` (tracks the road
  speed, so it is the set-point the moment cruise engages). `set_cruise_on` → On.
- **On** — `cruise_active = true`; `target_speed = speed -> (pre target_speed +
  (increase_by_one ? 1 : 0))` (holds the captured set-point, +1 per
  `increase_by_one`). `brake` or `turn_cruise_off` → Off.

## Verify it

- **Model (step simulation):** open in the Studio and use the Simulation watch
  table, or `openlustre simulate cruise_control.wksc --inputs scenarios/drive.csv`.
- **Compiled code:** *Code ▸ Compile C-Lite* in the Studio, or
  `openlustre emit-clite cruise_control.wksc --out build --driver --root CruiseControl`,
  then build and feed it the same CSV on stdin.

Both produce the **same** trace for `scenarios/drive.csv` (verified cell-for-cell
in `tests/cruise_control.rs` for the model, and against MSVC-compiled C):

```
cycle,cruise_active,target_speed
0,false,50      # off, tracking road speed
1,false,55      # set_cruise_on pressed; engaging, captures 55
2,true,55       # On — holds the set-point
3,true,56       # increase_by_one
4,true,57       # increase_by_one
5,true,57       # brake pressed; disengaging
6,false,70      # off again
```
