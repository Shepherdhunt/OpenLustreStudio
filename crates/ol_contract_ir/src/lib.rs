//! OpenLustre Studio: CoCoSpec contract IR.
//!
//! A contract is a separate first-class artifact from the node it describes,
//! exactly as in Kind 2's CoCoSpec model: it carries assumptions, guarantees,
//! ghost variables, modes, and imports, and refers to ports by name. The
//! contract checker verifies well-formedness and connection to a node; the
//! emitter renders it back into Kind 2-compatible Lustre syntax.

use serde::{Deserialize, Serialize};

use ol_ir::{Expr, Port, Type};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assumption {
    pub name: Option<String>,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guarantee {
    pub name: Option<String>,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GhostVar {
    pub name: String,
    pub ty: Type,
    pub definition: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mode {
    pub name: String,
    #[serde(default)]
    pub requires: Vec<Expr>,
    #[serde(default)]
    pub ensures: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractImport {
    pub contract: String,
    pub input_map: Vec<(String, Expr)>,
    pub output_map: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractDef {
    pub name: String,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    #[serde(default)]
    pub ghost_vars: Vec<GhostVar>,
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
    #[serde(default)]
    pub guarantees: Vec<Guarantee>,
    #[serde(default)]
    pub modes: Vec<Mode>,
    #[serde(default)]
    pub imports: Vec<ContractImport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractRef {
    pub contract: String,
}

/// Resolve the contracts stored as raw JSON on `Package::contracts` into
/// strongly typed `ContractDef`s. Returns `(contracts, errors)` so callers
/// can surface partial-parse failures alongside the successfully parsed
/// contracts.
pub fn parse_contracts(raw: &[serde_json::Value]) -> (Vec<ContractDef>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (i, v) in raw.iter().enumerate() {
        match serde_json::from_value::<ContractDef>(v.clone()) {
            Ok(c) => out.push(c),
            Err(e) => errors.push(format!("contract #{i}: {e}")),
        }
    }
    (out, errors)
}

impl ContractDef {
    pub fn find_mode(&self, name: &str) -> Option<&Mode> {
        self.modes.iter().find(|m| m.name == name)
    }
}
