# OpenLustre Studio vs. Ansys SCADE Suite — gap analysis

*Updated 2026-06-18.*

OpenLustre Studio aims to be the open, SCADE-shaped workbench: author synchronous
dataflow models graphically, check them, simulate them deterministically, prove
contracts, generate C, and test the generated code against the model. SCADE Suite
is the industry-accepted, DO-178C-qualified original. This document is honest about
which gaps are **bridgeable engineering work** and which are **structural** (you
cannot code your way to a qualification certificate), and prioritizes the former.

**Deliberate non-goals (not gaps).** OpenLustre Studio generates **C-Lite only** —
**Ada generation and MISRA-C-styled output are explicit non-goals**, the
product intentionally diverging from KCG there: the scope is graphical Lustre →
*directional* C-Lite to be built with a real-time/embedded OS and target
hardware. Items omitted from this document's gap tables on purpose: Ada/MISRA
codegen, and the SCADE Architect/Display companion products.

## 0. Status snapshot — resume here

**Repo**: `C:\Users\Jonathan\Projects\OpenLustreStudio` (Rust workspace, branch
`main`). **Full check**: `cargo test --workspace --no-fail-fast` (56 result
groups green as of 2026-06-16 on Windows/MSVC). The Studio GUI is one embedded
HTML page, `crates/ol_cli/src/studio_ui.html`, served by
`crates/ol_cli/src/studio_server.rs` (`openlustre studio serve <dir>`); the IR
is `crates/ol_ir`, sim `crates/ol_sim`, C emitter `crates/ol_clite_emit`,
typecheck `crates/ol_typecheck`.

**Landed recently (newest first):** **Tool Operational Requirements document.**
`docs/TOOL_OPERATIONAL_REQUIREMENTS.md` states every operation the tool claims as
a numbered requirement (TOR-001 … TOR-702) bound to its test evidence — the fourth
and last pillar of verification-by-equivalence (§4). Before that: **Float
intrinsics.** `square_root` is
un-greyed and a full math family — `sin`, `cos`, `tan`, `exp`, `log`, `abs`,
`min`, `max`, `pow` — is first-class across every stage, mirroring
`numeric_cast`: a new `Expr::Intrinsic { func, args }` IR node, surface syntax
`sqrt(x)` / `min(a, b)` (the parser maps the reserved names; arity checked),
typechecked (`float64`/`real` operands only, result `float64` — E0160/E0161),
simulated in `f64`, lowered to `<math.h>` in generated C (the double variants;
`-lm` linked in the Makefile and both compile paths), and emitted to the Kind 2
view as same-named user-suppliable functions (the cast/bit-op convention). A
dual-backend equivalence test (IR vs compiled C, integer-valued results so the
formatting matches cell-for-cell) and parser/typecheck/sim/codegen tests pin it.
`float32` intrinsics are a conscious v1 limit (they need the `f`-suffixed math
variants with matching f32 rounding in the sim). Before that: **State machines
are operator-owned.** A
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
1. *(Landed)* ~~**Canvas item ergonomics**~~ — resize boxes + per-box text
   wrap, drag a composite **type to MAKE / FLATTEN / SLICE**, and **composite
   constant values** (array/struct/`char[]` literals) all shipped (see git log).
2. *(Landed 2026-06-18)* ~~**Float intrinsics**~~ — `square_root` plus
   `sin/cos/tan/exp/log/abs/min/max/pow` as a `float64` family across IR, parse,
   typecheck, sim, generated C (`<math.h>`), and the Kind 2 view (§6). `float32`
   intrinsics remain roadmap.
3. *(Landed 2026-06-18)* ~~**Tool Operational Requirements document**~~ —
   `docs/TOOL_OPERATIONAL_REQUIREMENTS.md` enumerates every operation the tool
   claims (TOR-001…TOR-702) and binds each to its test evidence, completing the
   verification-by-equivalence story (§4). 214 passing tests across 57 groups are
   the verification cases.
4. *(Landed 2026-06-18)* ~~**Editor polish**~~ — zoom/pan, copy/paste + marquee
   select, **orthogonal (Manhattan) wire routing**, and **distinct per-family gate
   silhouettes** (curved-AND, pointed-OR, mux trapezoid, delay register) all
   shipped (§2, §6). Remaining editor items are now P2: multi-sheet diagrams and
   MDI document tabs.
5. *(Landed 2026-06-18)* ~~**Deployment & target codegen**~~ — `.wksc`/`.ols`
   file association, a generated multi-resolution app icon, and **target/OS build
   profiles** (host / embedded Linux-ARM / VxWorks / bare-metal — directional
   cross-build Makefile + `INTEGRATION.md` per target), a compilable
   `integration.c` entry skeleton, and a **Docker + QEMU emulated-target backend**
   (`clite-emulate` — the third equivalence backend, §6) shipped (§5, §6).
   Remaining: full-system board/RTOS emulation (under `qemu-system-*`), code
   signing (P2), winget/MSIX.
6. **Automata depth** (P2) — **history + inline nested-region authoring landed
   2026-06-18** (§6); remaining: **signals** (a new cross-stack IR construct —
   the next real automata feature) and richer parallel composition beyond
   instantiating several machines in one operator.

Everything ships across all stages — IR → typecheck → sim → generated C →
dual-backend equivalence test — or it isn't done. The §6 log records each slice.

## 1. Where the products stand today

| Workflow step | SCADE Suite | OpenLustre Studio today |
|---|---|---|
| Graphical authoring | Full diagram editor: palette drag-drop, pin-to-pin wire drawing, hierarchical sheets | Drag-drop palette, **SCADE gates with red "needs a source" input pins / red "needs a destination" output pin, pin-to-pin wiring** (result-local collapsed into the gate), **per-family gate silhouettes** (D-shape AND, pointed-OR, mux trapezoid, delay register), **orthogonal (Manhattan) wire routing**, draggable grid-snapped canvas with persisted layout that doesn't auto-collapse, **zoom/pan (Ctrl+wheel, middle/Space-drag, fit-to-window)**, **marquee select + copy/paste of blocks**, multi-select + right-click menu + Delete, red invalid-link coding |
| Language | Scade 6 (Lustre core + clocks, automata, iterators, packages) | Lustre subset + **boolean clocks (`when`/`merge`)** + **array iterators (`map`/`fold`)** + **float intrinsics (`sqrt`/`sin`/`cos`/`abs`/`min`/`max`/…)**: dataflow, `pre`/`->`, records/enums/arrays, constants, flat FSMs (lowered), imported C operators |
| Static checks | Type/clock checker | Type checker + **clock calculus** + contract checker (vacuity, unreachability, overlap), live in the GUI |
| Simulation | Cycle stepping, watch, plots, co-simulation | **Two-column watch/set table** (sticky typed inputs, computed locals/outputs), full per-item trace, CSV batch simulation, golden-trace scenarios |
| Formal verification | Design Verifier (Prover plug-in) | Kind 2 adapter (BMC/induction, realizability, mode coverage) + CoCoSpec contract emission, in-GUI Verify tab |
| Build & codegen | KCG qualified C/Ada (TQL-1), multiple target integrations | **Build pipeline** with a build-any-operator selector (the chosen operator becomes the root): per-operator validity check on its slice → its own `<operator>.lus` (blank stub on create, filled on build) → C-Lite → debug run in a terminal, C-Lite emitter + contract monitors + CSV driver + Makefile + **log-message probes**, selected-root slicing, **target/OS build profiles** (host / embedded Linux-ARM / VxWorks / bare-metal — directional cross-build Makefile + `INTEGRATION.md` per target). **By design the only codegen target is C-Lite** — Ada and MISRA-C-styled output are a deliberate non-goal, *not* a gap (the product is graphical Lustre → directional C-Lite for an RTOS/embedded target). |
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
| ~~P1 — Orthogonal wire routing~~ | Manhattan-routed wires with junctions | **Landed 2026-06-18**: cubic Béziers replaced with H/V routing — forward wires turn through a mid-x channel, backward (feedback) wires detour around boxes, near-aligned wires draw straight, all with rounded elbows (`orthoPoints`/`roundedPath` in studio_ui.html). See §6 | done |
| ~~P1 — Zoom/pan~~ | Standard | **Landed 2026-06-18**: a fixed `0 0 W H` viewBox painted at `W·zoom × H·zoom`; Ctrl/⌘+wheel (and trackpad pinch) zooms toward the cursor, middle-/Space-drag and scrollbars pan, View ▸ Zoom + Ctrl +/−/0, a click-to-reset % badge; `getScreenCTM().inverse()` makes drag/drop/wire math exact at any zoom. See §6 | done |
| ~~P1 — Copy/paste, marquee select~~ | Standard | **Landed 2026-06-18**: drag on empty canvas for a rubber-band marquee (overlap-selects boxes, Shift/Ctrl adds); Ctrl+C/X/V (and a context menu) copy/cut/paste operation blocks via `/api/edit/paste`, which clones equations with fresh result-local names, rewires references *within* the copied set, looks types up by name (clipboard survives index shifts), and cascades repeated pastes. See §6 | done |
| **P2 — Multi-sheet diagrams** | One operator can span sheets | Page list per node in `DiagramLayout` | Medium |
| ~~P2 — Per-family gate silhouettes~~ | Distinct shapes per operator family (gates, delays, switches) | **Landed 2026-06-18**: `gateFamily`/`gateBody` key off the symbol glyph — AND is a D-shape, OR/XOR/⇒ a pointed shield, ITE a multiplexer trapezoid, pre/FBY/-> a register block; the straight left edge keeps input pins seated and the right tip keeps the output pin seated. See §6 | done |

## 3. Language and toolchain gaps

| Gap | Notes | Priority |
|---|---|---|
| Source spans in diagnostics | `Diagnostic.span` exists but is never populated. Honest re-scope: models are GUI-authored JSON, so the `node X · equation N` context (landed) already pins every diagnostic to its diagram box — file:line:col only becomes meaningful with a textual `.lus` frontend, which is itself roadmap | P2 (was P0) |
| ~~Clocks (`when` / `merge`)~~ | **Landed 2026-06-12**: boolean clocks end to end — `e when c` / `e when not c` / `merge(c, a, b)` in IR, parser, formatter, clock calculus (E0130–E0135), simulator, generated C, Kind 2 view (V6 merge-case syntax), and the Time/Statefuls toolbox. See §6 | done |
| Hierarchical/parallel automata | **Landed**: state machines are **operator-owned**; a state can `refine` another machine or hold nested `Region`s, lowered recursively with restart-on-entry / freeze / history (§6); the editor now authors a **nested region inline** with a `{ … }` block including the **`history`** flag (§6). Remaining: signals, and richer parallel composition beyond instantiating several machines in one operator | P2 |
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
   with the test suite as verification cases (**done 2026-06-18**:
   `docs/TOOL_OPERATIONAL_REQUIREMENTS.md`, TOR-001…TOR-702, each requirement bound
   to its test evidence; 214 tests / 57 groups are the verification cases). With
   MC/DC and now the TOR document landed, the four pillars of
   verification-by-equivalence are all in place.

That story positions the tool as: *generated code you independently verify*, which
is a legitimate (if more laborious) DO-178C path where the applicant carries the
verification burden the qualified tool would otherwise discharge.

## 5. Windows deployment gaps

| Gap | Notes | Priority |
|---|---|---|
| ~~File association~~ | `.ols`/`.json` model double-click should open the Studio (registry entries in the installer) | **Done 2026-06-18**: the installer registers `.wksc` and `.ols` → `OpenLustreStudio.Model` → `openlustre.exe studio launch "%1"` (HKA, so per-user or per-machine). `.lus` is left unclaimed (import-only, shared with other Lustre tools) and `.json` is too generic to hijack. |
| ~~App icon~~ | The shortcut currently uses the default exe icon | **Done 2026-06-18**: `packaging/windows/make-icon.ps1` generates a multi-resolution `openlustre.ico` (white AND-gate on the app blue); used for the Setup.exe, Start-Menu/Desktop shortcuts, the file-type DefaultIcon, and the uninstall entry. |
| Code signing | Unsigned installers trip SmartScreen; needs a cert (cost, not code) | P2 |
| winget/MSIX distribution | `winget install OpenLustreStudio` once the repo publishes releases | P2 |
| Auto-update check | Studio could poll GitHub releases and show a banner | P3 |
| ~~Target/OS build profiles~~ | Pick the OS/board for codegen; generate a toolchain-tuned Makefile + integration note | **Done 2026-06-18**: host (compiles locally) + embedded Linux-ARM / VxWorks / bare-metal-ARM (directional — emit build files for the target toolchain). `crates/ol_cli/src/target.rs`, `/api/targets`, Compile dialog selector |
| ~~Emulated target testing (QEMU + Docker)~~ | Build → run the generated C on an emulated board in a container, against the same scenario suite as the IR/host backends — a *third* equivalence backend | **Landed 2026-06-18**: `openlustre clite-emulate` (`crates/ol_cli/src/emulate.rs`) emits a self-contained Docker context (cross-toolchain + `qemu-user-static`, static armhf link, `qemu-arm-static` entrypoint) and, where Docker is present, builds + runs it and checks the trace against the IR sim cell-for-cell. Live run is Docker-host-gated (like the cc-gated dual-backend tests). `--system` adds **full-system arm64** (`qemu-system-aarch64 -M virt` + a busybox initramfs, kernel supplied as input) — generation tested; first boot needs a Docker host. RTOS full-system (VxWorks under `qemu-system-*`) remains (no freely-distributable image). |

## 6. What closed recently

### 2026-06-18 — Open/Save/New workspace browse dialogs

File ▸ Open / Save As / New now **browse the filesystem** instead of typing a
path. The dialog is an **in-app navigator** (server `/api/fs/list` lists
subfolders + workspace files with a parent for *Up* and a *This PC* drives view;
the client renders breadcrumb + clickable entries) — works everywhere and is
fully testable. **Open** picks a `.wksc` to open; **Save As** (`/api/workspace/save_as`)
writes the current workspace to a chosen folder/name and switches to it
(carrying `types.json`); **New** creates an empty workspace in the chosen
folder. A **"Browse… (native)"** button additionally pops the native OS file
dialog on Windows (`/api/dialog/pick`, server-side PowerShell), falling back to
the in-app navigator when unavailable. Verified live (navigation incl. drives,
Open list, Save-As writes + switches, New is empty) + a server test
(`fs_list_navigates_and_save_as_switches`).

### 2026-06-18 — cruise-control example + blank-by-default workspaces

* **Empty by default.** The default/welcome workspace and *opening a fresh
  folder* now open **blank** — no `Heartbeat` starter operator (the user adds
  their own or opens an example). `New Workspace` was already empty; `studio
  serve <dir>` keeps a starter only for the test fixtures. (welcome / open
  seeding flipped to `empty_project`; the welcome test now asserts a blank
  project with the stdlib palette still merged.)
* **Worked example: cruise control** (`examples/cruise_control/`). A
  `CruiseControl(speed, set_cruise_on, brake, turn_cruise_off, increase_by_one)
  -> (cruise_active, target_speed)` operator driven by an owned **Off ⇄ On**
  state machine — engage captures the road speed as the set-point, `increase_by_one`
  raises it, `brake`/`turn_cruise_off` disengage. Driven through the whole loop:
  it **typechecks clean** (the Lustre is correct), **simulates** with correct
  cruise behavior, and its **C-Lite, compiled and run on the same input vector,
  produces the identical trace** — model and compiled code agree cell-for-cell.
  `tests/cruise_control.rs` generates the shipped `.wksc` + verifies typecheck +
  sim + emission; the compiled-vs-model match was confirmed with MSVC on the
  `scenarios/drive.csv` vector.

### 2026-06-18 — dependency detection & documentation (`openlustre doctor`)

The optional tools OpenLustre can use (a C compiler, Kind 2, Docker) are now
**documented with the functionality each unlocks, and detected at runtime** —
the installer ships the manifest and a one-click checker rather than bundling
third-party installers.

* **`openlustre doctor`** reports, for each optional dependency, whether it is
  present and what it enables: a **C compiler** (Compile C-Lite, debug run,
  dual-backend equivalence), **Kind 2** (contract proof / Verify tab), and
  **Docker** (the `clite-emulate` emulated-target backend). It distinguishes
  *installed* from *ready* — e.g. "Docker CLI present, but the daemon is not
  reachable" — and prints how to enable each missing one. The core
  design→check→simulate→generate workflow needs none of them.
* **`DEPENDENCIES.md`** documents the same as a table (unlocks / needed-for /
  how-to-get) and is shipped by the installer; the Windows installer adds a
  **Check Environment** Start-Menu shortcut that runs `openlustre doctor`.
* Verified live: `doctor` correctly reports MSVC present, Kind 2 absent, and
  Docker (29.5.3, daemon running) on this machine.

### 2026-06-18 — full-system arm64 emulation (`clite-emulate --system`)

Extends the emulation backend from qemu-user to a **real booted kernel/board**:
`--system` emits a Docker context that cross-compiles arm64, assembles a busybox
**initramfs** (the static model + an `/init` that runs it on the baked-in
scenario, frames the trace on the serial console, and powers off), and boots it
on **`qemu-system-aarch64 -M virt`** with a user-supplied kernel (`kernel/Image`
— the board/kernel choice is the engineer's). `extract_framed_trace` pulls the
CSV trace out of the boot log between markers; the comparison against the IR sim
is unchanged. `cmd_clite_emulate_system` orchestrates build → boot → extract →
compare where Docker is present, and otherwise emits the harness + a header-only
scenario template + clear next steps.

* **Why full-system is *integration*, not new equivalence:** the generated
  `_step` is pure compute (no syscalls), so the qemu-user backend already proves
  the compiled ARM behavior matches the model. Full-system adds booting the
  `integration.c`/driver on a real kernel — useful for the eventual RTOS story.
* **Verified:** unit tests pin the generated Dockerfile (static arm64 cross-link,
  initramfs assembly, `qemu-system-aarch64 -M virt` + `rdinit=/init`), the
  `/init` (frames + powers off), and `extract_framed_trace` (pulls the CSV from
  boot noise, errors on missing markers). Live generation verified on `Doubler`.
  **Not verified here:** the actual boot — this machine has no Docker/QEMU, and a
  boot harness has failure modes (kernel/console/busybox specifics) that need a
  first run on a Docker host. RTOS full-system (VxWorks) remains blocked by image
  availability.

### 2026-06-18 — Docker + QEMU emulated-target backend (`clite-emulate`)

The **third equivalence backend**: the generated C-Lite is cross-compiled for
armhf and run under QEMU user-mode inside Docker, and its trace is checked
against the IR simulator — beside the IR sim and the host-compiled C.

* **`crates/ol_cli/src/emulate.rs`.** Emits a self-contained Docker context:
  a `Dockerfile` (base `debian:stable-slim`, installs `gcc-arm-linux-gnueabihf`
  + `qemu-user-static`, **static-links** the model for armhf, and runs it under
  `qemu-arm-static`), plus the generated `.c`/`.h`, the CSV `driver.c` (and
  monitors when the operator has a contract), and a README. The binary stays the
  CSV driver — a vector on stdin, the trace on stdout — so one scenario drives
  all three backends. `traces_match` compares the emulated trace to the IR-sim
  reference cell-for-cell on the columns the driver emits.
* **`openlustre clite-emulate <model> [--node N] [--scenario csv] [--out dir]`.**
  Emits the harness; with a scenario it computes the IR-sim reference and, where
  Docker is on PATH, runs `docker build` + `docker run < scenario` and reports
  EQUIVALENT / MISMATCH. Where Docker is absent (e.g. this dev box) it writes
  `scenario.csv` + `expected_ir_trace.csv` and prints the exact commands — the
  harness runs unchanged on any Docker host. Live execution is Docker-gated, the
  same way the dual-backend C tests are cc-gated.
* **Verified:** unit tests in `emulate.rs` (the Dockerfile cross-compiles +
  qemu-runs, monitors included on contract, `traces_match` accepts a column
  subset and locates a mismatch / row-count / unknown-column error); and live —
  `clite-emulate` on `Doubler` emitted a correct armhf/qemu Dockerfile and the
  right IR reference (`y = 2x` → 6, 8, 10). Full-system board/RTOS emulation
  (a real VxWorks/bare-metal image under `qemu-system-*`) remains roadmap.

### 2026-06-18 — integration entry skeleton; empty new workspaces; C-Lite-only scope

Three follow-ups to the target-codegen work.

* **Generated integration entry point.** Compiling for a target now also emits
  `integration.c` — a *compilable* periodic-`_step` skeleton in the target's
  idiom (`ol_clite_emit::harness::emit_integration_main` + `IntegrationStyle`):
  a portable `main()` super-loop (host/Linux), a `taskSpawn`-able
  `<entry>_task` (VxWorks), or a `<entry>_tick()` for a timer ISR with static
  state (bare-metal). It declares the real API (`<entry>_init` / `<entry>_step`
  / the `_Input`/`_Output`/`_State` structs), `memset`s inputs to zero so it
  builds as-is, and marks the I/O stubs. `driver.c` stays the CSV test harness;
  `INTEGRATION.md` now points at `integration.c` (and the earlier doc's wrong
  `_reset`/`_state` names are gone — the file uses the emitter's real API).
  Verified: the host `integration.c` + generated `.c` compile cleanly with
  MSVC; unit tests pin each style in `harness.rs`.
* **New workspaces are empty.** File ▸ New Workspace (`/api/workspace/new`) no
  longer seeds the starter `Heartbeat` operator — a new workspace opens blank
  (`main: null`, no user operators); the engineer adds their own. (The CLI
  `new`/welcome demo still seed a starter.) Test strengthened to assert it.
* **C-Lite-only is a deliberate scope, not a gap.** The intro now states that
  Ada generation and MISRA-C-styled output are explicit non-goals — the product
  intentionally diverges from KCG and targets graphical Lustre → directional
  C-Lite only. The gap tables omit them on purpose.

### 2026-06-18 — target / OS build profiles (directional codegen)

The embedded-systems use case: generate C-Lite aimed at a specific OS/board, to
be built with that target's toolchain — SCADE's "generate, then integrate on the
target" workflow.

* **Target profiles.** `crates/ol_cli/src/target.rs` defines a `TargetProfile`
  (id, label, OS, arch, toolchain `cc`, CFLAGS/LDFLAGS, `cross`, integration
  note) and a built-in set: **host** (compiles locally with the auto-detected
  compiler, as before), **Linux x86-64 (gcc)**, **embedded Linux ARM**
  (`arm-linux-gnueabihf-gcc`), **VxWorks** (`wr-cc`), and **bare-metal ARM**
  (`arm-none-eabi-gcc`).
* **Directional generation.** `/api/clite/compile` takes a `target`: it emits a
  toolchain-tuned `Makefile` (right `CC`/`CFLAGS`/`LDLIBS`, target named in the
  header) and a target-specific `INTEGRATION.md` (build steps + the periodic
  `<entry>_step` integration pattern — `taskSpawn`/`taskDelay` for VxWorks, a
  timer ISR for bare-metal). The **host** target compiles locally as today; a
  **cross** target writes the build files and reports "build on the target
  toolchain" rather than attempting a cross-compile here. `makefile_for_entry`
  now delegates to the host profile (one source of truth).
* **UI.** The Compile-C-Lite dialog's target selector (previously a roadmap stub)
  is populated from `GET /api/targets`; choosing a cross target shows its
  toolchain note and explains the compiler box is ignored.
* **Verified.** Unit tests in `target.rs` (profiles, `find_target` defaulting to
  host, the Makefile carries the cross toolchain, `INTEGRATION.md` has balanced
  braces and names the step call). Live: all five targets list, the host target
  compiles `Doubler.exe`, embedded-Linux/VxWorks targets emit their files +
  `INTEGRATION.md` with the correct `arm-linux-gnueabihf-gcc` / `wr-cc`
  toolchains and are not compiled locally.
* **Roadmap (logged in §5):** a one-click **QEMU emulation + Docker** run of the
  generated build against the scenario suite — a *third* equivalence backend
  alongside the IR simulator and host-compiled C.

### 2026-06-18 — automata: inline nested-region & history authoring

The hierarchical-automata *engine* (nested `Region`s with restart-on-entry,
freeze, and **history**) has been in place since the June 16 slices, but the
text editor could only author flat machines and `refine`-by-reference. Now the
states box authors a nested region **inline**:

```
On: { initial Lo; history; Lo: beat=false; Hi: beat=true; Lo -> Hi when up; Hi -> Lo when down }
```

* **Client-only** — the server (`parse_sm_states`/`sm_state_json`) already
  round-trips nested `regions` with `history`, and the engine already lowers
  them; this slice is purely the editor. `fsmBuildPayload` parses the `{ … }`
  block (segments `initial Sub`, `history`, sub-state `Sub: lhs = expr`, and
  sub-transition `A -> B when g`) into the `regions` JSON; `fsmLoadIntoForm`
  reconstructs the block (so it round-trips); `fsmPreview` keyword-colours
  `initial`/`history` and the sub-states/transitions. One level of nesting in
  v1 (deeper nesting reports a clear error, never silently truncates).
* **Verified** end to end in the Studio: the client parse produces the exact
  `regions`/`history` shape and is round-trip-identical (load → re-parse); and a
  hierarchical, `history` machine authored this way, added to a fresh operator,
  is accepted, **lowers and typechecks clean** (both `…_StateEnum` and
  `…_r1_StateEnum`, and the nested-region activation local `__sm_r1_active`),
  and round-trips back through `/api/fsm` — all in a throwaway workspace so the
  user's workspace is untouched. The coloured preview was verified visually.
* **Still open (the next real automata feature):** **signals** — broadcast
  events within a synchronous step. Unlike history, signals are *not* in the
  engine yet; they need a new IR construct carried across typecheck, sim, and
  generated C (the project's "every stage" bar), so they are their own slice,
  not an editor-only addition.

### 2026-06-18 — file association & app icon (deployment)

The two §5 P1 deployment gaps, both in `packaging/windows`.

* **App icon.** `make-icon.ps1` generates `openlustre.ico` from scratch (no
  image tooling needed at build time): a white AND-gate D-shape with pin stubs
  on the app's blue (#2B579A) rounded square — the same silhouette the canvas
  draws — rendered with `System.Drawing` at 16/24/32/48/64/128/256 px and packed
  into a PNG-compressed multi-resolution `.ico` (validated by re-loading it and
  sampling pixels: transparent corner, blue field, white gate). The installer
  uses it for the Setup.exe (`SetupIconFile`), the Start-Menu and Desktop
  shortcuts (`IconFilename`), the file-type `DefaultIcon`, and the Add/Remove
  Programs entry (`UninstallDisplayIcon`).
* **File association.** A `[Registry]` block maps `.wksc` (the canonical
  workspace file) and `.ols` (a YAML model) to a `OpenLustreStudio.Model` ProgID
  whose `shell\open\command` runs `openlustre.exe studio launch "%1"` —
  `resolve_workspace` already serves a direct file path, so a double-click opens
  that model in the Studio. Written under `HKA` (per-machine on an elevated
  install, per-user otherwise), cleaned up on uninstall, with
  `ChangesAssociations=yes` so Explorer refreshes immediately. `.lus` is
  deliberately *not* claimed (it is an import-only format shared with other
  Lustre tooling) and `.json` is too generic to take over.
* Verified by building the installer end to end (`build-installer.ps1`): the
  release binary recompiles with the new icon embedded in the page, ISCC parses
  the new `[Registry]`/icon directives, packs `openlustre.ico`, and emits
  `OpenLustreStudio-0.1.0-Setup.exe`. (Installing and double-clicking a `.wksc`
  is the final manual confirmation.)

### 2026-06-18 — orthogonal wire routing & per-family gate silhouettes

The remaining §2 diagram-editor gaps, both GUI-only (studio_ui.html).

* **Orthogonal (Manhattan) wire routing.** The cubic-Bézier wire is replaced by
  a right-angle route with rounded elbows. `orthoPoints(x1,y1,x2,y2)` picks the
  route: a forward wire with a real vertical offset turns through a vertical
  channel at the mid-x; a near-aligned forward wire draws as one straight line
  (no pointless micro-jog); a backward/feedback wire (target left of source)
  stands off both box edges and shares a horizontal channel between them, so it
  detours *around* rather than looping through boxes. `roundedPath(pts, r)`
  renders any polyline with quadratic-rounded corners (collinear points collapse
  cleanly). All routing is in model space, so it is exact at any zoom.
* **Per-family gate silhouettes.** `gateFamily(it)` maps the symbol glyph to a
  family and `gateBody(fam, p, h, cls)` draws it: **AND** → a D-shape (flat
  back, semicircular front), **OR / XOR / ⇒** → a pointed shield, **ITE** → a
  multiplexer trapezoid (tall input edge, short output edge), **pre / FBY / ->**
  → a rectangle with a register bar; everything else stays the rounded rect.
  Every silhouette keeps a **straight left edge at `x`** (so the per-operand
  input pins stay seated on it) and reaches its **right tip at `x+w`** (so the
  output pin stays seated) — verified by measuring each shape's bounding box.
  The shapes reuse the `.box`/`.eq`/`.invalid`/`.selected` classes, so red
  invalid-coding and the selection highlight apply unchanged.
* GUI-only slice: the four new helpers were verified live in the browser — the
  served page parses cleanly (functions defined, zero console/server errors), the
  real Heartbeat operator's AND renders as a D-shape with correctly-seated pins,
  the three wire cases (straight / channel-elbow / feedback-detour) all draw
  correctly, and a probe gallery confirmed OR/ITE/pre/default. The Rust suite is
  untouched (no Rust changed; 214 tests green at baseline).

### 2026-06-18 — Tool Operational Requirements document

The fourth and final pillar of verification-by-equivalence (§4). New
`docs/TOOL_OPERATIONAL_REQUIREMENTS.md` is a DO-330-style TOR: it states, as
numbered requirements (TOR-001 … TOR-702), every operation the tool claims —
model management & authoring (1xx), static verification (2xx), simulation (3xx),
code generation (4xx), formal verification (5xx), test/coverage/equivalence
(6xx), deployment (7xx) — and binds each to its DO-330 verification method
(Test/Analysis/Review) and **evidence**: the test file/function or source
location that demonstrates it. A §8 enumerates the conscious v1 limits (each
loud, none silent) that bound the claims, and §9 records the verification
baseline (214 passing tests, 0 failed, across 57 result groups on Windows/MSVC).
The document's premise is that the test suite *is* the verification record —
a requirement is unmet the moment its cited test fails, so there is no separate
drifting artifact. Pure documentation; the test evidence already existed.

### 2026-06-18 — canvas copy/paste & marquee select

The second §2 editor-polish slice, building on the zoom/pan transform.

* **Rubber-band selection.** Dragging on empty canvas draws a selection
  rectangle (model-space, so it's correct at any zoom) and selects every
  operation/variable box it overlaps; Shift/Ctrl adds to the existing
  selection, a plain click still clears. A bare click vs. a drag is
  distinguished by a 3 px threshold.
* **Block clipboard.** Ctrl+C/Ctrl+X copy/cut the selected operation blocks
  into an in-session clipboard (also on the right-click menu); Ctrl+V pastes.
  The clipboard snapshots each equation's `lhs`, body text, and position — not
  indices — so it survives later edits.
* **Server `/api/edit/paste`.** One journaled (undoable) edit clones the
  equations into the operator: each result local gets a fresh name
  (`s1` → `s1_2` → `s1_3` …), references *among the copied set* are rewired to
  the new names (a copied chain stays connected) while references outside it
  are left to resolve against existing signals or surface as red unbound pins,
  and result types are resolved by name from the live node — pasting a block
  whose original was deleted is a clear error, not a wrong guess. Repeated
  pastes cascade by a growing offset. Covered by
  `paste_clones_equations_and_rewires_internal_references`
  (tests/numeric_cast_and_operations.rs).

### 2026-06-18 — canvas zoom & pan

The first of the §2 editor-polish gaps, and the foundation for the rest
(marquee-select and copy/paste all depend on a correct screen↔model transform).

* **Zoom as a pure presentation scale.** The diagram SVG keeps a fixed
  `viewBox="0 0 W H"` (model units) and is painted at `W·zoom × H·zoom` CSS
  pixels, so every box/wire/pin coordinate, the grid, snap-to-grid, and the
  saved layout are completely unchanged — zoom is never persisted or mixed into
  the model. `#diagram-host`'s scrollbars navigate the scaled canvas.
* **One coordinate transform for everything.** `clientToModel()` (used by
  `svgPoint`, the drag/resize/wire handlers, *and* the palette-drop handler)
  now goes through `svg.getScreenCTM().inverse()`, so pointer math is exact at
  any zoom or scroll offset — the previous `getBoundingClientRect()` subtraction
  only worked at 1:1.
* **Gestures.** Ctrl/⌘+wheel (and trackpad pinch, which arrives as a
  ctrl-wheel) zooms toward the cursor by anchoring the model point under the
  pointer; middle-button drag and Space+left-drag pan (scroll the host);
  scrollbars pan natively. View ▸ Zoom In/Out/Reset/Fit, Ctrl +/−/0, and a
  click-to-reset `%` badge in the diagram header. Zoom clamps to 25 %–400 %.
* GUI-only slice: JS syntax-checked, the served page verified to carry the new
  controls and code, full Rust suite still green. (Browser-driven interaction
  verified live, per the project's GUI-testing convention.)

### 2026-06-18 — float intrinsics (`sqrt`, `sin`, `cos`, `abs`, `min`, `max`, …)

`square_root` was greyed out pending "float intrinsics across sim/C/Lustre";
that family now exists, modelled exactly on `numeric_cast`.

* **IR.** A new `Expr::Intrinsic { func: FloatFn, args }` node, where `FloatFn`
  is the closed set of math built-ins the core operators cannot express —
  `Sqrt`, `Sin`, `Cos`, `Tan`, `Exp`, `Log`, `Abs` (unary), `Min`, `Max`, `Pow`
  (binary). It carries surface/Kind 2 names, arity, and the `<math.h>` C name
  (with the `f`-suffixed variant reserved for future `float32`).
* **Surface + Kind 2.** Function-style: `sqrt(x)`, `min(a, b)`. The parser maps
  the reserved names to the intrinsic (arity-checked), the formatter round-trips
  them, and the Kind 2 view emits the same call as a user-suppliable function —
  the established cast / bit-op convention.
* **Typecheck.** Operands must be `float64` (`real`) and the result is
  `float64` (E0160 arity, E0161 non-`float64`). Restricting to one width keeps
  the IR simulator (`f64`) and the generated `<math.h>` *double* calls exact,
  cell for cell. `float32` intrinsics are roadmap (they need `sqrtf`/`sinf`/…
  with matching f32 rounding in the simulator).
* **Sim + C.** The simulator evaluates each intrinsic in `f64`
  (`fmin`/`fmax`-matching NaN handling for `min`/`max`); the C emitter lowers to
  the double `<math.h>` functions, adds `#include <math.h>` to the generated and
  monitor sources, and links `-lm` (Makefile + both scenario/compile paths).
* **Authoring.** The Mathematics toolbox family enables `square_root` and adds
  `sin/cos/tan/exp/log/abs/min/max/pow`, each with a `float64` pin contract;
  dropping one lands a typed `float64` result local and a `sqrt(pin)` /
  `min(pin, pin)` equation.
* **Tested** end to end: parser round-trip + arity errors, an E0161 type error,
  an `f64` simulation (including `sin`/`cos` at clean points), a generated-C
  string check for the `<math.h>` calls, and a dual-backend IR↔compiled-C
  equivalence scenario (integer-valued results so the float formatting matches).

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
source spans in diagnostics, MC/DC masking analysis, requirements
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
