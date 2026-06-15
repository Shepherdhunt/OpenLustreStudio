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

/// A debug log probe — SCADE's "log message". Observation-only: it does not
/// affect the dataflow, but in a debug run the generated C prints
/// `<label>: <value>` for the named variable. `var` must be a name in the
/// node (input, output, or local).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// The text shown before the value, e.g. `"altitude"`.
    pub label: String,
    /// The variable whose value is logged.
    pub var: String,
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
    /// Grid pitch in canvas units. Dragged boxes snap to multiples of this,
    /// and saved positions land on it — the diagram's drawing metadata lives
    /// in the model file, so it opens with the same grid it was drawn on.
    /// `None` falls back to the GUI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<u32>,
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
    /// Debug log probes — printed by a debug run, ignored by normal codegen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<Probe>,
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
