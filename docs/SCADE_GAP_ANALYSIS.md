# OpenLustre Studio vs. Ansys SCADE Suite — gap analysis

*Updated 2026-06-11.*

OpenLustre Studio aims to be the open, SCADE-shaped workbench: author synchronous
dataflow models graphically, check them, simulate them deterministically, prove
contracts, generate C, and test the generated code against the model. SCADE Suite
is the industry-accepted, DO-178C-qualified original. This document is honest about
which gaps are **bridgeable engineering work** and which are **structural** (you
cannot code your way to a qualification certificate), and prioritizes the former.

## 1. Where the products stand today

| Workflow step | SCADE Suite | OpenLustre Studio today |
|---|---|---|
| Graphical authoring | Full diagram editor: palette drag-drop, pin-to-pin wire drawing, hierarchical sheets | Form-based authoring + draggable, grid-snapped canvas with persisted layout; palette inserts call text; red color-coding of invalid links |
| Language | Scade 6 (Lustre core + clocks, automata, iterators, packages) | Strict Lustre subset: dataflow, `pre`/`->`, records/enums/arrays, constants, flat FSMs (lowered), imported C operators |
| Static checks | Type/clock checker | Type checker + contract checker (vacuity, unreachability, overlap), live in the GUI |
| Simulation | Cycle stepping, watch, plots, co-simulation | Cycle stepping with full trace of every named item; CSV batch simulation; golden-trace scenarios |
| Formal verification | Design Verifier (Prover plug-in) | Kind 2 adapter (BMC/induction, realizability, mode coverage) + CoCoSpec contract emission, in-GUI Verify tab |
| Code generation | KCG qualified C/Ada (TQL-1) | C-Lite emitter + contract monitors + CSV driver + Makefile, selected-root slicing |
| Testing | SCADE Test: harness, MTC, MC/DC on model | Scenario harness: golden traces against IR simulator **and** compiled C; decision coverage with uncovered-direction reporting |
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
| **P0 — Pin-to-pin wire drawing** | Drag from an output pin to an input pin creates a connection | Pin-level ports on boxes; drag pin→pin rewrites the target call argument (today binding goes through the right-click Bind panel) | Medium |
| **P1 — Undo/redo** | Standard | Edit-journal on the server (every edit endpoint already round-trips the file; keep N previous states) | Small |
| **P1 — Orthogonal wire routing** | Manhattan-routed wires with junctions | Replace cubic Béziers with channel routing | Medium |
| **P1 — Zoom/pan, multi-select, copy/paste** | Standard | SVG viewBox transforms + selection rectangle | Medium |
| **P2 — Multi-sheet diagrams** | One operator can span sheets | Page list per node in `DiagramLayout` | Medium |
| **P2 — Block symbols** | Distinct shapes per operator family (gates, delays, switches) | Symbol library keyed by stdlib block name | Small, cosmetic |

## 3. Language and toolchain gaps

| Gap | Notes | Priority |
|---|---|---|
| Source spans in diagnostics | `Diagnostic.span` exists but is never populated; GUI cannot jump to file:line:col | P0 |
| Clocks (`when` / `merge`) | The single biggest Lustre-language omission; Kind 2 and codegen both support clocked code | P1 |
| Hierarchical/parallel automata | Our FSMs are flat Moore-style; SCADE automata nest, run in parallel, carry history and signals | P1 |
| Array iterators (`map`/`fold`) | Needed for vector-heavy avionics models; today arrays exist but loops must be unrolled | P1 |
| MC/DC proper | Decision coverage landed; MC/DC needs per-condition masking analysis on the same substrate | P1 |
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
3. **Coverage evidence** — decision coverage today, MC/DC next (§3).
4. **Tool Operational Requirements document** — enumerate what the tool claims to do,
   with the test suite as verification cases (not started; pure documentation work,
   P1 if certification-adjacent use is a goal).

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

## 6. What closed this session (2026-06-11)

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
