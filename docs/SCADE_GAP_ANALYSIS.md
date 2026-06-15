# OpenLustre Studio vs. Ansys SCADE Suite — gap analysis

*Updated 2026-06-13.*

OpenLustre Studio aims to be the open, SCADE-shaped workbench: author synchronous
dataflow models graphically, check them, simulate them deterministically, prove
contracts, generate C, and test the generated code against the model. SCADE Suite
is the industry-accepted, DO-178C-qualified original. This document is honest about
which gaps are **bridgeable engineering work** and which are **structural** (you
cannot code your way to a qualification certificate), and prioritizes the former.

## 0. Status snapshot — resume here

**Repo**: `C:\Users\Jonathan\Projects\OpenLustreStudio` (Rust workspace, branch
`main`). **Full check**: `cargo test --workspace --no-fail-fast` (56 result
groups green as of 2026-06-13 on Windows/MSVC). The Studio GUI is one embedded
HTML page, `crates/ol_cli/src/studio_ui.html`, served by
`crates/ol_cli/src/studio_server.rs` (`openlustre studio serve <dir>`); the IR
is `crates/ol_ir`, sim `crates/ol_sim`, C emitter `crates/ol_clite_emit`,
typecheck `crates/ol_typecheck`.

**Landed recently (newest first):** held-input debug runs; the SCADE build
pipeline (Build → Run Simulation → Generate C-Lite → Compile & Run) with
`<operator>.lus` written on a clean build, the Lustre pane gated on build, and
**log messages** (debug probes printed every 50 cycles); canvas select /
multi-select / right-click-menu / Delete-key; the two-column simulation
watch/set table with per-type validation; SCADE gates (input pins left, output
right) + pin-to-pin wiring; MC/DC coverage; array iterators (`map`/`fold`);
boolean clocks (`when`/`merge`); undo/redo; properties dock; constants; block
symbols; typed wire labels.

**Best next gaps (pick up here):**
1. **Float intrinsics** (P1, small, self-contained) — un-grey `square_root` and
   add `sin/cos/abs/min/max…` as a float-intrinsics family agreeing across sim,
   generated C (`<math.h>`), and the Kind 2 view. Mirrors the `numeric_cast`
   pattern. Good first slice in a fresh session.
2. **Hierarchical / parallel automata** (P1, large, structural) — today's FSMs
   are flat Moore-style (`crates/ol_ir/src/state_machine.rs` lowers them);
   SCADE automata nest, run in parallel, and carry history/signals.
3. **Tool Operational Requirements document** (P1 if certification-adjacent) —
   the last piece of the verification-by-equivalence story (§4); pure docs, the
   test suite already being the verification evidence.
4. **Editor polish** (P1/P2) — orthogonal (Manhattan) wire routing, zoom/pan,
   copy/paste, distinct per-family gate silhouettes (§2).
5. **Deployment** (§5) — `.lus`/`.ols` file association + app icon (P1,
   cosmetic), then code signing (P2, cost not code).

Everything ships across all stages — IR → typecheck → sim → generated C →
dual-backend equivalence test — or it isn't done. The §6 log records each slice.

## 1. Where the products stand today

| Workflow step | SCADE Suite | OpenLustre Studio today |
|---|---|---|
| Graphical authoring | Full diagram editor: palette drag-drop, pin-to-pin wire drawing, hierarchical sheets | Drag-drop palette, **SCADE gates with left input pins / right output, pin-to-pin wiring**, draggable grid-snapped canvas with persisted layout, multi-select + right-click menu + Delete, red invalid-link coding |
| Language | Scade 6 (Lustre core + clocks, automata, iterators, packages) | Lustre subset + **boolean clocks (`when`/`merge`)** + **array iterators (`map`/`fold`)**: dataflow, `pre`/`->`, records/enums/arrays, constants, flat FSMs (lowered), imported C operators |
| Static checks | Type/clock checker | Type checker + **clock calculus** + contract checker (vacuity, unreachability, overlap), live in the GUI |
| Simulation | Cycle stepping, watch, plots, co-simulation | **Two-column watch/set table** (sticky typed inputs, computed locals/outputs), full per-item trace, CSV batch simulation, golden-trace scenarios |
| Formal verification | Design Verifier (Prover plug-in) | Kind 2 adapter (BMC/induction, realizability, mode coverage) + CoCoSpec contract emission, in-GUI Verify tab |
| Build & codegen | KCG qualified C/Ada (TQL-1) | **Build pipeline** (validity check → `<op>.lus` → C-Lite → debug run in a terminal), C-Lite emitter + contract monitors + CSV driver + Makefile + **log-message probes**, selected-root slicing |
| Testing | SCADE Test: harness, MTC, MC/DC on model | Scenario harness: golden traces against IR simulator **and** compiled C; decision coverage **and unique-cause MC/DC** with uncovered reporting |
| Deployment | Commercial installer suite | Inno Setup Windows installer, Start Menu/Desktop shortcuts, embedded stdlib, Linux install script, CI release workflow |
| Qualification | DO-178C/DO-330 qualification kits, 20+ years of certification credit | None (see §4) |

## 2. Diagram editor gaps (the visible difference)

SCADE's editor is the product for most users. The canvas now has: persisted free-form
layout, **grid tracking with snap-to-grid** (the grid pitch is stored in the model
file next to the positions — the role SCADE's layout metadata files play), red
color-coding of invalid links/boxes with hover reasons, hierarchical dive
navigation, and an unmappable-problems banner. Remaining gaps, in priority order:

| Gap | What SCADE does | What we need | Effort |
|---|---|---|---|
| ~~P0 — Palette drop~~ | Drag a library block onto the canvas to instantiate it | **Landed 2026-06-11**: drag a palette chip onto the canvas → placed call equation with fresh typed output locals and red unbound pins | done |
| ~~P0 — Edit/delete in place~~ | Double-click a block to edit; delete removes it | **Landed 2026-06-11**: right-click any box → properties panel (equation edit/delete, variable rename/retype/role-change/delete, ghost-pin binding) | done |
| ~~P0 — Pin-to-pin wire drawing~~ | Drag from an output pin to an input pin creates a connection | **Landed 2026-06-13**: operation blocks render as SCADE gates with one input pin per operand on the left edge (red when unbound) and an output pin on the right; dragging a source pin onto a specific input pin binds that operand. AND/OR/etc. drop with their minimum two pins and grow to twelve. See §6 | done |
| ~~P1 — Undo/redo~~ | Standard | **Landed 2026-06-11**: server edit-journal (100 deep), Edit menu + Ctrl+Z / Ctrl+Y | done |
| ~~P1 — Multi-select / delete~~ | Select several, right-click, delete | **Landed 2026-06-13**: ctrl/shift-click multi-select, right-click context menu (Properties, Delete), Delete/Backspace key; Ctrl+Z restores | done |
| **P1 — Orthogonal wire routing** | Manhattan-routed wires with junctions | Replace cubic Béziers with channel routing | Medium |
| **P1 — Zoom/pan, copy/paste** | Standard | SVG viewBox transforms + selection-rectangle marquee + clipboard | Medium |
| **P2 — Multi-sheet diagrams** | One operator can span sheets | Page list per node in `DiagramLayout` | Medium |
| **P2 — Per-family gate silhouettes** | Distinct shapes per operator family (gates, delays, switches) | Gates now render as blocks with pins; SCADE's curved-AND / D-shaped-OR silhouettes are still a flat box — a symbol library keyed by operator id | Small, cosmetic |

## 3. Language and toolchain gaps

| Gap | Notes | Priority |
|---|---|---|
| Source spans in diagnostics | `Diagnostic.span` exists but is never populated. Honest re-scope: models are GUI-authored JSON, so the `node X · equation N` context (landed) already pins every diagnostic to its diagram box — file:line:col only becomes meaningful with a textual `.lus` frontend, which is itself roadmap | P2 (was P0) |
| ~~Clocks (`when` / `merge`)~~ | **Landed 2026-06-12**: boolean clocks end to end — `e when c` / `e when not c` / `merge(c, a, b)` in IR, parser, formatter, clock calculus (E0130–E0135), simulator, generated C, Kind 2 view (V6 merge-case syntax), and the Time/Statefuls toolbox. See §6 | done |
| Hierarchical/parallel automata | Our FSMs are flat Moore-style; SCADE automata nest, run in parallel, carry history and signals | P1 |
| ~~Array iterators (`map`/`fold`)~~ | **Landed 2026-06-12**: `map(F, a…)` / `fold(F, init, a)` over a stateless function, end to end — IR, parser/formatter, typecheck (E0140–E0146), element-wise simulation, generated C (`for` loops), array CSV I/O at the boundary, and the Higher Order toolbox. Dual-backend equivalence test passes on MSVC. Clocked/stateful iteration and Kind 2 iterator proving remain roadmap. See §6 | done |
| ~~MC/DC proper~~ | **Landed 2026-06-12**: unique-cause Modified Condition/Decision Coverage (DO-178C Level A) on the decision-coverage substrate — decisions are if-conditions and compound boolean equations; each atomic condition's value is captured in a single eval pass; suite-level independence-pair analysis reports which conditions still lack an isolating test, surfaced in `test run` and the Studio Tests dock. Unique-cause only (coupled conditions reported uncovered); masking MC/DC is roadmap. See §6 | done |
| Model diff (`openlustre diff`) | Semantic, not textual, diff of two model files — config management story | P2 |
| Requirements traceability | Annotate nodes/contracts with requirement IDs; emit a trace matrix (CSV/ReqIF) | P2 |
| Documentation generator | Render IR + diagrams + contracts to a design-document HTML/PDF | P2 |
| FMU export | Co-simulation entry ticket for the broader MBSE world | P3 |

## 4. The structural gap: qualification

SCADE KCG is qualified to TQL-1 under DO-330 — its generated code can be used in
DO-178C Level A software with reduced verification. No amount of feature work closes
this; qualification is a process/evidence artifact, not a code artifact.

The honest strategy for OpenLustre Studio is **verification-by-equivalence instead of
qualification-by-pedigree**, and it is already half-built:

1. **Dual-backend execution** — every test scenario runs against the IR simulator and
   the compiled generated C, cell-by-cell (done).
2. **Formal contract proofs** — Kind 2 proves the model's contracts; monitors compile
   the same contracts into the C so violations are observable at runtime (done).
3. **Coverage evidence** — decision coverage **and unique-cause MC/DC** (the DO-178C
   Level A metric) measured on the IR backend and reported per condition (done 2026-06-12;
   masking MC/DC for coupled conditions remains roadmap).
4. **Tool Operational Requirements document** — enumerate what the tool claims to do,
   with the test suite as verification cases (not started; pure documentation work,
   P1 if certification-adjacent use is a goal). With MC/DC landed, this is now the
   single remaining piece of the verification-by-equivalence story.

That story positions the tool as: *generated code you independently verify*, which
is a legitimate (if more laborious) DO-178C path where the applicant carries the
verification burden the qualified tool would otherwise discharge.

## 5. Windows deployment gaps

| Gap | Notes | Priority |
|---|---|---|
| File association | `.ols`/`.json` model double-click should open the Studio (registry entries in the installer) | P1 |
| App icon | The shortcut currently uses the default exe icon | P1, cosmetic |
| Code signing | Unsigned installers trip SmartScreen; needs a cert (cost, not code) | P2 |
| winget/MSIX distribution | `winget install OpenLustreStudio` once the repo publishes releases | P2 |
| Auto-update check | Studio could poll GitHub releases and show a banner | P3 |

## 6. What closed recently

### 2026-06-13 — debug runs use the watch-table inputs

The debug run (pipeline step 4) no longer holds inputs at zero: it holds each
input at the value the engineer set in the simulation watch table. `pipeRun`
sends those sticky values; `emit_debug_driver` parses each through
`c_literal` (a value is parsed to a *safe* C literal — bool/int/float only —
so no user text reaches the generated source verbatim) and emits
`in.<name> = <literal>;`, falling back to the memset-zero default when unset or
unparseable. So a `Doubler` with `x = 9` prints `initial inputs: x=9` and
`step 0 | y=18`. Injection-safety and the held-vs-default behaviour are
unit-tested.

### 2026-06-13 — the SCADE build pipeline + log messages

A four-step pipeline in the Build dock, gated like SCADE's:

* **1 · Build Model** is the model-checker step: type- and contract-check, and
  on a clean check emit the main operator's Lustre (root + dependencies) and
  write it to `<main>.lus` in the project folder. The Lustre pane is empty
  until then — *the model code exists only after a successful build* — and any
  edit re-locks the gate (a content fingerprint detects the change).
* **2 · Run Simulation** is gated on a successful build (you cannot simulate an
  unbuilt operator), **3 · Generate C-Lite** shows the generated C, and
  **4 · Compile & Run (debug)** compiles with `-DOL_DEBUG` and launches the
  executable in its own terminal window — a free run that prints a banner, the
  held inputs, and the outputs plus log messages every 50 cycles.
* **Log messages** (SCADE's debug probes): `NodeDef.probes` = `{label, var}`,
  added via Insert ▸ Log Message, type-checked (the var must exist, E0150),
  and emitted as a `printf` inside `_step` under `#ifdef OL_DEBUG` — so a
  probe prints `label: value` in the debug run while production C and the
  dual-backend equivalence tests never see it.

### 2026-06-13 — authoring & simulation ergonomics

Three round-two GUI gaps from live demo feedback:

* **Select / delete on the canvas.** Selection is now a set — ctrl/shift-click
  to multi-select, click empty to clear. Right-click opens a context menu
  (Properties, Delete) and **Delete/Backspace** removes the selected item(s):
  equations via `remove_equation` (descending index), variables via
  `remove_port`; ghosts are symptoms and skipped; Ctrl+Z restores.
* **Two-column simulation watch/set table.** The step view is a SCADE-style
  table: column 1 is `name : type` for every input/local/output, column 2 is
  an **editable, sticky** value for inputs (it holds across cycles until you
  change it) and a computed read-only value for locals/outputs. Every typed
  value is validated against its type — a bool only takes true/false, an int8
  stays in −128…127, a uint can't go negative — so the simulator never gets a
  value its type can't hold. (The 5 s inspect poll no longer wipes a running
  sim's computed cells.)
* **Cleaner unbound pins.** A gate's unbound input reads as “input N — needs a
  source” rather than exposing the internal `p0_1` placeholder; binding still
  happens by wiring the pin or, as the user prefers, typing the expression in
  Properties.

### 2026-06-13 — SCADE gates: input pins on the left, output on the right

The canvas drew every element — ports, operations, outputs — as the same
rectangle with one right-edge pin; an operation's operands were separate
floating boxes. Now operation blocks render as SCADE gates:

* `build_diagram` emits an ordered `inputs` array per equation (one pin per
  operand free variable; global constants stay inlined), and tags each bound
  operand's wire with its `to_port` index.
* The canvas seats those pins on the block's left edge, growing the gate's
  height to fit them, and routes each incoming wire to its specific pin. An
  unbound operand is a **red pin on the gate** (not a floating ghost box);
  the output pin stays on the right. So a dropped AND shows its minimum two
  input pins immediately and grows to twelve via the Properties control.
* Dragging a source pin onto a specific input pin binds *that* operand
  (rewiring or filling an unbound pin); dropping on the block body still
  binds the first free pin. The whole loop — create an operator, drop
  comparison/AND gates, wire their pins, and generate Lustre that compiles to
  C-Lite — is covered end to end by a new studio-API test.

### 2026-06-12 — array iterators (`map` / `fold`), vector-heavy modelling

* **The language**: `map(F, a₁…aₖ)` applies a stateless function element-wise
  across same-length arrays to build an array; `fold(F, init, a)` left-reduces
  an array to a scalar (`F` is `(accumulator, element) -> accumulator`). The
  iterated `F` is a **function** — no per-element state in this profile — and an
  iterator is always the whole right-hand side of its equation, so codegen is a
  single `for` loop and `map`'s array result has a clean home.
* **One representation, every stage**: `Expr::Iterate { kind, node, init,
  arrays }` flows through the parser/formatter (round-trips; Kind 2 view is the
  same surface text — iterator *proving* is roadmap), the typechecker
  (E0140–E0146: unknown/stateful/many-output `F`, non-array operands, unequal
  lengths, argument-type and arity mismatch, nesting), the simulator
  (element-wise application, building `Value::Array` for map / threading the
  accumulator for fold), and the C emitter (a guarded-free `for` loop calling
  `F_step` per element).
* **Arrays at the boundary**: the IR simulator and the generated CSV driver now
  read and write arrays as `[e0;e1;…]`, so array-interface nodes are testable.
  This unblocked the **dual-backend equivalence test**: a saturating-scale `map`
  and a sum `fold` over `int32[4]`, IR vs MSVC-compiled C, agree cell-by-cell.
* **A soundness fix the feature surfaced**: `slice_for_root` followed `Call`
  targets but not iterator function references, so generated C for a sliced root
  dropped the iterated function. Now both are kept (regression-tested).
* **Authoring**: `map(F)` / `fold(F)` are enabled in the Higher Order toolbox
  with pin contracts; the drop reads `F`'s signature to type the result local
  (an `int32[N]` for map, the accumulator type for fold) and renders a divable
  `map(F)` / `fold(F)` block.

### 2026-06-12 — boolean clocks (`when` / `merge`), the biggest language gap

* **Clock calculus** (`ol_ir::clocks`, shared by typecheck/sim/codegen so all
  three agree): every expression runs on the base clock or a chain of boolean
  conditions; `e when c` / `e when not c` samples down, `merge(c, a, b)` joins
  complementary streams back up. Conditions are variable names (the classic
  restriction). Locals infer their clock from their defining equation;
  inputs/outputs stay base-clocked. Violations are E0130–E0135, pinned to
  their equations: mixed-clock operands, non-bool clocks, clocked outputs
  (with a "use merge" hint), clocked equations in functions, clocked arrays.
* **Semantics: held values + first-tick temporals.** A clocked equation runs
  only on its clock's active cycles; its lhs holds the last value through
  inactive ones (the deterministic watch-view trace). A clocked `->` counts
  ticks of *its* clock — `0 -> pre cnt + one` on clock `tick` yields 0 on the
  first true cycle of `tick`, whenever that arrives. The simulator tracks
  per-chain tick counts; the generated C mirrors it with guarded equation
  blocks, `held_*` state fields, and per-chain `clkN_ticked` flags. The
  dual-backend scenario test pins IR and compiled C cell-by-cell on gating
  patterns (off-start, bursts, long holds).
* **Authoring**: `when` / `when not` / `merge` blocks in the Time/Statefuls
  toolbox family with pin contracts; WHEN/WHEN¬/MERGE diagram symbols; the
  Kind 2 view emits Lustre V6 merge-case syntax.
* Conscious v1 limits (all loud, none silent): stateful calls finer than
  their equation's clock are rejected (E0133), clocked arrays are roadmap
  (E0135), and merge branches don't count toward decision coverage yet.
* **Also this session**: operation pin contracts in the toolbox + variadic
  input counts (2..=12) for the associative operations, resizable from the
  Properties sheet (`/api/edit/set_operation_inputs`, journaled in undo).

### 2026-06-11

* Windows 11 is now a first-class build/test platform: the scenario harness's C
  backend discovers MSVC via `vswhere`/`vcvars64.bat` when no `cc`/`gcc`/`clang`
  exists, so the dual-backend equivalence tests run on a stock Visual Studio machine.
* The Inno Setup installer builds and installs on real Windows (per-user Inno Setup
  locations supported).
* Invalid/problematic links render **red** with hover reasons: undeclared names
  become dashed red ghost boxes, typecheck errors land on the equation boxes and
  defining wires they belong to, never-assigned outputs go red, and unmappable
  errors show in a red banner above the canvas.
* The canvas is grid-tracked: engineering grid rendering, snap-to-grid dragging, and
  the grid pitch persists in the model file's diagram metadata with the positions.
* The GUI was restyled to the SCADE Suite shape: docked Workspace tree, MDI-style
  document tabs, bottom Messages dock (click a message to select the node), toolbar,
  and status bar.

### Fourth slice, same day — declutter, dialog fix, end-to-end walkthrough

* Canvas header clutter removed: grid size / show grid / snap to grid moved
  into a **View** menu (with keep-open items, native mouse-down menu
  behavior, hover-switching); the instructional paragraph is gone; layout
  status moved to the status bar.
* Fixed: modal dialogs never displayed (`style.display = ""` fell back to
  the stylesheet's `none`) — this is why operators could not be created
  from the menus. Also fixed a diagram-load race that left the old canvas
  on screen after creating an operator.
* Verified the **complete developer loop through real UI event paths**:
  Insert > Operator (AvgFilter) → Insert > Input a/b : int32, Output
  avg : int32 → drag `plus` from the toolbox, bind its red pins to a/b via
  right-click → drag `divide`, edit to `plus0 / 2`, route to `avg` →
  File > Set Main → Simulation Build/Run/Step (a=10, b=20 ⇒ avg=15) →
  Code > Compile C-Lite ⇒ `AvgFilter.exe`, which computes correct averages
  from CSV on stdin. The tool authors, simulates, generates, and compiles
  real C from a blank workspace without touching a text editor.

### Fifth slice — Properties dock, constants, symbols, wire labels; the
### evaluation-order soundness fix

* **Properties dock** (SCADE's bottom-right pane): clicking any canvas box
  selects it (blue highlight) and the docked pane shows its sheet — name,
  data type (dropdown of primitives + named types), usage
  (input/output/local), and **default value**, which maps honestly onto the
  model: it is the variable's constant defining equation (created on first
  entry, edited thereafter; "defined by eqN" when the variable is computed;
  "driven by the environment" for inputs). Equations edit lhs/expression;
  red pins bind — all in the dock, the floating panel is gone.
* **Constants on canvas** (was P0): `constant (literal)` heads the
  Mathematics family; dropping prompts for the value and lands a typed
  literal source block (`2` → int32, `2.5` → float64, `true` → bool).
* **Block symbols**: equations with recognizable shapes draw as compact
  SCADE-style blocks — `+ − × / mod`, comparisons, `AND OR XOR NOT ⇒`,
  bitwise, `FBY` (init→pre), `->`, `pre`, `ITE`, cast targets, callee names,
  constant values — full equation text on hover. Free-form bodies keep the
  wide text box.
* **Typed wire labels**: every wire carries its variable and type
  (`n: int32`), toggleable in the View menu — the `_L2: bool` look.
* **Soundness fix found by drawing**: stepping a freshly drawn counter
  exposed that both the IR simulator *and* the generated C walked equations
  in **declaration order**, silently reading stale zero-defaults for forward
  references (`n = constant1 + …; constant1 = 1;`) — the exact shape canvas
  drops produce. Both backends had agreed on identically wrong traces. New
  `ol_ir::evaluation_order` (same-cycle reads, `pre` excluded, both `->`
  arms included) now drives the simulator (entry + called nodes) and the C
  emitter; true combinational cycles are a loud simulator error instead of
  a wrong answer. Four regression tests pin it, including golden-content
  checks so the dual-backend comparison can never again "pass" on matching
  wrong values.

### Alignment gaps visible against a real SCADE Suite session (screenshot-reviewed)

| Gap | SCADE | Ours today | Priority |
|---|---|---|---|
| ~~Constants on canvas~~ | Droppable literal source block | **Landed** — `constant (literal)` in the toolbox | done |
| ~~Block symbols~~ | Gates, comparators, FBY draw as distinct shapes | **Landed** — compact operator blocks, text on hover | done |
| ~~Typed wire labels~~ | Every wire named and typed inline (`_L2: bool`) | **Landed** — `name: type` labels, View-menu toggle | done |
| ~~Properties dock~~ | Persistent bottom-right pane for the selection | **Landed** — name/type/usage/default value sheets | done |
| ~~Edit menu, undo/redo~~ | Standard | **Landed** — server edit journal (100 deep, model + types files snapshotted per edit), Edit menu + Ctrl+Z / Ctrl+Y | done |
| ~~Pin-to-pin wire drag~~ | Drag output pin to input pin to connect | **Landed** — pin handles on every box; variable→block binds the first red pin, block→output rewires the result and sweeps the orphaned local | done |
| ~~Tree organization~~ | Operators folders per package; libraries as tree roots | **Landed** — collapsible packages with Operators/Contracts folders, stdlib under a Libraries root | done |
| ~~Output dock: Build tab~~ | Compile output is a dock tab | **Landed** — compile logs land in the Build dock | done |
| MDI document tabs | Several diagrams open side by side | One diagram at a time + breadcrumbs | P2 |
| Icon toolbars | Multiple icon strips under the menus | Menus only | P3, cosmetic |

### Industry-deployment hardening (sixth slice)

* **Generated C is identifier-safe**: model variables named `unsigned`,
  `for`, `double` (any C keyword, or the generated `in`/`out`/`self`
  parameter names) mangle with a trailing underscore at every emission
  site — structs, locals, references, state fields, driver, monitors —
  while CSV headers and traces keep the model's own names. Pinned by a
  dual-backend test with hostile names.
* **Every edit is undoable**: the server journals file snapshots before
  each successful mutation; undo/redo round-trips are tested, a new edit
  clears the redo branch, and empty stacks report instead of corrupting.

Remaining for an industry deployment story (unchanged priorities):
source spans in diagnostics, clocks, hierarchical automata, array
iterators, MC/DC masking analysis, float intrinsics, requirements
traceability, code signing, and the Tool Operational Requirements
document for certification-adjacent use.

### Third slice, same day — the SCADE shell: menus, toolbox, numeric_cast

* **Menu-driven GUI**: the tab strip is gone. File / Insert (Operator, Input,
  Output, Local, Equation, State Machine) / Simulation (Build, Run, Step —
  gated until a simulation is running — Stop, CSV vector) / Code (View Lustre,
  View Generated C, Generate C-Lite, Compile C-Lite…) / Project (Types, Tests,
  Verify, State Machines). One diagram document in the center; Messages /
  Simulation / Tests / Verify are bottom dock tabs; the **Lustre text of the
  model is always visible** in the right dock while drawing.
* **Operations toolbox** (right dock): the SCADE operator families —
  Mathematics (plus, minus, multiply, divide, modulo, numeric_cast, squared,
  cubed, to_nth_power(n); square_root listed but disabled pending float
  intrinsics), Comparisons, Logical, Structures/Arrays, Time/Statefuls,
  Choice, Bitwise, Higher Order (map/fold disabled — iterators are roadmap) —
  plus the project's operators and the 41 library blocks. Everything drags
  onto the canvas and lands as a placed equation with a typed result local
  and red unbound pins.
* **numeric_cast is a first-class IR operator**: surface syntax `int16(x)` /
  `float64(x)`; typechecked (numeric→numeric only, E0093/E0094); simulated
  with C semantics (two's-complement narrowing, float→int truncation,
  float32 rounding); generated C emits a real cast — IR and compiled C agree
  cell-by-cell in the test suite. The Kind 2 view emits `int_cast`/`real_cast`
  function calls (the bit_and convention) since unbounded-int Lustre cannot
  express widths.
* **Compile C-Lite from the GUI**: `Code > Compile C-Lite…` emits the
  generated sources + Makefile to a chosen directory and compiles them with
  auto-detected or selected compiler (MSVC via vcvars64, gcc/clang/cc);
  target OS is the host (cross-compilation noted as roadmap).
* Known small gaps recorded: variable names colliding with C keywords (e.g.
  `unsigned`) break generated-C compilation — needs an identifier mangling
  pass (P1); `square_root` and friends need a float-intrinsics family across
  sim/C/Lustre (P1).

### Second slice, same day — the SCADE project workflow

* **Workspaces**: opening a directory (`openlustre new <dir>` or `studio launch
  <dir>`) creates the project folder — `project.json` (starter operator),
  `types.json`, `scenarios/` — and serves it as one project.
* **Types file**: a Types tab defines enums, structures (records), and array
  aliases; definitions save into `types.json` and reach the model through
  `includes`. Uniqueness is validated against everything loaded.
* **Defined data types everywhere**: port/local type selectors list the full
  primitive set (`int8`–`int64`, `uint8`–`uint64`, `float32/64`, `bool`) plus
  every named type, defaulting to `bool` until changed; arrays via custom
  entry (`uint8[4]`).
* **Variable options**: right-click a variable on the canvas → rename (all
  uses rewritten via IR-level `Expr::rename_var`), retype, change role
  (input/output/local — "treat this local as an output"), or delete (readers
  ghost red rather than failing silently).
* **Draw on canvas**: drag operators/blocks from the diagram palette onto the
  canvas; the instance lands at the drop point with typed fresh outputs and
  red unbound input pins; right-click a red pin → Bind to wire it.
* **Per-equation diagnostics**: the typechecker now tags equation-level
  errors with `node X · equation N`, so the diagram pins *any* in-equation
  error (even ones naming no variables) to the exact box.
