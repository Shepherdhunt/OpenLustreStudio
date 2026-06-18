# OpenLustre Studio — Tool Operational Requirements (TOR)

*Document TOR-OLS-001. Revision 2026-06-18. Status: living document, tracks `main`.*

## Preface — what this document is, and is not

This is a **Tool Operational Requirements** document in the spirit of DO-330
(*Software Tool Qualification Considerations*). It enumerates, as numbered
requirements, every operation OpenLustre Studio claims to perform, and binds each
requirement to the **verification evidence** that demonstrates it — overwhelmingly
the automated test suite, which is run on every change.

It is **not** a qualification certificate and does not confer one. OpenLustre
Studio is not a DO-330-qualified tool. The honest strategy of the project (see
`SCADE_GAP_ANALYSIS.md` §4) is **verification-by-equivalence**: rather than
inheriting verification credit from a qualified code generator, the applicant
independently verifies the generated code against the model. This document is the
fourth and final pillar of that strategy:

1. **Dual-backend execution** — every scenario runs on the IR simulator *and* on
   the compiled generated C, compared cell-by-cell.
2. **Formal contract proofs** — Kind 2 proves the model's contracts; runtime
   monitors compile the same contracts into the C.
3. **Coverage evidence** — decision coverage *and* unique-cause MC/DC on the IR
   backend, reported per condition.
4. **Tool Operational Requirements** *(this document)* — a written statement of
   what the tool does, with the test suite as the verification cases.

### How to read a requirement

Each requirement has an identifier (`TOR-nnn`), a statement of intent, a DO-330
**verification method** — **T** (Test), **A** (Analysis), **R** (Review/Inspection)
— and **Evidence**: the test file (and, where stable, the test function) or source
location that demonstrates it. Test paths are relative to the repository root.

### Verification baseline

As of revision date, `cargo test --workspace --no-fail-fast` on Windows 11 /
MSVC reports **214 passing tests, 0 failed, 0 ignored, across 57 result groups**
(integration tests, per-crate unit tests, and doctests). The dual-backend
equivalence tests that require a C compiler are gated on its presence and run in
CI and on this developer machine (MSVC discovered via `vswhere` + `vcvars64.bat`);
where a test is so gated it is noted.

---

## TOR-0xx — Tool identification and operational environment

| ID | Requirement | Method | Evidence |
|---|---|---|---|
| TOR-001 | The tool SHALL be a single self-contained executable (`openlustre`) exposing a CLI and an embedded-HTML Studio GUI (`openlustre studio serve\|launch <dir>`). | R | `crates/ol_cli/src/main.rs`, `studio_server.rs`, `studio_ui.html` |
| TOR-002 | The tool SHALL operate on Windows 11 (MSVC toolchain) as a first-class build/test platform, discovering the C compiler via `vswhere`/`vcvars64.bat` when no `cc`/`gcc`/`clang` is on `PATH`. | T | `tests/packaging_launch.rs`; `scenario.rs` compiler discovery |
| TOR-003 | The tool's behavior SHALL be deterministic for a given model and input vector across both execution backends (no reliance on hash-map iteration order, wall-clock, or undefined evaluation order). | T,A | `tests/evaluation_order.rs`; `tests/trace_comparison.rs` |
| TOR-004 | The tool SHALL never emit text to a server-reachable stdout/stderr path (a broken pipe must not drop a request). | A,R | `studio_server.rs` (zero `print!`/`println!`); `resolve_workspace` is silent |
| TOR-005 | The generated C SHALL be free of constructs that depend on the host's integer width or endianness for the model's declared types (fixed-width `intN_t`/`uintN_t`, explicit casts). | T | `tests/numeric_cast_and_operations.rs`; `tests/trace_comparison.rs` |

---

## TOR-1xx — Model management and authoring

| ID | Requirement | Method | Evidence |
|---|---|---|---|
| TOR-101 | The tool SHALL load a model from a single file, a directory, or a `.wksc` workspace, merging packages reachable through an `includes` list, and SHALL detect cyclic includes. | T | `tests/multi_file_project.rs::{loader_follows_an_includes_list_and_merges_packages, loader_treats_a_directory_as_a_merged_project, cyclic_includes_are_detected}` |
| TOR-102 | The tool SHALL create, open, save, and switch the active workspace at runtime without leaking state between workspaces, and SHALL autosave on every successful build. | T | `tests/studio_workspace_types.rs::workspace_new_open_save_switches_the_active_workspace` |
| TOR-103 | The tool SHALL author operators graphically: drag a palette block or library node onto the canvas to place a call equation with fresh typed result locals and red unbound input pins. | T | `tests/studio_editing.rs`; `tests/studio_workspace_types.rs` |
| TOR-104 | The tool SHALL edit and delete model elements in place (rename a variable rewriting all uses, retype, change role input/output/local, delete; edit/delete an equation), and SHALL bind operand pins by wiring or by expression. | T | `tests/studio_editing.rs` |
| TOR-105 | Every model-mutating operation SHALL be journaled and undoable/redoable; a new edit SHALL clear the redo branch; undo/redo of empty stacks SHALL report rather than corrupt state. | T | `tests/industry_hardening.rs` |
| TOR-106 | The tool SHALL persist free-form diagram layout (positions, box sizes, grid pitch, wrap flags) in the model file and round-trip it. | T | `tests/critical_items.rs::layout_positions_persist_to_the_model_file_and_round_trip` |
| TOR-107 | The tool SHALL define named types (enum, record/struct, array alias) and project-wide constants, validate their uniqueness against everything loaded, and make them usable in operators. | T | `tests/typecheck_records_enums.rs`; `tests/constants.rs`; `tests/studio_workspace_types.rs` |
| TOR-108 | The tool SHALL support composite constant *values* — array, record, and `char`/string literals — end to end (parse, typecheck, simulate, emit). | T | `tests/composite_constants.rs`; `tests/studio_workspace_types.rs::project_composite_constants_array_and_char` |
| TOR-109 | The tool SHALL import existing Lustre (`type`/`const`/`node`/`function`) into the project, rejecting name clashes against the project or stdlib all-or-nothing, and round-tripping its own emitted Lustre. | T | `tests/studio_workspace_types.rs::import_lustre_adds_nodes_types_constants`; `lustre_import.rs` unit tests |
| TOR-110 | The tool SHALL import a C operator as an opaque node with a declared signature, callable from the model and linked into generated C. | T | `tests/imported_operator.rs` |
| TOR-111 | The tool SHALL copy/cut/paste operation blocks, cloning equations with fresh result-local names and rewiring references *within* the copied set while leaving external references to resolve or surface as unbound. | T | `tests/numeric_cast_and_operations.rs::paste_clones_equations_and_rewires_internal_references` |

---

## TOR-2xx — Static verification (the model checker)

The type checker, clock calculus, and contract checker run on the model before any
simulation or code generation. Each diagnostic carries a stable code and a
`node X · equation N` context that pins it to its diagram box.

| ID | Requirement | Method | Evidence |
|---|---|---|---|
| TOR-201 | The tool SHALL reject structurally invalid models: duplicate node/constant names (E0001/E0003), duplicate or shadowing ports (E0010–E0012), equations defining unknown names (E0020), and outputs never assigned (E0050). | T | `tests/typecheck_rules.rs`; `tests/diagram_validity.rs` |
| TOR-202 | The tool SHALL type-check every expression: unknown identifiers (E0080), operator operand types (E0081–E0092), `if`-condition is bool (E0090), and assignment type compatibility. | T | `tests/typecheck_rules.rs`; `tests/typecheck_records_enums.rs` |
| TOR-203 | The tool SHALL type-check node calls: unknown callee (E0100), arity (E0101), and per-argument types (E0102–E0103). | T | `tests/typecheck_rules.rs`; `tests/stdlib_subnode_calls.rs` |
| TOR-204 | The tool SHALL type-check composite construction/projection: array literals (E0123 empty-needs-annotation, E0124 heterogeneous), record construction (E0125–E0128). | T | `tests/composite_constants.rs`; `tests/typecheck_records_enums.rs` |
| TOR-205 | The tool SHALL enforce a clock calculus: mixed-clock operands, non-bool clocks, clocked outputs, clocked equations in functions, and clocked arrays are diagnosed (E0130–E0135). | T | `tests/clocks.rs` |
| TOR-206 | The tool SHALL type-check array iterators (`map`/`fold`): unknown/stateful/many-output iterated function, non-array operands, unequal lengths, argument-type and arity mismatch (E0140–E0146). | T | `tests/iterators.rs` |
| TOR-207 | The tool SHALL type-check the numeric-cast operator (numeric→numeric only, E0093/E0094) and float intrinsics (`float64` operands and result only — arity E0160, type E0161). | T | `tests/numeric_cast_and_operations.rs` |
| TOR-208 | The tool SHALL type-check log-message probes (the referenced variable must exist, E0150). | T | `tests/numeric_cast_and_operations.rs` / `tests/studio_editing.rs` (probe path) |
| TOR-209 | The contract checker SHALL detect vacuity, unreachability, and overlap in assume/guarantee contracts and report them in the GUI. | T | `tests/contract_check_polish.rs` |
| TOR-210 | The build/validity check for an operator SHALL be scoped to that operator's dependency *slice*, so an unrelated broken operator does not block it; setting a build target SHALL re-root the slice. | T | `tests/selective_codegen.rs`; `tests/studio_workspace_types.rs` |

---

## TOR-3xx — Simulation

| ID | Requirement | Method | Evidence |
|---|---|---|---|
| TOR-301 | The IR simulator SHALL execute the model cycle-by-cycle in dependency (data-flow) order — same-cycle reads resolved, `pre` excluded, both `->` arms counted — and SHALL report a true combinational cycle as an error rather than reading a stale default. | T | `tests/evaluation_order.rs` |
| TOR-302 | The simulator SHALL implement C-equivalent numeric semantics: two's-complement narrowing, float→int truncation, float32 rounding, and `<math.h>`-matching float intrinsic evaluation in `f64`. | T | `tests/numeric_cast_and_operations.rs`; `tests/trace_comparison.rs` |
| TOR-303 | The simulator SHALL implement clock semantics: a clocked equation runs only on its clock's active cycles and holds its last value otherwise; a clocked `->` takes its initial value on the first active tick of *its* clock. | T | `tests/clocks.rs`; `tests/trace_comparison.rs` |
| TOR-304 | The simulator SHALL evaluate state machines (flat, hierarchical/nested, and refined), including restart-on-entry, freeze-while-inactive, and history. | T | `tests/state_machines.rs` (`hierarchical_*`, `refine_*`, `operator_owned_machine_merges_into_the_operator_and_simulates`) |
| TOR-305 | The simulator SHALL read and write interface arrays at the boundary as `[e0;e1;…]`, so array-interface nodes are testable. | T | `tests/iterators.rs`; `tests/trace_comparison.rs` |
| TOR-306 | The Studio watch/set table SHALL validate every set value against its type (a bool only true/false; an `int8` within −128…127; an unsigned never negative) before the simulator sees it. | T,R | `studio_ui.html` `validateValue`; `tests/studio_editing.rs` |

---

## TOR-4xx — Code generation

| ID | Requirement | Method | Evidence |
|---|---|---|---|
| TOR-401 | The tool SHALL emit Lustre for the selected root operator and its dependencies, round-tripping its own surface syntax. | T | `tests/release_logic_pipeline.rs`; `lustre_import.rs` round-trip |
| TOR-402 | The tool SHALL emit C-Lite for the selected root: a `_step` function, state struct, CSV driver, and Makefile, walking equations in evaluation order. | T | `tests/scenario_harness.rs`; `tests/selective_codegen.rs` |
| TOR-403 | Code generation SHALL slice to the selected root, keeping every reachable node — including iterator function references and called nodes — and dropping the rest. | T | `tests/selective_codegen.rs`; `tests/iterators.rs` (slice regression) |
| TOR-404 | Generated C identifiers SHALL be safe: a model name colliding with a C keyword or a generated parameter name (`in`/`out`/`self`) SHALL be mangled at every emission site, while CSV headers keep the model's own names. | T | `tests/industry_hardening.rs` |
| TOR-405 | The tool SHALL compile contract monitors into the generated C so contract violations are observable at runtime. | T | `tests/runtime_monitor.rs` |
| TOR-406 | The tool SHALL emit log-message probes as guarded `printf`s under `#ifdef OL_DEBUG`, absent from production C and from the equivalence tests. | T | `tests/numeric_cast_and_operations.rs`; debug-driver path |
| TOR-407 | The tool SHALL lower float intrinsics to the double `<math.h>` functions, add `#include <math.h>`, and link `-lm` on every compile path. | T | `tests/numeric_cast_and_operations.rs` (generated-C string check + equivalence) |
| TOR-408 | The tool SHALL lower state machines (including nested regions) to flat data-flow C, emitting one state enum per region. | T | `tests/state_machine_codegen.rs` |
| TOR-409 | The tool SHALL compile the generated sources from the GUI to a chosen directory with an auto-detected or selected compiler. | T | `tests/scenario_harness.rs`; `scenario::compile_in_dir` |
| TOR-410 | The tool SHALL generate target-tuned build files (a toolchain/flag-specific `Makefile` and an `INTEGRATION.md`) for a selected target OS/board (host, embedded Linux-ARM, VxWorks, bare-metal-ARM); the host target compiles locally, a cross target is emitted for its own toolchain and built on the target. | T | `crates/ol_cli/src/target.rs` unit tests (`makefile_uses_the_target_toolchain`, `integration_readme_*`); `/api/targets`, `/api/clite/compile` |

---

## TOR-5xx — Formal verification

| ID | Requirement | Method | Evidence |
|---|---|---|---|
| TOR-501 | The tool SHALL emit a Kind 2 view of the model (with CoCoSpec contracts) and pass timeout/property selections into the invocation. | T | `tests/kind2_adapter.rs::{timeout_and_property_selection_flow_into_kind2_invocation, defaults_do_not_emit_timeout_or_properties_args}` |
| TOR-502 | The tool SHALL render a Kind 2 JSON counterexample into a cycle-indexed waveform table, including multi-scope counterexamples, and SHALL fail safe on a malformed counterexample. | T | `tests/kind2_adapter.rs::{waveform_renders_a_kind2_counterexample, waveform_renders_multi_scope_counterexamples_into_one_table, waveform_returns_none_for_a_non_array_counterexample}` |
| TOR-503 | The Kind 2 view SHALL express constructs unbounded-integer Lustre cannot (casts, bit ops, intrinsics) as same-named user-suppliable functions, and clocks as V6 merge-case syntax. | T,R | `crates/ol_kind2`; `tests/clocks.rs` (V6 merge) |

---

## TOR-6xx — Test, coverage, and equivalence (the verification-by-equivalence core)

These requirements are the heart of the project's strategy: they are what lets an
applicant treat the generated C as *independently verified* rather than
*qualified-by-pedigree*.

| ID | Requirement | Method | Evidence |
|---|---|---|---|
| TOR-601 | The scenario harness SHALL run golden-trace scenarios against the IR simulator. | T | `tests/scenario_harness.rs` |
| TOR-602 | The scenario harness SHALL run the *same* scenarios against the compiled generated C and compare the two traces **cell-by-cell**; any divergence SHALL fail. | T (CI/MSVC-gated) | `tests/trace_comparison.rs`; `tests/scenario_harness.rs` |
| TOR-603 | Dual-backend equivalence SHALL be demonstrated across the language's hard cases: numeric casts, forward-reference evaluation order, `map`/`fold` over arrays, boolean clocks (gating patterns), state-machine region enums, and hostile (keyword-colliding) identifiers. | T (CI/MSVC-gated) | `tests/trace_comparison.rs`; `tests/numeric_cast_and_operations.rs`; `tests/iterators.rs`; `tests/clocks.rs`; `tests/state_machine_codegen.rs`; `tests/industry_hardening.rs` |
| TOR-604 | The harness SHALL measure decision coverage of the model (if-conditions and compound boolean equation RHS) and report uncovered decisions. | T | `tests/mcdc.rs` |
| TOR-605 | The harness SHALL measure **unique-cause MC/DC** (DO-178C Level A), capturing each atomic condition's value in a single eval pass and reporting per condition which still lacks an isolating test pair. | T | `tests/mcdc.rs` |
| TOR-606 | Equivalence comparisons SHALL guard against passing on identically-wrong values: golden-content checks pin the expected trace text, not merely backend agreement. | T | `tests/evaluation_order.rs` (golden-content regression); `tests/trace_comparison.rs` |

---

## TOR-7xx — Deployment and integrity

| ID | Requirement | Method | Evidence |
|---|---|---|---|
| TOR-701 | The tool SHALL build a Windows installer (Inno Setup) with Start-Menu/Desktop shortcuts and the embedded standard library, and a Linux install script. | T,R | `tests/packaging_launch.rs`; `packaging/windows/build-installer.ps1` |
| TOR-702 | The embedded standard library SHALL be loaded and merged identically whether installed or run from source. | T | `tests/stdlib_library.rs`; `tests/library_state_machines_and_studio_api.rs` |

---

## 8. Operational constraints and known limitations

These are **conscious v1 limits**, each enforced loudly (a diagnostic or a hard
error), never silent. They bound the tool's claims above.

| Limitation | Enforcement | Tracking |
|---|---|---|
| Float intrinsics are `float64`-only (`float32` needs `sqrtf`/… with matching f32 sim rounding). | E0161 | gap doc §0/§6 |
| Clocks: stateful calls finer than their equation's clock (E0133) and clocked arrays (E0135) are rejected. | E0133/E0135 | gap doc §6 |
| Iterators: the iterated function must be stateless and the iterator the whole RHS; no clocked iteration; Kind 2 iterator *proving* is roadmap. | E0140–E0146 | gap doc §3 |
| MC/DC is unique-cause only; masking MC/DC for coupled conditions is reported as uncovered. | reported uncovered | gap doc §4 |
| `char` is non-numeric and unsupported at the CSV I/O boundary (constant/internal value only). | typecheck reject / parse Err | gap doc §6 |
| Inline composite literals in an equation RHS are not whole-assigned in C; the supported path is a named constant + index/field. | emitter placeholder | gap doc §6 |
| `Diagnostic.span` (file:line:col) is not populated; diagnostics are pinned to diagram boxes via `node X · equation N` context instead. | n/a | gap doc §3 (P2) |
| The tool is **not** DO-330 qualified; generated code requires independent verification (this document supports that path, it does not replace it). | n/a | gap doc §4 |

---

## 9. Verification summary

| Metric | Value |
|---|---|
| Automated tests | **214 passing, 0 failed, 0 ignored** |
| Result groups | 57 (integration + unit + doctests) |
| Integration test files | 33 |
| Platform of record | Windows 11 / MSVC |
| Dual-backend equivalence | Demonstrated (CI + developer MSVC); cell-by-cell |
| Coverage metrics | Decision coverage + unique-cause MC/DC |

Every requirement above is `Test`-verified unless marked `A` (Analysis) or `R`
(Review). The verification cases are the test suite itself; re-running
`cargo test --workspace --no-fail-fast` re-executes them. A requirement is
considered *unmet* the moment its evidence test fails — there is no separate,
drifting verification record to maintain.

---

*Maintenance: when a feature lands across the stack (IR → typecheck → sim →
generated C → equivalence), add or amend the TOR-nnn requirement and cite the new
test in the same commit. This document and `SCADE_GAP_ANALYSIS.md` §6 are updated
together.*
