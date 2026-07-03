# OpenLustre Studio vs. Ansys SCADE Suite — gap analysis

*Updated 2026-06-16.*

OpenLustre Studio aims to be the open, SCADE-shaped workbench: author synchronous
dataflow models graphically, check them, simulate them deterministically, prove
contracts, generate C, and test the generated code against the model. SCADE Suite
is the industry-accepted, DO-178C-qualified original. This document is honest about
which gaps are **bridgeable engineering work** and which are **structural** (you
cannot code your way to a qualification certificate), and prioritizes the former.

## 0. Status snapshot — resume here

**Repo**: `C:\Users\Jonathan\Projects\OpenLustreStudio` (Rust workspace, branch
`main`). **Full check**: `cargo test --workspace --no-fail-fast` (56 result
groups green as of 2026-06-16 on Windows/MSVC). The Studio GUI is one embedded
HTML page, `crates/ol_cli/src/studio_ui.html`, served by
`crates/ol_cli/src/studio_server.rs` (`openlustre studio serve <dir>`); the IR
is `crates/ol_ir`, sim `crates/ol_sim`, C emitter `crates/ol_clite_emit`,
typecheck `crates/ol_typecheck`.

**Landed recently (newest first):** **Gate silhouettes (2026-07-03):**
AND/OR/XOR/NOT draw as true IEC/SCADE shapes (same bounding box, pins and
wires unchanged). Before that: **Manhattan wires (2026-07-03):**
orthogonal wire routing by default (mid-channel verticals, staggered
parallels, feedback hooks; View-menu toggle back to Béziers). Before that:
**Editor polish (2026-07-03):** canvas
zoom (Ctrl+wheel / View menu / status-bar %), middle-button pan, marquee
multi-select, and Ctrl+C/Ctrl+V copy/paste of block sub-diagrams
(server-side `duplicate_equations`: fresh `_copy` locals, internal wiring
rewritten, one journaled edit) — browser-verified. Before that:
**Float intrinsics (2026-07-03).** The
`<math.h>` double family is first-class: `sqrt sin cos tan asin acos atan
atan2 exp log log10 pow floor ceil round abs min max` as
`Expr::FloatIntrinsic`, float64-only (explicit `float64(x)` casts in/out —
E0160/E0161), agreeing across parser/formatter, typecheck, clock calculus,
IR simulation (f64 = the same libm doubles C calls), generated C
(`<math.h>` + `-lm`), contract monitors, and the Kind 2 view (function-call
convention like `bit_and`). `square_root` is un-greyed and a **Float Math**
toolbox family drops all of them; dual-backend equivalence pinned on the
exactly-rounded subset (`tests/float_intrinsics.rs`). Before that:
**State machines are operator-owned.** A
machine now belongs to exactly one operator and *is* its body: `StateMachineDef`
gained an `owner`, and lowering merges an owned machine's state/transition/output
logic into that operator's node (it drives the operator's outputs) — no separate
node, no separate tree group. The workspace tree shows operators expandable into
**Inputs / Locals / StateMachine: Name → states / Outputs**, the machine nested
under its operator and nowhere else. Created from the operator (right-click ▸ Add
State Machine, or the editor's operator picker), one per operator, I/O inherited
from the operator. Owner-less machines (stdlib library blocks like `srff`) still
lower to standalone nodes. Before that: **Hierarchical automata — authoring, by
refinement.** A state can now `refine` another machine in the editor: write
`Active: refine Spin` in the states box and, while `Active` is active, the
`Spin` machine runs nested (its states inlined as a region, names qualified per
site so they never collide, resolved live so edits to `Spin` propagate). It
validates on save (unknown/cyclic refinement reported), builds to nested
dataflow, round-trips back into the form, and the keyword preview colours the
`refine`. Before that: **Hierarchical (nested) automata — the engine.** A state can now contain nested `Region`s (sub-automata); `lower()`
walks the tree recursively — each region gets its own state enum and a state
variable that advances only while its parent state is active and restarts at
its initial state on (re-)entry (or keeps history), and every output is a
selection over the whole state tree. It lowers to the same flat dataflow, so
typecheck / sim / generated C need no changes; a hierarchical `Mode` machine
simulates correctly (restart-on-entry verified) and its C carries both region
enums. SCADE-strict "every output assigned on every path" is checked
recursively. The server accepts nested `regions` in the state-machine payload;
GUI authoring of the nesting is the next step. Before that: **Text-based
state-machine authoring** —
state machines are a first-class tree group (parallel to Types / Constants),
created and **edited** textually (states, transitions, per-state variable
equations) with default scaffolding, a keyword-coloured live preview, and
SCADE-strict "every output assigned in every state" checking. A machine is used
inside an operator as a **block**: drop it from the Operations toolbox, the
operator's inputs feed in on the left and its outputs come out on the right
(`out = TrafficLight(tick, emergency)`). The lowered node + its `_StateEnum`
type are kept out of the Operators / Types views so a machine reads as one
thing. (Deeper hierarchical/parallel automata remain — see §1.) Before that:
**Import existing Lustre** (`File ▸ Import Lustre…`) — a small Lustre frontend (`crates/ol_cli/src/lustre_import.rs`)
parses `type` / `const` / `node` / `function` declarations into the project,
delegating equation bodies and constant values to `ol_stdlib::parse_expr` and
types to `parse_type` (with the `elem^len` array form mapped). Paste or choose a
`.lus` file; the dataflow subset OpenLustre emits round-trips (an operator's own
`<op>.lus` re-imports and rebuilds), name clashes (including against stdlib) are
rejected all-or-nothing, and unsupported constructs report a located error.
Before that: **Types and Constants tree nodes**
(SCADE-style, between the operators and the libraries) — a Types node listing
the project's named types (opens the type editor), and a Constants node for
project-wide constants (`NAME : type = value`, all-caps by convention, add via a
dialog, right-click to delete; referenced in operators like any global,
`out = NAME`). Scalar constants (int/uint/float/bool) work end to end; composite
values (arrays/structs/strings) await array-literal expression parsing. Before
that: project & code-pane fixes from demo feedback — **empty new projects**
(`openlustre new --empty`, no starter
operator; the Studio serves a blank project and stays editable); a **right-click
operator menu** in the workspace tree (Build this operator / Add Input / Add
Output / Add Local / Set as Main — the discoverable "build *this* operator"
path); and **both code side-panes now gated and copyable** (Lustre appears only
after a clean Build, the generated C only after Generate C-Lite, each with a
Copy button and selectable text). Before that: a round of live-demo authoring
fixes — the workspace tree no longer auto-collapses (folder disclosure survives the
5 s inspect poll); a **Build-dock operator selector** (build any operator, not
just the root — building makes it the root so Simulate/Generate/Run follow);
**per-operator `.lus` files** (a blank `<Name>.lus` stub on create, filled when
that operator builds; the build is scoped to the operator's slice so an
unrelated broken operator can't block it); and **red I/O pins instead of ghost
boxes** (a dropped operation shows red "needs a source" pins on the left and a
red "needs a destination" pin on the right, with the carrier result-local
collapsed into the gate). Before that: held-input debug runs; the SCADE build
pipeline (Build → Run Simulation → Generate C-Lite → Compile & Run) with
`<operator>.lus` written on a clean build, the Lustre pane gated on build, and
**log messages** (debug probes printed every 50 cycles); canvas select /
multi-select / right-click-menu / Delete-key; the two-column simulation
watch/set table with per-type validation; SCADE gates (input pins left, output
right) + pin-to-pin wiring; MC/DC coverage; array iterators (`map`/`fold`);
boolean clocks (`when`/`merge`); undo/redo; properties dock; constants; block
symbols; typed wire labels.

**Best next gaps (pick up here):**
1. **Canvas item ergonomics** (P1/P2) — resize inputs/outputs/locals/operations
   on the canvas, and a right-click "wrap text / don't wrap" per box.
   *Also requested:* drag a composite **type onto the canvas to MAKE / FLATTEN**
   it (construct an array/struct from element inputs, or destructure one) — a
   Structures/Arrays authoring feature tied to the Types node.
   *Also:* **composite constant values** — array/struct/string (`char[]`)
   constants need array-literal syntax in `ol_stdlib::parse_expr` (today only
   scalar constants parse) plus a `char` type; scalar constants already work.
2. ~~Float intrinsics~~ — **landed 2026-07-03** (see §6); float32-native
   variants (`sqrtf` & friends) remain conscious roadmap — today float32
   casts through float64 explicitly.
3. ~~Tool Operational Requirements document~~ — **landed 2026-07-03**:
   `docs/TOOL_OPERATIONAL_REQUIREMENTS.md` (TOR-1…12, each claim mapped to
   its verification tests; usage constraints; documented exclusions). The
   verification-by-equivalence story (§4) is complete.
4. ~~Editor polish~~ — **all landed 2026-07-03**: orthogonal wire routing,
   zoom/pan, marquee select, copy/paste, and per-family gate silhouettes
   (§2). Remaining §2 items are P2 multi-sheet diagrams and cosmetic
   junction dots.
5. **Deployment** (§5) — `.lus`/`.ols` file association + app icon (P1,
   cosmetic), then code signing (P2, cost not code).
6. **Automata depth** (P2) — history / signals UI, and richer parallel
   composition beyond instantiating several machines in one operator.

Everything ships across all stages — IR → typecheck → sim → generated C →
dual-backend equivalence test — or it isn't done. The §6 log records each slice.

## 1. Where the products stand today

| Workflow step | SCADE Suite | OpenLustre Studio today |
|---|---|---|
| Graphical authoring | Full diagram editor: palette drag-drop, pin-to-pin wire drawing, hierarchical sheets | Drag-drop palette, **SCADE gates with red "needs a source" input pins / red "needs a destination" output pin, pin-to-pin wiring** (result-local collapsed into the gate), draggable grid-snapped canvas with persisted layout that doesn't auto-collapse, multi-select + right-click menu + Delete, red invalid-link coding |
| Language | Scade 6 (Lustre core + clocks, automata, iterators, packages) | Lustre subset + **boolean clocks (`when`/`merge`)** + **array iterators (`map`/`fold`)**: dataflow, `pre`/`->`, records/enums/arrays, constants, flat FSMs (lowered), imported C operators |
| Static checks | Type/clock checker | Type checker + **clock calculus** + contract checker (vacuity, unreachability, overlap), live in the GUI |
| Simulation | Cycle stepping, watch, plots, co-simulation | **Two-column watch/set table** (sticky typed inputs, computed locals/outputs), full per-item trace, CSV batch simulation, golden-trace scenarios |
| Formal verification | Design Verifier (Prover plug-in) | Kind 2 adapter (BMC/induction, realizability, mode coverage) + CoCoSpec contract emission, in-GUI Verify tab |
| Build & codegen | KCG qualified C/Ada (TQL-1) | **Build pipeline** with a build-any-operator selector (the chosen operator becomes the root): per-operator validity check on its slice → its own `<operator>.lus` (blank stub on create, filled on build) → C-Lite → debug run in a terminal, C-Lite emitter + contract monitors + CSV driver + Makefile + **log-message probes**, selected-root slicing |
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
| ~~P1 — Orthogonal wire routing~~ | Manhattan-routed wires with junctions | **Landed 2026-07-03**: wires route orthogonally by default (horizontal out, one mid-channel vertical, horizontal in; feedback wires hook around below; parallel wires stagger a few px so channels don't collapse); View ▸ "orthogonal wires" toggles back to Béziers. Junction dots at fan-outs remain cosmetic roadmap | done |
| ~~P1 — Zoom/pan, copy/paste~~ | Standard | **Landed 2026-07-03**: viewBox zoom (Ctrl+wheel around the cursor, View-menu items, Ctrl+= / − / 0, status-bar %), middle-button pan, marquee multi-select on empty canvas, and Ctrl+C/Ctrl+V duplicating the selected blocks as one journaled edit (`/api/edit/duplicate_equations` — fresh `_copy` locals, internal wiring rewritten, external reads kept) | done |
| **P2 — Multi-sheet diagrams** | One operator can span sheets | Page list per node in `DiagramLayout` | Medium |
| ~~P2 — Per-family gate silhouettes~~ | Distinct shapes per operator family (gates, delays, switches) | **Landed 2026-07-03**: AND (flat back, round nose), OR (shield), XOR (second back arc), NOT (triangle + bubble) draw as true IEC/SCADE silhouettes fitted to the same bounding box (pins/labels/resize unchanged); everything else keeps its compact symbol box | done |

## 3. Language and toolchain gaps

| Gap | Notes | Priority |
|---|---|---|
| Source spans in diagnostics | `Diagnostic.span` exists but is never populated. Honest re-scope: models are GUI-authored JSON, so the `node X · equation N` context (landed) already pins every diagnostic to its diagram box — file:line:col only becomes meaningful with a textual `.lus` frontend, which is itself roadmap | P2 (was P0) |
| ~~Clocks (`when` / `merge`)~~ | **Landed 2026-06-12**: boolean clocks end to end — `e when c` / `e when not c` / `merge(c, a, b)` in IR, parser, formatter, clock calculus (E0130–E0135), simulator, generated C, Kind 2 view (V6 merge-case syntax), and the Time/Statefuls toolbox. See §6 | done |
| Hierarchical/parallel automata | **Landed**: state machines are **operator-owned** (a machine is an operator's body, nested under it in the tree, created within it); a state can `refine` another machine or hold nested `Region`s, lowered recursively with restart-on-entry / freeze / history (§6). Remaining: signals, and richer parallel/history UI (a state's inline nested-region authoring beyond `refine`) | P2 |
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
4. **Tool Operational Requirements document** — **done 2026-07-03**:
   `docs/TOOL_OPERATIONAL_REQUIREMENTS.md` enumerates the tool's claims
   (TOR-1…12) with each mapped to its verification evidence in the test
   suite, plus usage constraints and documented exclusions. The
   verification-by-equivalence story is complete: dual-backend execution,
   formal contract proofs, coverage evidence, and now the TOR document.

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

### 2026-07-03 (gates) — per-family silhouettes

The logic family draws its classic IEC/SCADE shapes instead of labeled
boxes: **AND** flat back + semicircular nose, **OR** a shield (curved back,
pointed nose), **XOR** the shield plus a detached second back arc, **NOT** a
triangle with the inversion bubble. Each silhouette is fitted to the same
bounding box the rect used, so input pins, the output pin, wires, marquee
hit-testing, and the resize grip all keep working; the symbol text drops
(the shape *is* the operator) while the full equation stays on hover.
Arithmetic/comparison/temporal blocks keep their compact symbol boxes —
SCADE's own convention. Verified in Chromium (silhouette paths + decorations
present, `+` still a labeled rect; screenshot-checked).

### 2026-07-03 (wires) — orthogonal (Manhattan) routing

Wires draw SCADE-style right angles instead of cubic Béziers: horizontal out
of the source pin, one vertical mid-channel (snapped between the boxes, with
a per-wire stagger so parallel wires don't collapse onto one line),
horizontal into the target pin — which for gates is still the exact operand
pin. A backward (feedback) wire hooks around: a stub out, down to a channel
below both boxes, back left, and in. Wire labels sit on the channel segment.
View ▸ "orthogonal wires" (default on) switches back to the old Béziers.
Verified in Chromium: every rendered wire is M/L-only, multi-segment routes
present, and the toggle restores curves.

### 2026-07-03 (editor) — zoom/pan, marquee select, copy/paste

The three "standard editor" gaps from §2, verified end to end in a real
browser (Playwright/Chromium driving the served Studio):

* **Zoom** is a viewBox transform: the svg element scales, model coordinates
  stay 1:1 (`svgPoint` divides by the zoom, so dragging/wiring/dropping all
  keep working at any zoom). Ctrl+wheel zooms around the cursor, the View
  menu has Zoom In / Out / 100% (Ctrl+= / Ctrl+- / Ctrl+0), the status bar
  shows the current percentage, clamped 25–300 %.
* **Pan** is middle-button drag (the canvas is the host's scroll area, so
  panning is scrolling). **Marquee**: left-drag on empty canvas rubber-bands
  a rectangle; boxes it touches become the selection (additive with
  ctrl/shift); a click that never grows into a drag still clears. Both
  gestures are tracked at the *document* level — the svg is rebuilt by every
  render, so element-level listeners would drop the pointerup the moment the
  cursor left the canvas (found live, fixed, retested).
* **Copy/paste**: Ctrl+C copies the selected equation blocks (Ctrl+C with
  text selected keeps native copying — the handler only acts on a collapsed
  selection); Ctrl+V posts `/api/edit/duplicate_equations`, which clones the
  set server-side as **one journaled edit**: every result gets a fresh
  `_copy`-suffixed local typed like its source (an output lhs pastes as a
  local — two blocks can't drive one output), references *within* the copied
  set are rewritten onto the fresh names so the pasted sub-diagram stays
  internally wired, reads of anything outside the set keep pointing at the
  originals, and each pasted box lands offset from its source. One Ctrl+Z
  removes the whole paste. Edit menu carries Copy/Paste; a cross-operator
  paste is refused with a message.
* Tests: `duplicate_equations_pastes_a_rewired_sub_diagram`
  (tests/studio_editing.rs) covers the rewiring, typing, offsets, `_copy2`
  suffixing, single-undo, and the loud 400s; the browser smoke drive
  verified zoom/wheel/marquee/paste/pan against the live page.

### 2026-07-03 — float intrinsics: the `<math.h>` double family, end to end

`square_root` stops being the greyed-out chip. `Expr::FloatIntrinsic { op, args }`
(`ol_ir::FloatOp`: sqrt, sin, cos, tan, asin, acos, atan, atan2, exp, log,
log10, pow, floor, ceil, round, abs, min, max) flows through every stage:

* **Semantics: float64 only.** Operands and result are `float64`; a float32
  or integer operand is **E0161** with the explicit-cast hint
  (`sqrt(float64(x))`), wrong arity is **E0160** (also caught at parse
  time). This keeps the profile's no-implicit-conversion rule *and* makes
  the backends agree exactly: the simulator computes in Rust `f64` (the
  platform libm — the same functions the C calls), the generated C calls
  the `<math.h>` double family (`abs/min/max` → `fabs/fmin/fmax`), and
  `round` is half-away-from-zero on both. The generated header includes
  `<math.h>`; the emitted Makefile, the scenario harness, and the in-GUI
  compile all link `-lm` on POSIX.
* **Surface syntax** is function-style and round-trips
  (`sqrt(x)`, `atan2(y, x)`); the names are reserved in call position only.
  The Kind 2 view prints the same call text — the `bit_and` convention: the
  user supplies matching Lustre `real` functions when proving.
* **Authoring**: `square_root` enabled in Mathematics; a new **Float Math**
  toolbox family drops the rest with typed `float64` pins (two pins for
  `atan2/pow/min/max`); the diagram symbol is the function name; contract
  monitors emit the same C calls.
* **Tests** (`tests/float_intrinsics.rs`): parse/format round-trip, the
  E0160/E0161 rules, exact-value simulation for the exactly-rounded subset
  (sqrt/abs/min/max/floor/ceil/round/pow on representable decimals),
  tolerance-checked transcendentals, generated-C content, and the
  dual-backend IR-vs-compiled-C equivalence run — the byte-exact scenario
  uses only the exactly-rounded subset, since libm transcendentals are not
  correctly-rounded and could differ across platforms by an ulp.
* Conscious limits, all loud: float32-native variants (`sqrtf`) are
  roadmap — float32 casts through float64 explicitly today.

### 2026-06-16 (owned) — state machines are operator-owned

Per the SCADE shape the user asked for: a state machine belongs to exactly one
operator and *is* (part of) its body, shown nested under it in the tree and
nowhere else.

* **IR/lowering.** `StateMachineDef` gained `owner: Option<String>`.
  `lower_state_machines` now branches: an owned machine (`owner = Some(op)`) has
  its lowered state/next/output equations + locals + state-enum merged into
  operator `op`'s node (driving `op`'s outputs); an owner-less machine (stdlib
  library blocks such as `libraries/state_machines/srff.yaml`) still lowers to a
  standalone node. Refine resolution and the nested-region engine are unchanged.
  An `UnknownOwner` error guards a machine whose operator is missing.
* **Server.** `add_state_machine` takes `operator`, requires it to exist, and
  allows one machine per operator; inputs/outputs come from that operator.
  `inspect` lists each machine's `owner` + state names; `fsm_get` returns the
  owner.
* **Tree.** Operators are now expandable: **Inputs / Locals / StateMachine: Name
  → states / Outputs** (the generated `__sm_*` locals and `*_StateEnum` type are
  hidden). The separate "State Machines" group is gone. Create from an operator
  (right-click ▸ Add State Machine, or the editor's operator selector — I/O
  auto-filled and read-only).
* **Verified** end to end: `MyTrafficLight(tick, emergency) → (go, warn)` with an
  owned `Lights` machine (Red/Green/Yellow) renders as the exact requested tree,
  merges, and builds. Tests:
  `operator_owned_machine_merges_into_the_operator_and_simulates`
  (tests/state_machines.rs) and `state_machine_owned_by_operator_*`
  (tests/studio_workspace_types.rs); the critical-items FSM test was updated to
  the owned model.

### 2026-06-16 (refine) — authoring hierarchy by refinement

The editor half of nested automata, in the spirit the user asked for —
text-based, composed by reference. A state can `refine` another machine: in the
state-machine editor's states box, write `Active: refine Spin` (alongside any of
the state's own equations). At lowering time `resolve_refines` looks `Spin` up
among the project's machines and inlines its (recursively resolved) states as a
nested region of that state — **qualifying the inlined state names per site**
(`Active_Lo`, …) so they collide with neither the standalone `Spin` nor a second
refinement, and resolving **live** so edits to `Spin` propagate. Unknown and
cyclic refinements are reported; the machine validates on save (resolve + lower)
and round-trips back into the form (`fsm_get` returns each state's `refines`).
The keyword preview colours `refine`. Verified end to end in the Studio: author
flat `Spin`, refine it from `RefMode`'s `Active`, build → nested lowering
(`RefMode_r1_StateEnum`, `Active_Lo`); tests in `tests/state_machines.rs`
(`refine_resolves_a_sub_machine_and_simulates`, `refine_to_unknown_machine_is_rejected`).
Top-level parallel composition is already available by instantiating several
machines in one operator; history/signals UI remains (§ best-next #6).

### 2026-06-16 (hierarchy) — hierarchical (nested) automata: the engine

The big structural step for automata. A `StateDef` can now hold nested
`Region`s — sub-automata that run while the containing state is active — and
`crates/ol_ir/src/state_machine.rs::lower` walks the tree recursively:

* **Per-region state machinery.** Each region (the top automaton and every
  nested one) gets its own `<machine>[_rN]_StateEnum` and a state variable. A
  nested region also gets a boolean activation local (`parent active and
  parent_state = S`) so its activation can be `pre`'d within the profile's
  `pre <var>` rule. The region's next-state advances only while active and
  otherwise **freezes**; on (re-)entry it **restarts** at its initial state
  (`active and not pre active`) unless `history` is set, in which case it
  resumes. Outputs are a single selection chain over the whole state tree — a
  state's value for an output is its nested region's value when one drives it,
  else the state's own equation.
* **Lowers to the same flat dataflow**, so typecheck, the simulator, and the
  C-Lite emitter need *no* changes. A hierarchical `Mode(go,stop,tick)` machine
  (top Idle/Active; Active nests Lo↔Hi driving `level`) simulates correctly —
  including restart-on-entry and freeze-while-inactive — and its generated C
  carries both region enums (`Mode_StateEnum`, `Mode_r1_StateEnum`); a cc-gated
  byte-for-byte IR↔C test guards equivalence.
* **Validation is recursive**: unique state names across all regions, per-region
  initial/target checks, and SCADE-strict "every output assigned along every
  path" with a precise offending-state error.
* The Studio server already accepts nested `regions` in the create/update
  state-machine payload (and `fsm_get` returns them); **authoring the nesting in
  the editor is the next step** (today the text form covers flat machines).

### 2026-06-16 (automata) — text-based state-machine authoring, used as a block

State machines already had a solid IR (`crates/ol_ir/src/state_machine.rs`):
they lower to a dataflow node + a `<name>_StateEnum` enum, and lowering already
enforces SCADE strictness (unknown initial/target states, and **every output
assigned in every state**). What was missing was authoring and presence. Added:

* **Edit, not just create.** `/api/edit/update_state_machine` (replace in place)
  and `/api/edit/remove_state_machine`, sharing one validated parser with the
  create path. The editor (`dlg-fsm`) now loads an existing machine back into
  its text form (name fixed, states/transitions/IO re-filled), so you can change
  it and **Save changes**; "Load template" drops a worked Initial→…→ example.
* **A keyword-coloured live preview** under the textareas — state names, `->`,
  `when`, and the initial-state tag highlight as you type, so the textual syntax
  is easy to follow without a rich-text editor.
* **First-class in the tree.** `build_inspect` lists the raw machines, so a
  **State Machines** group (parallel to Types / Constants) shows each one
  (`name · N states`, click to edit, right-click to delete). The lowered node
  and its `_StateEnum` type are filtered out of the Operators / Types views so a
  machine reads as a single thing — it still appears in the Operations toolbox.
* **Used as a block inside an operator** — the model the user asked for: drop the
  machine from the toolbox onto an operator's canvas, the operator's inputs wire
  into the block on the left, its outputs come out on the right
  (`go, warn = TrafficLight(tick, emergency)`), and the operator builds. Verified
  end to end (create → edit → drop-as-block → wire → build); workspace test
  `state_machine_create_edit_use_as_block_and_remove`.

This is text-based authoring on top of the existing flat lowering; nested /
parallel automata (history, signals) are the remaining structural step (§1).

### 2026-06-16 (import) — import existing Lustre

`File ▸ Import Lustre…` reuses models authored elsewhere. There was no
node-level Lustre parser (only `ol_stdlib::parse_expr` / `parse_type`), so this
adds a small frontend, `crates/ol_cli/src/lustre_import.rs`:

* **What it parses** — `type` (enum / struct / alias), `const`, `node`, and
  `function` declarations, including the `var` section, multi-output tuple
  left-hand sides `(a, b) = …`, `--` and `(* *)` comments, and the `elem^len`
  array form our own emitter produces. Equation bodies and constant values go to
  `parse_expr`; types to `parse_type`. So the dataflow subset the tool emits
  round-trips — an operator's own `<op>.lus` re-imports and rebuilds — and the
  `function Inc … node Pipeline … t = Inc(a); out = Inc(t)` example imports,
  appears in the tree and toolbox, and builds.
* **How it lands** — `/api/edit/import_lustre` parses, then checks every
  imported name against the whole loaded project (operators *and* the stdlib,
  types, constants): any clash rejects the import all-or-nothing before a byte
  is written. Nodes go to the model file, types and constants to the
  project-global types file (matching the dialogs), each imported operator gets
  its blank `.lus` stub, and the whole thing is one journaled (undoable) edit.
  Unsupported surface (assertions, inline contracts, malformed declarations)
  produces a located error rather than a silent or wrong import.
* **Tested** — parser unit tests (node/function, Lustre `real`/`int`, `^`
  arrays, types+consts, `assert` rejection, empty input) plus a workspace
  integration test (import a type+const+node, build it, reject the re-import);
  verified live.

### 2026-06-16 (later still) — Types & Constants tree nodes

SCADE puts **Types** and **Constants** in the model browser between the
operators and the libraries; now so do we.

* **Types node** — lists the project's named types (enum / record / alias)
  with a kind hint; clicking a type or "New type…" opens the existing Types
  editor, right-click deletes. (The data was already in `/api/inspect`; this
  surfaces it in the tree.)
* **Constants node** — project-wide constants, all-caps by convention
  (`NAME : type = value`). The whole IR pipeline already supported `ConstDef`
  (typecheck registers them, the simulator evaluates them, both emitters emit
  `const` declarations, slicing keeps the used ones); what was missing was the
  *editing* surface. New `/api/edit/add_constant` (upper-cases the name, parses
  the type and value, saves into the project-global types file) and
  `/api/edit/remove_constant`; the inspect now carries each constant's formatted
  value. A dialog adds them; the tree lists them; an operator uses one like any
  global (`out = MAX_SPEED` → `const MAX_SPEED : int = 32;` in the emitted
  Lustre). Verified end to end + a workspace test.
* **Scope:** scalar constants (int/uint/float/bool) work fully. Array / struct /
  string (`char[]`) constant *values* need array-literal syntax in
  `ol_stdlib::parse_expr` (it parses scalars, not `[1;2;3]`) and a `char` type —
  logged in §0 alongside the requested drag-a-type-to-MAKE/FLATTEN authoring.

### 2026-06-16 (later) — empty projects, operator right-click menu, gated/copyable code panes

A second demo-feedback batch:

* **Empty new projects.** `openlustre new --empty` seeds a project with no
  operators (one `user` package, no `main`); the Studio serves a blank project
  and stays fully editable. The default `new` and a served not-yet-created
  workspace still seed the starter operator (a double-clicked shortcut opens
  something runnable). Covered by a workspace test.
* **Right-click an operator in the workspace tree.** Each operator row now has a
  context menu: **Build** (makes it the build target, opens the Build dock, and
  builds it — the discoverable answer to "let me pick which operator to
  build"), **Add Input / Add Output / Add Local** (prefilled for that operator),
  and **Set as Main**.
* **Both code panes gated and copyable.** The Generated-C side pane used to
  always show C; now it is empty until **Generate C-Lite** is pressed, exactly
  as the Lustre pane is empty until a clean **Build**. (Both re-lock on an
  edit.) Each pane has a **Copy** button and is text-selectable, so the engineer
  can lift the Lustre or C straight out. The earlier `pipeGenerate` ordering bug
  (switching to the C pane *before* marking it generated) is fixed.

Still open from the same demo (logged in §0): import existing Lustre (needs a
node-level parser), SCADE-shaped state-machine authoring nested in an operator,
and canvas item resize / text-wrap.

### 2026-06-16 — live-demo authoring fixes (tree, build target, per-operator files, red I/O pins)

Four round-three GUI gaps from a live demo:

* **The workspace tree stops auto-collapsing.** The 5 s `/api/inspect` poll
  rebuilds the tree, and every rebuild reset the `<details>` folders to their
  default state — so a folder you expanded (the Libraries / stdlib root
  especially) snapped shut a couple of seconds later. Disclosure state is now
  remembered per folder (`state.treeOpen`, keyed `pkg:<name>` / `__libraries__`,
  written from a `toggle` listener), so folders only close when *you* close
  them.
* **Choose which operator to build.** The Build dock has an *operator-to-build*
  selector; `/api/build` takes an optional `{node}`, makes it the root (SCADE
  "set as root", persisted + journaled), and scopes the validity check to that
  operator's **slice** — so you can build a clean operator even while an
  unrelated one is mid-edit. Simulate / Generate / Run all follow the chosen
  operator; changing the selector re-locks the pipeline until it is built.
* **Every operator has its own Lustre file.** Creating an operator writes a
  blank `<Name>.lus` stub next to the model immediately; building that operator
  fills it with its emitted Lustre (root + dependencies). The model JSON stays
  the single source of truth — the `.lus` files are per-operator projections,
  blank until built (a failed build never fills the stub).
* **Red I/O pins instead of ghost boxes.** A dropped operation used to spawn a
  separate result-local box on the right. Now the gate's own right pin *is* the
  result: red ("output — needs a destination") until something consumes it, the
  way unbound operands are already red pins on the left ("input — needs a
  source"). The carrier local is preserved in the model (it still wires gates
  together) but its box is collapsed into the gate, and wires leaving it are
  drawn from the gate's pin. So a freshly dropped gate reads as red-in / red-out
  and you wire your existing inputs straight onto it. Server marks each
  single-output gate's result `collapsible`; the canvas does the hiding,
  rerouting, and pin colouring. Covered by two new studio-API tests plus live
  verification.

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
