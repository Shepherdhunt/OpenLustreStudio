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
//! * No clocks beyond the base clock (Phase 0 profile).
//! * Arrays are fixed-size and statically typed.
//! * Records are nominal and declared.
//! * `pre` always has an initial value supplied via `->`.

pub mod types;
pub mod expr;
pub mod node;
pub mod project;
pub mod diag;
pub mod loader;

pub use diag::{Diagnostic, Severity, SourceSpan};
pub use expr::{BinOp, Expr, Literal, UnaryOp};
pub use node::{Equation, Local, NodeDef, NodeKind, Port};
pub use project::{ConstDef, Package, Project, TypeDef, TypeBody, EnumDef, RecordField};
pub use types::Type;
pub use loader::load_project;
