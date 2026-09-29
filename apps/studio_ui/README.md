# OpenLustre Studio UI

This directory documents the OpenLustre Studio graphical editor.

A first browser-based front end ships **inside the `openlustre` binary**:

```bash
openlustre studio serve path/to/model.ols --with-stdlib libraries --port 8181
# studio: serving http://127.0.0.1:8181 (model: path/to/model.ols)
```

Open the printed URL in any browser to get the Project Explorer, the
diagnostics panel, the generated Lustre and C-Lite views, and a
simulation runner — no JS toolchain, no separate install, no Node, no
Tauri build step. The page re-fetches the JSON inspection every five
seconds so external edits to the model are picked up on the fly.

The dataflow canvas is a SCADE-style drafting surface:

- **Operator glyphs** — recognized blocks render as real shapes: MIL
  AND/OR/XOR gates, the NOT triangle-and-bubble, the if/then/else
  selector trapezoid (condition pin on the sloped top edge), temporal
  blocks with a state bar, pointed literal tags; inputs and outputs are
  flow-direction pennants and flags.
- **Orthogonal wires** — rounded-corner Manhattan routing (with a
  feedback loop-around for backward wires), source-anchored
  `name: type` annotations, and a View-menu toggle back to curves.
- **Navigation** — Ctrl+wheel zoom about the cursor (25%–400%),
  Zoom to Fit / 100% in the View menu, middle-drag panning, and a
  status-bar zoom readout.
- **Selection** — rubber-band marquee on empty canvas, ctrl-click
  toggling, group dragging of the whole selection, arrow-key nudging
  (grid-step; Shift for 1 px), delete of everything selected.
- **Layout tools** — align lefts/rights/tops/bottoms/centers and
  distribute horizontally/vertically, from the Diagram menu or the
  canvas right-click menu; positions persist into the model file.
- **Clipboard** — Ctrl+C / Ctrl+V / Ctrl+D (and the context menu's
  Copy / Paste / Paste-here / Duplicate) copy a selection and paste it
  into any operator's canvas: pasted results get fresh local names,
  wiring inside the pasted group is kept, and references to signals the
  target operator doesn't have surface as red pins to re-bind.
- **Minimap** — a corner overview of the whole diagram with the
  current viewport outlined; click or drag it to jump. View-menu
  toggle.
- **Export** — the Diagram menu writes the current diagram as a
  standalone `.svg` (styles inlined) or a 2× `.png`.

The state-chart view in the State Machines dialog is a drafting surface
too: drag states to arrange the chart (transitions re-route live, the
arrowheads land on the target's rim, the initial state carries its entry
arrow), and the arrangement persists into the model file per machine —
`/api/edit/set_fsm_layout`, journaled like every other edit, kept across
textual machine updates.

Contracts are edited in the Contracts dialog (Project ▸ Contracts,
Insert ▸ Contract, the tree, an operator's right-click menu, or the chip
beside the diagram's operator name): clause rows, a mode table, and a
CoCoSpec text view over the same clauses. `/api/contract` serves them,
`/api/contract/check` dry-runs an edit (introduced errors, every
diagnostic about the contract, its CoCoSpec text), and
`/api/edit/{add,update,remove}_contract` save them; the interface is
copied from the operator and re-synced on port edits.

Conditional activations (SCADE activate-if) have their own dialog —
Insert ▸ Activation, the palette's *Activation (if / elsif / else)* item,
or the workspace tree — with a one-branch-per-line editor
(`if c as Name: lhs = e; …`, `elsif …`, `else: …`) and a live
decision-tree chart. The server (`/api/activation`,
`/api/edit/{add,update,remove}_activation`) lowers and type-checks the
tree before saving and rejects only the errors the edit introduces. On
an owner's canvas, state machines and activations are single blocks —
`/api/diagram` returns them in `constructs` (reads, drives, states or
conditions) and keeps the generated internals off the diagram.

Simulation is a server-side session, so stepping is incremental rather
than a replay. `POST /api/sim/start {"node": N}` type-checks the
operator's slice and opens a session (one per server; it lives on its own
thread with the simulator's state) and returns the signal list
(`[{name, kind: input|local|output, type}]`). `POST /api/sim/step
{"inputs": {name: "text"}, "count": 1..10000, "break": "expr",
"stop_on_violation": bool}` runs up to `count` cycles with the same
inputs and returns `{cycle, rows: [{cycle, values, modes, violations}],
stopped: count|break|violation|error}`; the break expression is a
stateless condition over the operator's signals (temporal operators are
rejected). A step after a semantic model edit answers `409
{"stale": true}` and the client restarts the session. `POST
/api/sim/stop` ends it and `GET /api/sim/state` reports it. Contract
monitoring uses the same observer node as the generated C, so the modes
and violations shown per cycle match a compiled run.

A step can also carry `"sequence": [{name: "text"}, …]` — one input set
per cycle, with `inputs` filling any gaps — which is how a test scenario
or a Kind 2 counterexample is replayed in the session. Every step of the
sequence is parsed before any runs, so a bad value runs nothing (the
error names the step).

The waveform viewer draws three sources. The session's rows, kept by the
client (the newest 10000 cycles). `POST /api/tests/run`'s `traces`: per
scenario, the `golden`, `ir` and `c` traces as `{header, rows,
truncated}` (at most 5000 rows each; the C trace has the golden's cycle,
output and monitor columns). And `POST /api/prove`'s per-property
`trace`: the counterexample as `{cycles, streams: [{scope, name, type,
class, values}]}` alongside the text `waveform`, plus the `main`
operator it belongs to.

The Tauri shell described below is still the longer-term target (it
gives native desktop windows, file-pickers, and a block-diagram
ReactFlow canvas), but the back-end contract is what was actually
missing — and the SPA shipping in `studio serve` proves it.

## Target stack

The plan calls for a Tauri + ReactFlow front end. That gives:

- Native desktop binaries (Tauri).
- Block-diagram editor (ReactFlow).
- A clean separation between the front end (TypeScript/React) and the
  back end (the Rust crates in this repository).

## How the GUI talks to the back end

There is **no in-process language binding**. The GUI shells out to the
existing `openlustre` CLI, which already exposes every operation the GUI
needs. This keeps the back end one binary, language-agnostic, scriptable,
and trivially reproducible from a terminal.

The CLI exposes a stable JSON IPC surface through the `studio` sub-command:

```bash
# Project Explorer + diagnostics panel: one JSON document.
openlustre studio inspect path/to/model.ols [--with-stdlib libraries] [--pretty]
```

Output schema (versioned, additive — fields are only ever added):

```json
{
  "schema_version": 1,
  "tool": "openlustre studio inspect",
  "project": {
    "name": "...",
    "main": "...|null",
    "package_count": N,
    "node_count": N,
    "packages": [
      {
        "name": "...",
        "types": [{"name": "...", "body": {...}}],
        "constants": [{"name": "...", "type": {...}}],
        "nodes": [
          {
            "name": "...",
            "kind": "Function|Operator|Imported",
            "inputs": [{"name": "...", "type": {...}}],
            "outputs": [{"name": "...", "type": {...}}],
            "locals": [{"name": "...", "type": {...}}],
            "equation_count": N,
            "contract": "...|null"
          }
        ],
        "contracts": [
          {
            "name": "...",
            "assumption_count": N,
            "guarantee_count": N,
            "mode_count": N,
            "modes": ["..."],
            "import_count": N
          }
        ],
        "state_machine_count": N
      }
    ]
  },
  "diagnostics": [
    {
      "severity": "Error|Warning|Info",
      "code": "E0040",
      "message": "...",
      "context": ["..."],
      "source": "typecheck|contract"
    }
  ],
  "summary": { "errors": N, "warnings": N }
}
```

The other plan-listed GUI panes already have CLI commands behind them:

| GUI pane | CLI command | Output |
|---|---|---|
| Project Explorer | `studio inspect` | JSON above |
| Generated Lustre view | `emit-lustre --out DIR` | `DIR/model.lus`, `DIR/contracts.lus` |
| Generated C-Lite view | `emit-clite --out DIR` | `DIR/clite/*.{c,h}`, monitors, optional driver and imported-operator wrappers |
| Simulation Trace | `simulate --inputs CSV` | CSV trace (matches the per-cycle waveform shape) |
| Proof Results | `prove [--timeout SECS] [--property NAME] [--waveform]` | One line per property; counterexamples as JSON or ASCII waveform |
| Counterexample Viewer | `prove --waveform` | Fixed-width per-cycle table |
| Diagnostics panel | `studio inspect` `diagnostics[]` | structured error / warning / info list |
| Block library palette | `lib-check libraries` + this README | 41 blocks across 8 categories |

## What's left to build

The GUI layer itself:

1. **Front end shell** — Tauri main process that exec's the CLI for every
   operation. No `ipc::invoke` calls reach into Rust crates directly;
   everything is text in / text out, which keeps the back end testable and
   the GUI freely rewritable in any language.
2. **Block Diagram editor** — ReactFlow canvas backed by a project file.
   Saves through round-tripping the IR's existing JSON / YAML format (the
   `Project` struct is `Serialize + Deserialize` and the loader already
   handles `includes:` for multi-file projects).
3. **Contract Editor + Mode Table** — structured forms editing the
   `ContractDef` JSON the IR already understands. Plus a raw-text mode
   that runs through the textual library parser.
4. **Trace + Proof viewers** — read the trace CSV / `studio inspect`
   diagnostics / `prove --waveform` output.

Because every back-end capability is already a CLI command with a stable
output shape, the front end can be built incrementally — one pane at a
time, each one wrapping the matching CLI command — without ever needing
to modify the Rust crates.

## Standard-library block palette

The `studio inspect` schema's `packages[].nodes` field is what the GUI's
block palette renders. Today the standard library (loaded with
`--with-stdlib libraries`) advertises **41 blocks** across these
categories:

- core logic — `And`, `Or`, `Not`, `Xor`, `Mux`, `Switch`
- math — `Add`, `Subtract`, `Multiply`, `Divide`, `Min`, `Max`, `Clamp`,
  `Saturate`, `RateMonitor`
- comparison — `Equal`, `NotEqual`, `Less`, `LessEqual`, `Greater`,
  `GreaterEqual`
- temporal — `RisingEdge`, `FallingEdge`, `Latch`, `Delay`, `Counter`,
  `Timer`
- safety — `Watchdog`, `RangeCheck`
- observer — `Assert`, `Assume`
- bits — `BitAnd`, `BitOr`, `BitXor`, `ShiftLeft`, `ShiftRight`
- avionics — `Arinc429Label`, `Arinc429SDI`, `Arinc429Payload`,
  `Arinc429SSM`
- state-machine — `SRFlipFlop`

## Running the back end without a GUI

Every panel-equivalent command exists today and can be exercised from a
terminal. A reference smoke flow:

```bash
openlustre check    model.ols --with-stdlib libraries
openlustre simulate model.ols --inputs tests/input.csv --with-stdlib libraries
openlustre emit-clite model.ols --out build/ --with-stdlib libraries --driver
openlustre prove    model.ols --with-stdlib libraries --timeout 30 --waveform
openlustre studio inspect model.ols --with-stdlib libraries --pretty
```

Once the GUI is in place, those same commands are what its panels run
internally.
