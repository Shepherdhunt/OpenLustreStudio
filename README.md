# OpenLustre Studio

**An open-source, SCADE-like graphical modeling IDE for safety-critical
embedded software.** Engineers graphically design synchronous models —
dataflow blocks, if/then/else logic, state machines, math, mode-aware
CoCoSpec contracts — whose native storage and semantics are
**Lustre**. The selected root operator, and everything it transitively
uses, is auto-generated into **Directional C-Lite** that compiles and
provably behaves identically to the simulated model.

OpenLustre Studio is **not a SCADE replacement**. It is a similar
models-to-source-code capability — open, scriptable, and verifiable —
for teams that want the SCADE workflow shape (draw → check → simulate →
generate → test → prove) without a qualified-tool license, or as a
front-of-pipeline workbench before downstream SCADE / qualified
code-generation flows.

```text
The model is not just equations.
The model is equations + contracts + modes + evidence.
```

## Installing

**Windows** — download `OpenLustreStudio-<version>-Setup.exe` from the
releases page and run it. You get a normal install wizard, a Start Menu
entry, and an optional Desktop shortcut; double-clicking the shortcut runs
`openlustre studio launch`, which starts the Studio and opens your browser
on a welcome project (created at `%USERPROFILE%\OpenLustre` on first run).
The 41-block standard library is embedded in the binary — nothing else to
install. (Installer built from `packaging/windows/openlustre.iss`; the
`release` GitHub Actions workflow produces it on every version tag.)

**Linux / macOS** — grab the release archive (or `cargo build --release
-p ol_cli`), then `./packaging/linux/install.sh` to get the binary in
`~/.local/bin` plus an application-menu shortcut, or just run:

```bash
openlustre studio launch        # starts the Studio + opens your browser
```

## The workflow

```bash
# 1. Open the Studio in a browser (the embedded block library loads
#    automatically; --with-stdlib DIR overrides it for development):
openlustre studio launch model.json
#    → Project Explorer, dataflow Diagram, Edit forms (create operators,
#      ports, equations with if/math/temporal ops and a 41-block library
#      palette), SCADE-style Step tab (deterministic value for EVERY item,
#      every cycle), Tests tab, generated Lustre + C-Lite views, Build tab.

# 2. Check the model (types, clocks, contracts, modes):
openlustre check model.json --with-stdlib libraries

# 3. Simulate (batch or stepped; full traces carry every signal):
openlustre simulate model.json --inputs scenario.csv --with-stdlib libraries

# 4. Generate C for the SELECTED operator and all that it uses (SCADE KCG
#    behavior — nothing unused leaks into the generated source):
openlustre emit-clite model.json --root MyOperator --with-stdlib libraries \
    --out build/ --driver
cd build/clite && make        # → standalone executable named after the
                              #   user-designated main operator

# 5. Verify model ↔ generated C equivalence with golden-trace scenarios:
openlustre test record model.json --scenarios scenarios/
openlustre test run    model.json --scenarios scenarios/ --backend both
# [PASS] nominal (ir)   [PASS] nominal (c)   ← byte-identical traces, CI-ready

# 6. Prove properties with Kind 2 (counterexamples as per-cycle waveforms):
openlustre prove model.json --timeout 30 --waveform
```

## What makes it OpenLustre (differences from SCADE)

* **User-defined entry point** — any operator can be designated `main`;
  the generated build produces a standalone executable named after it.
  (SCADE fixes the runtime shape; OpenLustre lets the model own `main`.)
* **Contracts are first-class** — CoCoSpec assume/guarantee/mode clauses
  live beside the equations, are checked statically (vacuity,
  unreachable modes, import signatures), monitored at runtime in both
  the simulator and the generated C, and proved with Kind 2.
* **Everything is a CLI command** — the GUI shells the same commands, so
  every panel action is scriptable and CI-able.

## Repository layout

```text
crates/
  ol_ir              strict dataflow + state-machine IR, project slicer
  ol_contract_ir     CoCoSpec contract IR
  ol_typecheck       types, records/enums/arrays, no-implicit-narrowing
  ol_contract_check  contract well-formedness + vacuity/unreachability
  ol_lustre_emit     Lustre emitter (Kind 2-compatible)
  ol_cocospec_emit   contract emitter (modern con/noc + legacy)
  ol_clite_emit      Directional C-Lite + monitors + drivers + Makefile
  ol_sim             cycle-accurate IR interpreter (full-trace stepping)
  ol_kind2           Kind 2 adapter (timeout, property selection, waveforms)
  ol_stdlib          41-block library loader (logic/math/temporal/safety/
                     observer/bits/avionics/state-machine categories)
  ol_cli             the `openlustre` binary: every command + Studio server
libraries/           the standard block library (YAML, contract-carrying)
examples/            ReleaseLogic MVP with committed golden-trace scenarios
apps/studio_ui/      GUI architecture notes (browser SPA ships in the binary)
```

## Verification spine

The repository's tests enforce the load-bearing invariant end to end:
**the IR simulator and the compiled generated C produce byte-identical
traces** — across stateful operators, state machines, compound types,
bit manipulation, constants, contract monitors, and user scenarios.
`openlustre test run --backend both` puts that same invariant in users'
hands for their own models.
