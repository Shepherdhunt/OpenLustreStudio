# OpenLustre Studio — goals review and SCADE-parity roadmap

Status as of the activate-if work (branch `claude/openlustre-studio-graphics-78hfg9`).
This document checks the Studio against the goals the project set for itself
(`README.md`, `implemenation_plan.md`), records where recent work landed, and
proposes what to build next, in priority order.

## The two goal statements

1. **The project's own goal** (README, implementation plan): a graphical Lustre +
   CoCoSpec workbench — *"the model is equations + contracts + modes +
   evidence"* — whose headline differentiators are first-class contracts,
   Kind 2 proof, and generated C-Lite provably equivalent to the simulated
   model.
2. **The working direction for this effort**: be Ansys-SCADE-like end to end —
   design models graphically, have a code generator read the model and emit
   C-Lite, compile and run it, and step/simulate the running model.

The two are compatible: SCADE's own value is exactly "graphical model →
qualified code → evidence". But they weight things differently, and the
review below shows the GUI has drifted toward (2) while the centre of (1) has
no GUI at all.

## Where things stand

| Capability | CLI / engine | Studio GUI |
|---|---|---|
| Dataflow authoring (blocks, wires, typed ports) | ✅ | ✅ SCADE-style glyphs, orthogonal wires, zoom/pan, minimap, align/distribute, clipboard, export |
| State machines (flat + hierarchical, operator-owned) | ✅ | ✅ textual editor, draggable chart, canvas block |
| Conditional activation (activate-if) | ✅ clocked branches (frozen when inactive) + `last(v)` | ✅ editor, decision-tree chart, canvas block |
| Type checking, function/operator rules | ✅ | ✅ errors mapped onto boxes and wires |
| **CoCoSpec contracts: assume / guarantee / modes** | ✅ IR, checker (now type-checks clauses), CoCoSpec emit, observer-based runtime monitors | ✅ **contract editor: clause rows, mode table, CoCoSpec text, live checking** (was read-only) |
| Lustre + CoCoSpec export | ✅ | ✅ Lustre pane |
| C-Lite generation (selected root + closure) | ✅ `@trace` per equation, trace matrix, generation report (SHA-256) | ✅ Generate / C pane / save files; **click C ↔ select model element; generation report** |
| Compile & run | ✅ host compiler, CSV driver, Makefile | ✅ host only; cross-compile shown as "roadmap" |
| Stepping / simulation | ✅ batch + full trace; incremental `step_observed`; per-cycle input sequences | ✅ **server-side session: step / run N / breakpoints / stop on violation; live values, active state, fired branch and contract modes on the diagram; waveform with cycle review; the compiled C stepped in lockstep ("C in the loop")** (was replay-from-zero) |
| IR ≡ compiled-C trace equivalence | ✅ `test run --backend both` | ✅ Tests dock (with decision + MC/DC coverage); **each run as a waveform against its golden, first divergence marked; replay in the simulator; live, cycle by cycle, in the Simulation dock** |
| Kind 2 proof | ✅ adapter; bmc-ind / realizability / mode-coverage modes; structured counterexamples | ⚠️ default mode only; **counterexample as a waveform, replayable in the simulator** (was an ASCII block); **Kind 2 not bundled or in CI** |
| Evidence report | ✅ `openlustre evidence` (HTML + JSON, fails on FAIL) | ✅ Project ▸ Evidence Report (verdict, sections, full page, downloads) |
| Imported C operators | ✅ manifests, wrappers, validation | ❌ no way to register or place one |

### Assessment

**On track:** the verification spine is genuinely strong — the IR simulator
and compiled C agree byte for byte across every construct, including the new
activations. The typed, strict IR, selective code generation, and graphical
drafting (now close to SCADE for dataflow) all serve both goals.

**Drift:** the last several rounds went into drafting polish and SCADE
control structures while the project's headline differentiator — contracts
and modes — still cannot be touched from the GUI. The README says contracts
"live beside the equations"; in the Studio they only live in the JSON file.
Likewise the "evidence layer" of the plan has no output, and proving needs a
tool most users (Windows installer; as far as we know Kind 2 publishes Linux
and macOS builds only) won't have.

**Correctness debt found and fixed this round** (worth knowing because they
were silent):

- An equation could assign an operator's **input** with no error — now
  `E0022`.
- Construct validation rejected a save if the owner had *any* type error, so
  an operator fed by two constructs could never get its first one — now only
  errors the edit *introduces* block a save.
- Renaming or retyping a port did not propagate into the operator's state
  machine (or activation) — it now does.
- The canvas showed generated `__sm_*` / `__act_*` internals for any
  operator owning a construct — constructs now render as single blocks.
- Found by stepping the compiled C in lockstep with the simulator (item 6):
  the simulator did not wrap sized integers on assignment (`uint8`
  200 + 100 stayed 300; the C stores 44) — it now converts on store exactly
  as C does; and it rejected enum-typed inputs outright — they now parse by
  variant name.
- Also found by it: `float32` was simulated in double precision, and a real
  literal with an integral value (`0.0`) was emitted as a C *int* literal,
  which silently kept neighbouring arithmetic in `float`. The simulator now
  computes `float32` in single precision with C's promotion rules, real
  literals are always C double literals, and both sides print reals with
  one shortest round-trip algorithm (item 6b).

**Activation semantics now match SCADE** (item 4): branches are clocked and
freeze when inactive; `last(v)` gives SCADE's `last 'v` for hold patterns.
A model written for the earlier stage-1 semantics that used `pre v` inside a
branch to mean "the previous cycle" should use `last(v)` instead.


## Recommendations

Sizes: **S** ≈ a session, **M** ≈ 2–3 sessions, **L** ≈ 4+.

### P0 — close the gaps at the centre of both goals

1. ✅ **Contract editor + mode table — done.** Contracts dialog with three
   views (assume/guarantee rows, SCADE-style mode table, editable CoCoSpec
   text), live dry-run checking that outlines offending rows, one contract
   per operator with its interface kept in step with port edits.
   *Correction to this plan:* the back end was **not** complete, as it
   first said. Building the editor exposed that contract expressions were
   never type-checked (now C0080), that five shipped library contracts
   referenced undeclared names and one promised the opposite of its
   block's behaviour (fixed, with ghost-variable support added to the
   library format), and that both runtime monitors were stateless — `pre`
   / `->` compiled to `1`, ghosts broke the C monitor's compile, and the
   simulator skipped assumptions. Monitors now run each contract's
   observer node in both the simulator and the generated C.
2. ✅ **Stateful simulation session + live values on the diagram — done.**
   A server-side session (its own thread holding the simulator's state)
   replaces replay-from-zero stepping: Step runs one more cycle, Run N a
   batch, and a run stops at a breakpoint condition (`cmd > 80`; stateless
   expressions only, `pre` is refused) or at the first contract violation.
   Every wire shows its current value, the active state and the fired
   activation branch are highlighted on canvas blocks and in the chart
   dialogs, and the contract chip shows the active modes and ✓ / violated
   clauses. Model edits mark the session stale (a hash of the model with
   layout stripped) and the next Step restarts it. The simulator gained
   an incremental `step_observed` API that the batch CSV runner now uses
   too, so the dock, `simulate` and the tests share one code path.
3. ✅ **Waveform viewer — done.** One component, three uses. The
   simulation session's cycles as live lanes (digital, stepped analog, bus,
   and a contract-check status lane; stops marked), where picking a cycle
   reviews it on the diagram and charts. Each test scenario's IR or C run
   against its golden, the golden dashed underneath, differing cells
   banded with the expected value and the first divergence marked. Kind 2
   counterexamples, with the falsifying cycle marked and the text table
   kept one click away. Scenarios and counterexamples replay in the live
   simulator (`/api/sim/step` takes a per-cycle `sequence`), so a failure
   can be stepped through on the diagram. Only the visible window is drawn,
   so a 10000-cycle session redraws in milliseconds; a Table view shows
   the same data for readers who don't want the chart.

### P1 — SCADE semantics and code-generation quality

4. ✅ **Clocked activation — done.** Each branch runs on its own nested
   clock (`when not b1 … when gk`), every variable it reads is sampled onto
   that clock by a local that holds while the branch is inactive, and the
   outputs merge back — so `pre` / `->` / stateful calls inside a branch
   freeze exactly as in SCADE. `last(v)` / `last(v, init)` provides SCADE's
   `last 'v`. The simulator, the generated C (byte-identical traces, and in
   lockstep under C in the loop) and the Lustre for Kind 2 (clocked locals
   declared `x: int when c`) all run the same lowered clocks.
5. ✅ **Model-to-code traceability — done.** Lowering records which owned
   construct each equation came from; the generated C carries a one-line
   ASCII `@trace` comment per equation (operator, diagram element, construct
   role such as "branch Engaged computes cmd", model text); the trace matrix
   gives each equation's line range; the generation report lists files with
   SHA-256, operators (interface, step function, state fields, sub-instances,
   constructs) and coverage. `emit-clite` writes `trace.json` and
   `generation_report.{md,json}`; the Studio navigates C ↔ model both ways
   and shows the report (Code ▸ Generation Report).
6. ✅ **C-in-the-loop stepping — done.** A "C in the loop" toggle compiles
   the simulated operator (reusing the build while the model is unchanged)
   and runs its CSV driver as a child process, fed the same inputs each
   cycle. Outputs and contract-monitor columns are compared every cycle
   (enums by name, reals exactly — see 6b); the waveform draws the C
   as each lane's reference, the watch table gains a C column, the diagram
   shows "≠ C value" under a disagreeing output, and a Run stops at the
   first divergence. Attaching mid-session replays the history first. It
   found two simulator bugs on its first runs (above) and one open gap
   (`float32`, above).
6b. ✅ **Float32 fidelity — done.** The simulator has a single-precision
   real: `float op float` computes in `float`; a `float64` operand or a real
   literal promotes to `double` (C's usual arithmetic conversions); `if` /
   `->` / `merge` take their branches' common type like `?:`; storing,
   passing arguments, reading inputs and evaluating constants convert to
   the declared type. The C emitter writes real literals as C double
   literals (`0.0` used to come out as the int `0`), the CSV driver reads
   `float32` inputs with `strtof`, and both sides print reals with one
   shortest round-trip positional algorithm — so traces stay byte-identical
   and C in the loop compares reals exactly. The integrator that drifted
   now runs 10 000 cycles in lockstep; a filter / all-float / float64 /
   cast / call mix matches over 3000 cycles (`tests/float_fidelity.rs`).
7. **Target integration (M).** Cross-compilation toolchains (the Compile
   dialog's disabled option), cyclic-task wrapper templates (bare-metal loop,
   RTOS task), configurable symbol prefixes.

### P2 — graphical depth

8. **Auto-layout (M).** A layered, dependency-ordered "Arrange" for imported
   Lustre and large operators; today new items stack in one column.
9. **Graphical authoring of state machines and activations (L).** Draw
   states, transitions and branches on the charts themselves (states already
   drag and persist); keep the textual editors as the power path.
10. **Canvas annotations, multiple diagrams per operator (S–M).**
11. **Graphical contract/observer blocks (M)** — assume/guarantee placed on
    the canvas, linked to contract clauses (after item 1).

### P3 — evidence and tooling

12. **Provision Kind 2 (S–M).** Detect and guide installation (WSL/Docker on
    Windows), bundle on Linux/macOS releases, add a CI job that proves the
    examples; expose realizability and mode-coverage in the Verify dock.
13. ✅ **Evidence report — done.** One document per operator — the plan's
    "evidence layer": identification (interface, contract, model files'
    SHA-256, a layout-independent fingerprint of the operator's slice),
    static checks, the contract in CoCoSpec, the Kind 2 proof, tests with
    decision and MC/DC coverage, model ≡ generated code (compiler identity,
    flags, cycles compared), and traceability (generated files' SHA-256,
    trace matrix). Each section is pass / gaps / fail / not run, with an
    overall verdict. `openlustre evidence` writes standalone HTML (print to
    PDF) and JSON and exits non-zero on FAIL; the Studio shows it under
    Project ▸ Evidence Report.
14. **Imported C operators in the GUI (M).** Register a manifest, place it as
    a block, see its contract.

### Deliberately not recommended now

- **Rewriting the GUI on Tauri + ReactFlow** (the plan's long-term stack).
  The in-binary SPA is working, tested, and shipping; a stack switch now is
  churn that delivers none of the items above.

## Suggested next step

Item 13 is done: an operator's checks, contract, proof, tests and coverage,
model ≡ code, and traceability now come out as one fingerprinted document,
and CI can gate on its verdict.

The most valuable next step is item **12** (provisioning Kind 2): the
evidence report's proof section is "not run" wherever Kind 2 is missing —
which is most machines, and this one — and the clocked Lustre the
activations now lower to has not yet been proved in CI. After it, item **7**
(target integration: cross-compilation, cyclic-task wrappers) is the last P1
item, and item **9** (drawing states and branches directly on the charts)
the biggest remaining graphical gap with SCADE.
