use serde::{Deserialize, Serialize};

use crate::expr::Expr;
use crate::types::Type;

/// Function vs Operator distinguishes stateless math from stateful synchronous
/// components, matching SCADE and Kind 2 semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    /// Pure stateless function. No `pre`, no `->`, no node calls (only
    /// function calls), no retained state.
    Function,
    /// Stateful synchronous node — Lustre `node` semantics.
    Operator,
    /// Externally implemented in C. Body is empty; a contract must be supplied.
    Imported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Port {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Local {
    pub name: String,
    pub ty: Type,
}

/// Single-output or multi-output equation. The simulator and emitters use
/// the same shape — multi-output equations bind a tuple from a node call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Equation {
    /// LHS names; length 1 for scalar equations.
    pub lhs: Vec<String>,
    pub rhs: Expr,
}

/// Position of one diagram element. Keys in [`DiagramLayout::positions`] use
/// the same ids the Studio diagram API serves: port and local names, and
/// `eqN` for the N-th equation box.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NodePos {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DiagramLayout {
    /// Free-form layout hints used by the GUI; ignored by the compiler.
    pub notes: Option<String>,
    /// Persisted free-form canvas positions, keyed by diagram element id.
    /// Absent entries fall back to the GUI's automatic column layout.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub positions: std::collections::BTreeMap<String, NodePos>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeDef {
    pub name: String,
    pub kind: NodeKind,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    #[serde(default)]
    pub locals: Vec<Local>,
    #[serde(default)]
    pub equations: Vec<Equation>,
    /// Reference to a contract by name in the same package.
    #[serde(default)]
    pub contract: Option<String>,
    #[serde(default)]
    pub diagram: DiagramLayout,
}

impl NodeDef {
    pub fn is_function(&self) -> bool {
        matches!(self.kind, NodeKind::Function)
    }
    pub fn is_imported(&self) -> bool {
        matches!(self.kind, NodeKind::Imported)
    }

    pub fn signature(&self) -> NodeSignature {
        NodeSignature {
            name: self.name.clone(),
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeSignature {
    pub name: String,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
}
