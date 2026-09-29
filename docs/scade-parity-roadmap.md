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
| Conditional activation (activate-if) | ✅ stage 1 (selected, not clocked) | ✅ editor, decision-tree chart, canvas block |
| Type checking, function/operator rules | ✅ | ✅ errors mapped onto boxes and wires |
| **CoCoSpec contracts: assume / guarantee / modes** | ✅ IR, checker (now type-checks clauses), CoCoSpec emit, observer-based runtime monitors | ✅ **contract editor: clause rows, mode table, CoCoSpec text, live checking** (was read-only) |
| Lustre + CoCoSpec export | ✅ | ✅ Lustre pane |
| C-Lite generation (selected root + closure) | ✅ | ✅ Generate / C pane / save files |
| Compile & run | ✅ host compiler, CSV driver, Makefile | ✅ host only; cross-compile shown as "roadmap" |
| Stepping / simulation | ✅ batch + full trace | ⚠️ watch table + trace table; **every Step replays the whole history** (no session) |
| IR ≡ compiled-C trace equivalence | ✅ `test run --backend both` | ✅ Tests dock (with decision + MC/DC coverage) |
| Kind 2 proof | ✅ adapter; bmc-ind / realizability / mode-coverage modes | ⚠️ default mode only; counterexample as an ASCII block; **Kind 2 not bundled or in CI** |
| Evidence report | ❌ | ❌ |
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

**Known semantic gap (documented):** activations are stage 1 — branches are
*selected*, not *clocked*. `pre` inside a branch reads the previous cycle
(like SCADE `last`), not the branch's previous activation. Models that rely
on frozen inactive branches will behave differently than in SCADE.

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
2. **Stateful simulation session + live values on the diagram (M).** A
   server-side `Sim` session (step / run N / reset) replaces replay-from-zero
   stepping. With it: current values annotated on every wire, the active
   state highlighted on state-machine blocks and charts, the selected branch
   highlighted on activation blocks and trees, conditional breakpoints
   (`break when <bool expr>`) and run-until. This is the SCADE Simulator's
   signature experience and the direct answer to "step and simulate models
   running".
3. **Waveform viewer (M).** Signals as digital/analog lanes over cycles, used
   for step traces, test-scenario diffs (IR vs C divergence highlighted at the
   first differing cycle), and Kind 2 counterexamples (replacing the ASCII
   block).

### P1 — SCADE semantics and code-generation quality

4. **Clocked activation, stage 2 (M–L).** Lower branches onto clocks
   (`when` / `merge`, or condact-style) so inactive branches freeze, matching
   SCADE; keep the byte-identical IR-vs-C test as the gate.
5. **Model-to-code traceability (M).** Per-equation comments in the generated
   C naming operator / equation / diagram element, a machine-readable trace
   matrix, and a generation report (files, operators, state sizes). This is
   what KCG users expect and what DO-178C-style reviews need.
6. **C-in-the-loop stepping (M).** Step the compiled executable next to the
   IR simulator in the Simulation dock, flagging divergence live (the batch
   comparison already exists in `test run --backend both`).
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
13. **Evidence report (M).** One HTML/PDF per operator: type and contract
    checks, proofs, coverage (decision, MC/DC), IR-vs-C equivalence, trace
    matrix — the plan's "evidence layer".
14. **Imported C operators in the GUI (M).** Register a manifest, place it as
    a block, see its contract.

### Deliberately not recommended now

- **Rewriting the GUI on Tauri + ReactFlow** (the plan's long-term stack).
  The in-binary SPA is working, tested, and shipping; a stack switch now is
  churn that delivers none of the items above.

## Suggested next step

Item **1** is done. Next is item **2** (a stateful simulation session with
live values on the diagram), which completes the SCADE design → generate →
run → simulate loop — and pairs naturally with contracts: the per-cycle
active mode and violations the monitors now compute correctly can be shown
live on the canvas.
