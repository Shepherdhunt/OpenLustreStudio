//! OpenLustre Studio: strict dataflow IR.
//!
//! This crate defines the canonical, language-neutral representation of an
//! OpenLustre model. Every downstream tool (type checker, contract checker,
//! Lustre emitter, C-Lite emitter, simulator, Kind 2 adapter) operates on this
//! IR and nothing else. The IR is intentionally a conservative subset of
//! Lustre with the following rules:
//!
//! * No higher-order operators.
//! * No anonymous nodes.
//! * Boolean clocks only: `e when c` / `e when not c` / `merge(c, a, b)`
//!   with variable-name conditions (see [`clocks`]).
//! * Arrays are fixed-size and statically typed.
//! * Records are nominal and declared.
//! * `pre` always has an initial value supplied via `->`.

pub mod types;
pub mod expr;
pub mod node;
pub mod order;
pub mod project;
pub mod slice;
pub mod state_machine;
pub mod mono;
pub mod diag;
pub mod loader;
pub mod clocks;

pub use clocks::{infer_clocks, node_uses_clocks, Clock, ClockError, ClockInfo};
pub use diag::{Diagnostic, Severity, SourceSpan};
pub use expr::{BinOp, Expr, FieldInit, FloatFn, IterKind, Literal, UnaryOp};
pub use node::{DiagramLayout, Equation, Local, NodeDef, NodeKind, NodePos, Port, Probe};
pub use order::evaluation_order;
pub use project::{
    ConstDef, EnumDef, GenericInst, GenericNode, Package, Project, RecordField, TypeArg, TypeBody,
    TypeDef,
};
pub use mono::MonoError;
pub use slice::slice_for_root;
pub use state_machine::{
    lower as lower_state_machine, resolve_refines, signal_present_local, signal_value_local, Emit,
    LoweredMachine, Region, SignalDef, StateDef, StateMachineDef, Transition,
};
pub use types::Type;
pub use loader::{load_project, LoadError};
