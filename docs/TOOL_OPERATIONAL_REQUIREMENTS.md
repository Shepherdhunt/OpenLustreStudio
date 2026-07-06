# OpenLustre Studio — Tool Operational Requirements (TOR)

*Version 0.1, 2026-07-03. Companion to `docs/SCADE_GAP_ANALYSIS.md` §4.*

This document enumerates what OpenLustre Studio **claims to do** when used in
a development flow for safety-related synchronous software, and maps every
claim to the automated verification evidence that demonstrates it. It is
written in the spirit of a DO-330 Tool Operational Requirements document, for
one honest reason:

> **OpenLustre Studio is NOT a qualified tool.** It carries no DO-330/DO-178C
> qualification credit, and nothing in this repository can create such
> credit. The intended usage pattern is **verification-by-equivalence**: the
> applicant independently verifies the tool's *outputs* (the generated C, the
> proof results, the coverage figures), and this document plus the test suite
> tell them precisely what the tool intends those outputs to mean and how
> each intention is checked.

Under DO-178C this positions the tool where an unqualified tool belongs: its
outputs receive the same review/verification any hand-written artifact would.
What the tool *adds* is mechanical assistance plus an executable body of
evidence (`cargo test --workspace`) that its checks, simulator, and code
generator agree with each other.

## 1. Tool identification and operational environment

| Item | Value |
|---|---|
| Tool | OpenLustre Studio (`openlustre` CLI + embedded Studio GUI) |
| Repository layout | Rust workspace: `ol_ir`, `ol_typecheck`, `ol_contract_check`, `ol_lustre_emit`, `ol_cocospec_emit`, `ol_clite_emit`, `ol_sim`, `ol_kind2`, `ol_stdlib`, `ol_contract_ir`, `ol_cli` |
| Verified platforms | Windows 11 (MSVC), Linux (gcc/clang); the scenario harness discovers `cc`/`gcc`/`clang` or MSVC via `vswhere`/`vcvars64.bat` |
| External tools | A C11 compiler (for generated-code execution and equivalence testing); optionally Kind 2 (for formal proofs) |
| Verification evidence | `cargo test --workspace --no-fail-fast` — 58 green result groups as of 2026-07-03; individual mappings below |

Operational commands covered by this TOR:

```text
openlustre check            type-check + contract-check a model
openlustre emit-lustre      emit Lustre + CoCoSpec
openlustre emit-clite       emit Directional C-Lite + contract monitors
openlustre simulate         run the IR simulator on a CSV vector
openlustre prove            run Kind 2 on the generated Lustre
openlustre contract-check   contract checks only
openlustre lib-check        check every standard-library block
openlustre test record/run  golden traces; IR vs compiled-C comparison
openlustre studio …         the GUI (same headless endpoints underneath)
```

## 2. Operational requirements

Each requirement is stated as a claim the tool makes. **Evidence** names the
test files (under `tests/`) and, where relevant, the diagnostic codes that
make a violation loud rather than silent. Diagnostics carry stable codes
(`E0001`–`E0174` today) so review checklists can reference them.

### TOR-1 — Model well-formedness (type system)

The tool shall reject, with a located diagnostic, any model that:

- wires incompatible types, calls a node with a mismatched signature, or
  implicitly narrows a numeric type (`E0080`–`E0094` family);
- assigns an output twice, leaves an output unassigned, or assigns an
  undeclared name;
- uses temporal operators (`pre`, `->`) or stateful calls inside a stateless
  `function`;
- uses `pre` without an initializing `->` (`E0070`);
- contains a combinational cycle not broken by a temporal operator (the
  dependency checker; a true cycle is also a loud simulator error, never a
  wrong answer);
- misuses `numeric_cast` (`E0093`/`E0094`) or a float intrinsic
  (`E0160`/`E0161` — arity, and float64-only operands with an explicit-cast
  hint).

**Evidence:** `typecheck_rules.rs`, `typecheck_records_enums.rs`,
`compound_types.rs`, `evaluation_order.rs`, `numeric_cast_and_operations.rs`,
`float_intrinsics.rs`, `bit_ops.rs`, `iterators.rs` (E0140–E0146),
`constants.rs`, `composite_constants.rs`.

### TOR-2 — Clock calculus

Boolean clocks (`when` / `when not` / `merge`) shall obey a checked clock
discipline (`E0130`–`E0135`): mixed-clock operands, non-boolean clocks,
clocked outputs, clocked equations in functions, and clocked arrays are
rejected; the same calculus drives the typechecker, the simulator, and the C
emitter so all three agree on which cycles an equation runs.

**Evidence:** `clocks.rs` — including dual-backend (IR vs compiled C)
cell-by-cell agreement on gating patterns (off-start, bursts, long holds).

### TOR-3 — Contract well-formedness and analysis

Contracts (CoCoSpec: assume / guarantee / mode require/ensure) shall be
checked for: assumptions independent of current outputs, boolean-typed
clauses, contract-local ghost variables, signature-compatible imports, and
warned for vacuity, unreachability, overlap, and missing public contracts.

**Evidence:** `contract_check_polish.rs`, `release_logic_pipeline.rs`,
`safety_blocks.rs`; the standard library's own contracts are checked by
`stdlib_library.rs` and `openlustre lib-check`.

### TOR-4 — Deterministic simulation semantics

The IR simulator shall execute a model cycle-by-cycle deterministically:

- equations evaluate in data-dependency order (`ol_ir::evaluation_order`),
  never declaration order — forward references read this cycle's value, and
  a genuine cycle is an error;
- `pre`/`->` follow Lustre initialization semantics; clocked equations hold
  their last value through inactive cycles; clocked `->` counts ticks of its
  own clock;
- numeric semantics match C: two's-complement narrowing on `numeric_cast`,
  float32 emulation by rounding through `f32`, float intrinsics computed in
  `f64` (the same double-precision libm family the generated C calls).

**Evidence:** `evaluation_order.rs` (including golden-content checks so the
dual-backend comparison can never pass on matching *wrong* values),
`clocks.rs`, `numeric_cast_and_operations.rs`, `float_intrinsics.rs`,
`state_machines.rs`, `scenario_harness.rs`.

### TOR-5 — Directional C-Lite generation

Generated C shall be a restricted, reviewable dialect:

- no dynamic memory, no recursion, no function pointers, no pointer
  arithmetic, no hidden globals;
- fixed-width integer types only; explicit `<Node>_Input` / `_Output` /
  `_State` structs and explicit `_init` / `_step` entry points;
- identifiers colliding with C keywords or the generated parameter names
  mangle deterministically (model names stay in CSV headers/traces);
- proof-only logic (contract monitors) is generated into a separate monitor
  harness, never into production code paths; debug probes compile only
  under `-DOL_DEBUG`;
- float intrinsics call the `<math.h>` double family and the emitted
  Makefile/harness link `-lm` on POSIX.

**Evidence:** `state_machine_codegen.rs`, `selective_codegen.rs` (slicing
keeps exactly the reachable nodes/functions), `industry_hardening.rs`
(hostile identifiers, dual-backend), `runtime_monitor.rs` (monitors),
`float_intrinsics.rs`, `critical_items.rs`.

### TOR-6 — Model/code equivalence (the core claim)

For every recorded test scenario, the compiled generated C shall produce a
trace **textually identical, cell by cell,** to the IR simulator's trace for
the same input vector (`openlustre test run --backend both`). This is the
claim that substitutes for a qualified generator: the model you simulated is
the code you run, demonstrated per project, per scenario, on the applicant's
own machine.

Known bound: libm transcendentals (`sin`, `exp`, …) are not correctly-rounded
and may differ across *platforms* by an ulp; the exactly-rounded subset
(`sqrt`, `abs`, `min`, `max`, `floor`, `ceil`, `round`, `pow` on
representable cases) is byte-stable and is what the repository's own
equivalence tests pin. On any single machine both backends call the same
libm.

**Evidence:** `trace_comparison.rs`, `scenario_harness.rs`,
`release_logic_pipeline.rs`, plus the dual-backend assertions inside
`clocks.rs`, `iterators.rs`, `industry_hardening.rs`,
`numeric_cast_and_operations.rs`, `float_intrinsics.rs`,
`state_machine_codegen.rs`.

### TOR-7 — State machine lowering

State machines (flat, hierarchical via nested regions or `refine`,
operator-owned) shall lower to plain dataflow with SCADE-strict validation:
unknown initial/target states rejected, and **every output assigned along
every path** (checked recursively through the state tree), restart-on-entry
vs history semantics, freeze-while-inactive.

**Evidence:** `state_machines.rs`, `state_machine_codegen.rs`,
`library_state_machines_and_studio_api.rs`, `critical_items.rs`.

### TOR-8 — Coverage measurement

The scenario harness shall measure, on the IR backend: decision coverage and
**MC/DC** (the DO-178C Level A metric) — each atomic condition's value
captured in a single evaluation pass, independence pairs analyzed
suite-wide, uncovered conditions reported by name. Unique-cause pairs are
sought first; where none can exist (notably **coupled conditions** — the
same condition appearing more than once in a decision), the **masking
analysis** applies: a pair qualifies when the condition differs, the
outcome differs, and the condition is controlling in both trials as
re-evaluated over the decision's recorded boolean structure. The report
states how many conditions were covered via masking.

**Evidence:** `mcdc.rs`.

### TOR-9 — Formal verification adapter

`openlustre prove` shall emit Kind 2-compatible Lustre + CoCoSpec (modes,
assumptions, guarantees; V6 merge-case syntax for clocks), invoke Kind 2 for
BMC/induction, realizability, and mode coverage, and parse proof results and
counterexample traces back to model terms. Constructs standard Lustre lacks
(bit operators, casts, float intrinsics) emit as conventionally-named
function calls the user supplies for proving.

**Evidence:** `kind2_adapter.rs`; emission syntax in
`numeric_cast_and_operations.rs`, `float_intrinsics.rs`, `bit_ops.rs`.

### TOR-10 — Imported C operators

Every imported C operator shall carry a manifest (typed signature,
assumptions/guarantees, purity/determinism/boundedness properties); the tool
shall validate the signature and generate the wrapper and compile/test
harness. Imported C without a contract is rejected — external code cannot
bypass the safety model.

**Evidence:** `imported_operator.rs`.

### TOR-11 — Editing integrity (Studio)

Every GUI edit shall go through journaled server endpoints: an edit either
applies fully and is undoable (100-deep journal, file snapshots), or is
rejected with a 4xx and the model files stay untouched. Multi-step edits
(paste, Lustre import) are single journal entries. Parsed user input never
reaches generated source verbatim (debug-driver literals are re-emitted from
parsed values).

**Evidence:** `studio_editing.rs`, `studio_server.rs`,
`studio_workspace_types.rs`, `library_state_machines_and_studio_api.rs`,
`multi_file_project.rs`, `industry_hardening.rs`.

### TOR-12 — Lustre import fidelity

`File ▸ Import Lustre` shall parse the supported dataflow subset
(`type`/`const`/`node`/`function`, `elem^len` arrays) all-or-nothing: name
clashes reject the whole import, unsupported constructs produce a located
error, and the subset the tool itself emits round-trips (an operator's own
`.lus` re-imports and rebuilds).

**Evidence:** parser unit tests in `crates/ol_cli/src/lustre_import.rs` and
the workspace import test in `studio_workspace_types.rs`.

### TOR-13 — FMU export equivalence

`openlustre fmu` shall export an operator as an FMI 2.0 co-simulation FMU
whose behavior, driven through the standard fmi2 API, is identical to the
IR simulator's cycle-for-cycle: one `fmi2DoStep` advances the Lustre
program by exactly one cycle regardless of the communication step size.
The archive shall be deterministic (an unchanged model re-exports
byte-identically, with a content-hashed GUID) and shall reject
compound-typed interfaces loudly rather than mis-declare them.

**Evidence:** `tests/fmu_export.rs` (archive validity, determinism, typed
model description, compound-port rejection, and the fmi2-driven trace
equality against `ol_sim`).

## 3. Usage constraints

1. **Independent output verification is mandatory** for certification-adjacent
   use: review the generated C as you would hand code; run `openlustre test
   run --backend both` with project scenarios achieving your required
   coverage (TOR-8 reports it); treat Kind 2 results as supporting evidence,
   not sole verification.
2. **Record goldens deliberately.** `test record` snapshots the IR simulator
   as the reference; a wrong model records a wrong golden. Goldens are
   review artifacts.
3. **Pin your toolchain.** Equivalence is demonstrated on the applicant's
   compiler/platform; changing either re-runs the evidence (`cargo test`,
   `openlustre test run`).
4. **Transcendental floats:** on one platform both backends agree; across
   platforms expect ulp-level differences (§TOR-6). Use the exactly-rounded
   subset in golden traces you intend to be portable.

## 4. Documented exclusions (loud, not silent)

- Float32-native intrinsics (`sqrtf` …): float32 casts through float64
  explicitly; the typechecker refuses the implicit path (`E0161`).
- Stateful/clocked array iteration; Kind 2 proving of iterator bodies.
- Qualification credit of any kind (see the preamble).

## 5. Maintaining this document

A change that adds a checked rule, a generated-code property, or a new
backend agreement point must (a) land with tests in the files named above —
"everything ships across all stages or it isn't done" — and (b) extend the
relevant TOR here. The §6 log in `SCADE_GAP_ANALYSIS.md` records each slice;
this document records the *claims*.
