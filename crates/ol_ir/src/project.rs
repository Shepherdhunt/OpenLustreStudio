use serde::{Deserialize, Serialize};

use crate::expr::Expr;
use crate::node::NodeDef;
use crate::state_machine::{lower, LowerError, StateMachineDef};
use crate::types::Type;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordField {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TypeBody {
    Enum(EnumDef),
    Record { name: String, fields: Vec<RecordField> },
    Alias { name: String, target: Type },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeDef {
    pub body: TypeBody,
}

impl TypeDef {
    pub fn name(&self) -> &str {
        match &self.body {
            TypeBody::Enum(e) => &e.name,
            TypeBody::Record { name, .. } => name,
            TypeBody::Alias { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstDef {
    pub name: String,
    pub ty: Type,
    pub value: Expr,
}

/// A package groups types, constants, nodes, contracts, and imported
/// operators. Contracts are stored as plain JSON values here so that the IR
/// crate does not depend on `ol_contract_ir` (the contract crate depends on
/// `ol_ir`, not the other way around). Higher layers re-hydrate them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    #[serde(default)]
    pub types: Vec<TypeDef>,
    #[serde(default)]
    pub constants: Vec<ConstDef>,
    #[serde(default)]
    pub nodes: Vec<NodeDef>,
    /// Raw contract definitions; parsed by `ol_contract_ir`.
    #[serde(default)]
    pub contracts: Vec<serde_json::Value>,
    /// Imported operator manifests; parsed by `ol_clite_emit`.
    #[serde(default)]
    pub imported_operators: Vec<serde_json::Value>,
    /// Finite state machines. They are lowered to dataflow nodes (and an
    /// auto-generated state-enum type) by [`Project::lower_state_machines`]
    /// before any downstream tool runs.
    #[serde(default)]
    pub state_machines: Vec<StateMachineDef>,
}

impl Package {
    pub fn find_node(&self, name: &str) -> Option<&NodeDef> {
        self.nodes.iter().find(|n| n.name == name)
    }

    pub fn find_type(&self, name: &str) -> Option<&TypeDef> {
        self.types.iter().find(|t| t.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub packages: Vec<Package>,
    /// Optional default entry point; used by simulator and Kind 2 adapter.
    #[serde(default)]
    pub main: Option<String>,
}

impl Project {
    pub fn find_node(&self, name: &str) -> Option<&NodeDef> {
        for pkg in &self.packages {
            if let Some(n) = pkg.find_node(name) {
                return Some(n);
            }
        }
        None
    }

    pub fn all_nodes(&self) -> impl Iterator<Item = &NodeDef> {
        self.packages.iter().flat_map(|p| p.nodes.iter())
    }

    /// Replace each [`StateMachineDef`] in every package with the dataflow
    /// node and state-enum type it lowers to. After this call, downstream
    /// tools see only ordinary nodes and types and need no per-tool
    /// awareness of state machines.
    pub fn lower_state_machines(&mut self) -> Result<(), Vec<LowerError>> {
        let mut errors = Vec::new();
        for pkg in &mut self.packages {
            let machines = std::mem::take(&mut pkg.state_machines);
            for sm in machines {
                match lower(&sm) {
                    Ok(low) => {
                        pkg.types.push(low.state_type);
                        pkg.nodes.push(low.node);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}
