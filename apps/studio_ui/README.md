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
