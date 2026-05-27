```text
Graphical Lustre/CoCoSpec IDE
  -> strict model IR
  -> CoCoSpec contract IR
  -> Lustre + CoCoSpec export
  -> Directional C-Lite generation
  -> simulation, proof, realizability, and trace evidence
```

CoCoSpec matters because it gives you a mode-aware assume/guarantee contract language over Lustre. The original CoCoSpec work was designed specifically for embedded/reactive systems and lets engineers express mode behavior directly instead of hiding modes inside ordinary implication guarantees. ([Kind][1]) Kind 2’s contract model is explicitly a triple of **assumptions, guarantees, and modes**, and its modes are built from `require` and `ensure` clauses. ([Kind][2])

## Project concept

Working name:

```text
OpenLustre Studio
```

Better description:

> **An open-source graphical Lustre and CoCoSpec modeling IDE for safety-critical embedded software, with SCADE-like typed blocks, strict semantic checking, imported C operators, Directional C-Lite generation, simulation, and Kind 2 verification.**

The key principle:

```text
The model is not just equations.
The model is equations + contracts + modes + evidence.
```

## Top-level architecture

```text
Graphical IDE
   |
   v
Strict Model IR
   |
   +--> Dataflow IR
   +--> State Machine IR
   +--> Type IR
   +--> Imported Operator IR
   +--> Contract IR / CoCoSpec IR
   |
   +--> Lustre + CoCoSpec emitter
   +--> Directional C-Lite emitter
   +--> Simulator
   +--> Kind 2 verification adapter
   +--> Contract realizability checker adapter
   +--> Test vector generator
   +--> Trace/evidence report generator
```

The big change from the previous plan is the addition of a **first-class Contract IR**.

## Core semantic layers

### 1. Dataflow layer

This is normal Lustre-style synchronous logic:

```text
Inputs
Outputs
Locals
Equations
pre / init
node calls
function calls
typed wires
fixed-size arrays
records
enums
```

This layer answers:

> “What does the software do each cycle?”

### 2. CoCoSpec contract layer

This answers:

> “What must be true for the component to be used legally, and what does it guarantee in each mode?”

Contract elements:

```text
assume
guarantee
mode
require
ensure
ghost variables
contract imports
realizability metadata
non-vacuity checks
```

Kind 2 distinguishes assumptions from guarantees: assumptions constrain legal use of a node and cannot depend on current outputs, while guarantees describe node behavior and may mention current outputs. ([Kind][2])

### 3. Mode semantics layer

This is the CoCoSpec-specific addition.

A mode is not just a state-machine state. A mode is a **contract case**:

```text
mode ArmedRelease
  require master_arm = true
  require consent = true
  require fault_present = false
  ensure release_cmd => station_selected
```

Kind 2 treats a mode as a situation/reaction implication: if the mode’s `require` conditions hold, then its `ensure` conditions must hold. ([Kind][3])

### 4. Implementation layer

This is the executable model:

```text
Lustre equations
state machines
imported operators
generated C-Lite
```

This answers:

> “Does the implementation refine the contract?”

### 5. Evidence layer

Outputs:

```text
type check report
clock/rate check report
contract well-formedness report
mode exhaustiveness report
non-vacuity report
Kind 2 proof results
counterexample traces
realizability results
C-Lite trace comparison
imported C operator contract report
```

Kind 2 can check contract realizability; when a contract is unrealizable, it can provide a deadlocking computation and conflicting guarantees, which is exactly the kind of feedback this IDE should surface graphically. ([arXiv][4])

## Updated internal IR

The IR should be split cleanly.

```rust
Project
  packages: Vec<Package>

Package
  types: Vec<TypeDef>
  constants: Vec<ConstDef>
  nodes: Vec<NodeDef>
  contracts: Vec<ContractDef>
  imported_operators: Vec<ImportedOperatorDef>

NodeDef
  name: String
  kind: Function | Operator | StateMachine | Imported
  inputs: Vec<Port>
  outputs: Vec<Port>
  locals: Vec<Local>
  equations: Vec<Equation>
  contract: Option<ContractRef>
  diagram: DiagramLayout

ContractDef
  name: String
  inputs: Vec<Port>
  outputs: Vec<Port>
  ghost_vars: Vec<GhostVar>
  assumptions: Vec<Assumption>
  guarantees: Vec<Guarantee>
  modes: Vec<Mode>
  imports: Vec<ContractImport>

Mode
  name: String
  requires: Vec<Expr>
  ensures: Vec<Expr>
```

The GUI should let the user edit both:

```text
Behavior View     -> Lustre/dataflow equations
Contract View     -> CoCoSpec assume/guarantee/mode clauses
```

That is a major differentiator from a simple block editor.

## SCADE-like concepts

### Function

Stateless, mathematical, no temporal behavior.

Rules:

```text
No pre
No init arrow
No retained state
No node calls
Only function calls
Same inputs always produce same outputs
```

Kind 2’s `function` semantics are stateless and disallow temporal operators such as `pre`, `->`, `merge`, `when`, `condact`, and `activate`. ([Kind][5])

### Operator / Node

Stateful synchronous component.

Allowed:

```text
pre
init
state variables
state-machine lowering
node calls
temporal logic
contracts
modes
```

Generated C-Lite shape:

```c
typedef struct {
    bool initialized;
    bool prev_release_request;
} ReleaseLogic_State;

void ReleaseLogic_init(ReleaseLogic_State* self);

void ReleaseLogic_step(
    ReleaseLogic_State* self,
    const ReleaseLogic_Input* in,
    ReleaseLogic_Output* out
);
```

### Imported operator

A typed visual block backed by external C.

Manifest:

```yaml
name: CRC16_CCITT
kind: imported_operator
language: c
header: crc16.h
source: crc16.c
symbol: crc16_ccitt

inputs:
  - name: data
    type: uint8[32]
  - name: length
    type: uint32

outputs:
  - name: crc
    type: uint16

contract:
  assumptions:
    - length <= 32
  guarantees:
    - crc_is_deterministic
  properties:
    pure: true
    deterministic: true
    no_dynamic_memory: true
    no_global_write: true
    bounded_execution: true
```

The IDE should require every imported C operator to have a contract. Otherwise imported C becomes a backdoor around the safety model.

## CoCoSpec-aware example

A simple release interlock should not only have behavior:

```lustre
node ReleaseLogic(
  master_arm: bool;
  station_selected: bool;
  consent: bool;
  fault_present: bool;
  release_request: bool
) returns (
  release_cmd: bool;
  inhibit: bool
);
let
  release_cmd =
    master_arm and station_selected and consent and
    not fault_present and release_request;

  inhibit = release_request and not release_cmd;
tel
```

It should also emit a contract:

```lustre
node ReleaseLogic(
  master_arm: bool;
  station_selected: bool;
  consent: bool;
  fault_present: bool;
  release_request: bool
) returns (
  release_cmd: bool;
  inhibit: bool
);
con
  guarantee release_cmd => master_arm;
  guarantee release_cmd => station_selected;
  guarantee release_cmd => consent;
  guarantee release_cmd => not fault_present;

  mode SafeInhibit (
    require release_request;
    require fault_present;
    ensure not release_cmd;
    ensure inhibit;
  );

  mode AuthorizedRelease (
    require release_request;
    require master_arm;
    require station_selected;
    require consent;
    require not fault_present;
    ensure release_cmd;
    ensure not inhibit;
  );

  mode Idle (
    require not release_request;
    ensure not release_cmd;
  );
noc
let
  release_cmd =
    master_arm and station_selected and consent and
    not fault_present and release_request;

  inhibit = release_request and not release_cmd;
tel
```

That is the style you want: behavior and mode-aware requirements live together.

## Revised implementation phases

### Phase 0 — Define the OpenLustre semantic profile

Create:

```text
OpenLustre Profile 0.1
```

It should define:

```text
Allowed Lustre subset
Allowed CoCoSpec subset
Type rules
Clock/rate rules
Initialization rules
Function/operator distinction
Imported operator rules
C-Lite generation rules
Contract semantics rules
```

Start conservative. The goal is not full Lustre. The goal is a safe, analyzable, SCADE-like subset.

### Phase 1 — Build compiler core before GUI

Repository:

```text
openlustre-studio/
  crates/
    ol_ir/
    ol_contract_ir/
    ol_typecheck/
    ol_clockcheck/
    ol_contract_check/
    ol_lustre_emit/
    ol_cocospec_emit/
    ol_clite_emit/
    ol_sim/
    ol_kind2/
    ol_cli/
  apps/
    studio_ui/
  libraries/
    core/
    math/
    temporal/
    arrays/
    records/
    bits/
    state_machines/
    avionics/
  examples/
  tests/
  docs/
```

Build the CLI first:

```bash
openlustre check model.ols
openlustre emit-lustre model.ols -o generated/model.lus
openlustre emit-clite model.ols -o generated/clite/
openlustre simulate model.ols --inputs tests/input.csv
openlustre prove model.ols
openlustre contract-check model.ols
```

### Phase 2 — Type checker

Checks:

```text
All wires are type-compatible
All node calls match signatures
No implicit narrowing
No output assigned twice
No missing output assignment
No illegal temporal operators in functions
No illegal imported operator use
No uninitialized pre
No combinational cycle without temporal break
```

### Phase 3 — Contract checker

This is now a major subsystem.

Checks:

```text
Assumptions do not depend on current outputs
Guarantees are Boolean streams
Mode requires are Boolean streams
Mode ensures are Boolean streams
Ghost variables are contract-local
Contract imports map inputs/outputs correctly
Every public operator has at least a minimal contract
Imported C operators have declared assumptions/guarantees
```

Also add warnings:

```text
Mode may be unreachable
Mode set may be non-exhaustive
Guarantee may be vacuous
Assumption may be too strong
Contract may be unrealizable
Implementation may not refine contract
```

Kind 2 performs defensive mode checks: when contracts have modes, it checks that modes account for all situations allowed by assumptions, and can provide a counterexample when modes are not exhaustive. ([Kind][3])

### Phase 4 — Lustre + CoCoSpec emitter

Generate:

```text
model.lus
contracts.lus
top_observers.lus
kind2_config.json
```

Support two contract syntax targets:

```text
Target A: modern Kind 2 con/noc contract syntax
Target B: legacy block-comment contract syntax
```

That avoids tying the IDE to one parser version.

### Phase 5 — Directional C-Lite emitter

Generate C-Lite from the same IR.

Rules:

```text
No malloc/free
No recursion
No function pointers
No pointer arithmetic
No hidden globals
No implicit casts
No dynamic arrays
Fixed-width integer types only
Explicit state structs
Explicit init/step
Explicit input/output structs
```

Contract handling in C-Lite:

```text
assume      -> input precondition comments / optional runtime checks
guarantee   -> generated assertion checks in test harness
mode require/ensure -> generated mode trace monitors
ghost vars  -> test/proof-only monitor variables
```

Do not pollute production C-Lite with proof-only logic by default. Generate it into a monitor harness:

```text
generated/
  clite/
    model.c
    model.h
  monitors/
    release_logic_contract_monitor.c
    release_logic_contract_monitor.h
  tests/
    trace_compare.c
```

### Phase 6 — Simulator

The simulator should run both:

```text
IR interpreter
Generated C-Lite executable
```

Then compare traces:

```text
input.csv
expected_output.csv
actual_ir_output.csv
actual_clite_output.csv
contract_monitor_output.csv
```

The simulator should report:

```text
Cycle 17:
  release_cmd differs
  IR: false
  C-Lite: true
  violated guarantee: release_cmd => not fault_present
  active mode: SafeInhibit
```

That kind of feedback is gold.

### Phase 7 — Kind 2 integration

Commands:

```bash
openlustre prove model.ols
openlustre prove --node ReleaseLogic
openlustre prove --contract ReleaseLogic
openlustre prove --realizability ReleaseLogic
openlustre prove --mode-coverage ReleaseLogic
```

Kind 2 is an SMT-based model checker for safety properties over synchronous reactive systems written in an extension of Lustre, and it can return counterexample traces when properties fail. ([Kind2 MC][6])

IDE features:

```text
Show proven properties
Show failed properties
Show counterexample as waveform
Highlight failing block
Highlight failing contract clause
Show active mode per cycle
Show conflicting guarantees
Show deadlocking contract trace
```

### Phase 8 — Visual editor

The GUI should have these panes:

```text
Project Explorer
Block Diagram
Contract Editor
Mode Table
Generated Lustre
Generated C-Lite
Simulation Trace
Proof Results
Counterexample Viewer
```

For each operator, the user should see:

```text
Interface
Behavior
Contract
Modes
Tests
Evidence
Generated artifacts
```

The contract editor should allow both structured editing and raw text:

```text
Structured mode:
  Add assumption
  Add guarantee
  Add mode
  Add require
  Add ensure

Advanced mode:
  Edit CoCoSpec text directly
```

### Phase 9 — Built-in libraries

Each built-in block should include behavior plus contract.

Example block metadata:

```yaml
name: RisingEdge
kind: operator
inputs:
  - x: bool
outputs:
  - edge: bool

contract:
  guarantees:
    - edge => x
    - edge => not (false -> pre x)
  modes:
    - name: FirstCycle
      require: first_cycle
      ensure: not edge
    - name: Rising
      require: x and not pre_x
      ensure: edge
    - name: NotRising
      require: not (x and not pre_x)
      ensure: not edge
```

Library categories:

```text
Core logic
Math
Comparison
Condition blocks
Temporal blocks
Arrays
Records/structures
Bit manipulation
State machines
Avionics protocol/message blocks
Safety monitor blocks
Contract/observer blocks
```

## Minimum viable product

The MVP should prove the whole concept with a serious but small vertical slice.

MVP features:

```text
1. Strict IR
2. Function/operator distinction
3. Typed graphical blocks
4. Core math/logic blocks
5. Temporal blocks: pre, delay, latch, edge
6. CoCoSpec contract editor
7. Mode-aware contracts
8. Lustre + CoCoSpec export
9. C-Lite generation
10. IR simulation
11. C-Lite trace comparison
12. Kind 2 proof execution
13. Counterexample display
14. Imported C operator manifest
```

MVP demo:

```text
Release Authorization Component
  - master arm
  - consent
  - station select
  - fault inhibit
  - release request
  - edge detect
  - timeout
  - inhibit reason
  - CoCoSpec modes
  - Kind 2 proof
  - generated C-Lite
  - trace comparison
```

## Codex-ready task list

```text
Task 1: Create Rust workspace
- ol_ir
- ol_contract_ir
- ol_typecheck
- ol_contract_check
- ol_lustre_emit
- ol_clite_emit
- ol_sim
- ol_kind2
- ol_cli

Task 2: Define core dataflow IR
- Project
- Package
- TypeDef
- NodeDef
- Port
- Local
- Equation
- Expr
- BlockInstance
- Wire
- DiagramLayout

Task 3: Define CoCoSpec Contract IR
- ContractDef
- Assumption
- Guarantee
- Mode
- Require
- Ensure
- GhostVar
- ContractImport
- ContractRef

Task 4: Implement primitive type system
- bool
- int8/int16/int32/int64
- uint8/uint16/uint32/uint64
- float32/float64
- fixed arrays
- records
- enums

Task 5: Implement dataflow type checker
- port compatibility
- expression inference
- no implicit narrowing
- output assignment checks
- stateless function restrictions
- temporal operator restrictions

Task 6: Implement contract checker
- assumptions cannot depend on current outputs
- guarantee expressions must be Boolean
- mode require/ensure expressions must be Boolean
- ghost variables are contract-local
- contract imports are signature-compatible
- warn on missing public contracts

Task 7: Implement dependency checker
- detect combinational cycles
- allow cycles only through explicit temporal operators
- emit user-friendly diagnostics

Task 8: Implement Lustre emitter
- node signatures
- functions
- operators
- locals
- equations
- if/then/else
- pre/init
- arrays/records/enums

Task 9: Implement CoCoSpec emitter
- emit assumptions
- emit guarantees
- emit modes
- emit require/ensure
- emit ghost vars
- emit contract imports
- support con/noc and legacy block-comment syntax

Task 10: Implement C-Lite emitter
- pure functions
- stateful init/step operators
- fixed-width types
- input/output structs
- state structs
- no dynamic memory
- no pointer arithmetic
- generated compile tests

Task 11: Implement contract monitor generation
- assumptions as precondition monitors
- guarantees as assertion monitors
- modes as active-mode monitors
- emit C test harness monitors

Task 12: Implement simulator
- execute IR cycle-by-cycle
- load CSV inputs
- emit CSV outputs
- compare against generated C-Lite traces
- report cycle-level divergences

Task 13: Implement Kind 2 adapter
- run Kind 2 on generated Lustre/CoCoSpec
- parse proof results
- parse counterexamples
- support main node selection
- support contract checks
- support realizability checks

Task 14: Implement imported C operator support
- YAML manifest parser
- typed signature validation
- purity/determinism contract fields
- wrapper generation
- compile harness
- test-vector harness

Task 15: Build initial block library
- And, Or, Not, Xor
- Mux, Switch, Compare
- Add, Subtract, Multiply, Divide
- Clamp, Saturate, Min, Max
- Pre, Delay, Latch
- RisingEdge, FallingEdge
- Counter, Timer
- Assert, Assume, Guarantee, Mode block

Task 16: Build ReactFlow/Tauri GUI
- project browser
- graphical node editor
- block palette
- typed ports
- wire validation
- contract editor
- mode table
- generated Lustre view
- generated C-Lite view
- simulation trace viewer
- Kind 2 result viewer

Task 17: Build MVP example
- Release Authorization Component
- CoCoSpec contract
- generated Lustre
- generated C-Lite
- simulation trace
- Kind 2 proof
- failed-proof counterexample demo
```

## The better final framing

The project is not merely an open-source SCADE clone.

It is more specific and more defensible:

> **A graphical Lustre and CoCoSpec workbench for building safety-critical synchronous software models, generating Directional C-Lite, and producing proof-oriented evidence before downstream SCADE, SWAN, or qualified code-generation workflows.**

That is the right lane. CoCoSpec should be treated as a primary semantic feature, because for avionics the contracts, modes, and assumptions are often as important as the equations themselves.

[1]: https://kind.cs.uiowa.edu/papers/CGKT%2B16.pdf?utm_source=chatgpt.com "CoCoSpec: A Mode-aware Contract Language for Reactive ..."
[2]: https://kind.cs.uiowa.edu/kind2_user_doc/2_input/1_lustre.html "Kind 2 Input"
[3]: https://kind.cs.uiowa.edu/kind2_user_docs/v2.0.0/9_other/2_contract_semantics.html "Contract Semantics"
[4]: https://arxiv.org/abs/2205.09082?utm_source=chatgpt.com "Realizability Checking of Contracts with Kind 2"
[5]: https://kind.cs.uiowa.edu/kind2_user_docs/v2.0.0/2_input/1_lustre.html "Lustre Input"
[6]: https://kind2-mc.github.io/kind2/?utm_source=chatgpt.com "Kind 2"
