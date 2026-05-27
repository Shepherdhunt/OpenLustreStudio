//! The concise standard-library block schema and its lowering to strict IR.
//!
//! A library file documents reusable blocks in a compact YAML form, e.g.:
//!
//! ```yaml
//! - name: And
//!   kind: function
//!   category: logic
//!   inputs:  [{ name: a, type: bool }, { name: b, type: bool }]
//!   outputs: [{ name: y, type: bool }]
//!   equations: [{ lhs: [y], body: "a and b" }]
//!   contract:
//!     guarantees: ["y = (a and b)"]
//! ```
//!
//! [`LibBlock::lower`] turns one block into an [`ol_ir::NodeDef`] plus an
//! optional [`ol_contract_ir::ContractDef`], parsing every textual expression
//! and type through [`crate::parser`].

use serde::Deserialize;

use ol_contract_ir::{Assumption, ContractDef, Guarantee, Mode};
use ol_ir::{Equation, Local, NodeDef, NodeKind, Port};

use crate::parser::{self, ParseError};

#[derive(Debug, thiserror::Error)]
pub enum LowerError {
    #[error("block `{block}`: {field}: {source}")]
    Parse {
        block: String,
        field: String,
        #[source]
        source: ParseError,
    },
    #[error("block `{block}`: unknown kind `{kind}` (expected function, operator, or imported)")]
    BadKind { block: String, kind: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPort {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawLocal {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawEquation {
    pub lhs: Vec<String>,
    pub body: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawMode {
    pub name: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub ensures: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawContract {
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub guarantees: Vec<String>,
    #[serde(default)]
    pub modes: Vec<RawMode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibBlock {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub inputs: Vec<RawPort>,
    #[serde(default)]
    pub outputs: Vec<RawPort>,
    #[serde(default)]
    pub locals: Vec<RawLocal>,
    #[serde(default)]
    pub equations: Vec<RawEquation>,
    #[serde(default)]
    pub contract: Option<RawContract>,
}

/// Lowering result for one block.
pub struct LoweredBlock {
    pub node: NodeDef,
    pub contract: Option<ContractDef>,
    pub category: Option<String>,
}

impl LibBlock {
    fn contract_name(&self) -> String {
        format!("{}_contract", self.name)
    }

    pub fn lower(&self) -> Result<LoweredBlock, LowerError> {
        let kind = match self.kind.as_str() {
            "function" => NodeKind::Function,
            "operator" => NodeKind::Operator,
            "imported" => NodeKind::Imported,
            other => {
                return Err(LowerError::BadKind {
                    block: self.name.clone(),
                    kind: other.to_string(),
                })
            }
        };

        let inputs = self.lower_ports(&self.inputs, "inputs")?;
        let outputs = self.lower_ports(&self.outputs, "outputs")?;
        let locals = self
            .locals
            .iter()
            .map(|l| {
                Ok(Local {
                    name: l.name.clone(),
                    ty: self.parse_ty(&l.ty, "locals")?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut equations = Vec::new();
        for eq in &self.equations {
            let rhs = self.parse_body(&eq.body, "equations")?;
            equations.push(Equation {
                lhs: eq.lhs.clone(),
                rhs,
            });
        }

        let contract = self.lower_contract(&inputs, &outputs)?;

        let node = NodeDef {
            name: self.name.clone(),
            kind,
            inputs,
            outputs,
            locals,
            equations,
            contract: contract.as_ref().map(|_| self.contract_name()),
            diagram: Default::default(),
        };

        Ok(LoweredBlock {
            node,
            contract,
            category: self.category.clone(),
        })
    }

    fn lower_ports(&self, raw: &[RawPort], field: &str) -> Result<Vec<Port>, LowerError> {
        raw.iter()
            .map(|p| {
                Ok(Port {
                    name: p.name.clone(),
                    ty: self.parse_ty(&p.ty, field)?,
                })
            })
            .collect()
    }

    fn lower_contract(
        &self,
        inputs: &[Port],
        outputs: &[Port],
    ) -> Result<Option<ContractDef>, LowerError> {
        let Some(c) = &self.contract else {
            return Ok(None);
        };
        let assumptions = c
            .assumptions
            .iter()
            .map(|s| {
                Ok(Assumption {
                    name: None,
                    expr: self.parse_body(s, "contract.assumptions")?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let guarantees = c
            .guarantees
            .iter()
            .map(|s| {
                Ok(Guarantee {
                    name: None,
                    expr: self.parse_body(s, "contract.guarantees")?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut modes = Vec::new();
        for m in &c.modes {
            let requires = m
                .requires
                .iter()
                .map(|s| self.parse_body(s, "contract.modes.requires"))
                .collect::<Result<Vec<_>, _>>()?;
            let ensures = m
                .ensures
                .iter()
                .map(|s| self.parse_body(s, "contract.modes.ensures"))
                .collect::<Result<Vec<_>, _>>()?;
            modes.push(Mode {
                name: m.name.clone(),
                requires,
                ensures,
            });
        }
        Ok(Some(ContractDef {
            name: self.contract_name(),
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            ghost_vars: vec![],
            assumptions,
            guarantees,
            modes,
            imports: vec![],
        }))
    }

    fn parse_ty(&self, src: &str, field: &str) -> Result<ol_ir::Type, LowerError> {
        parser::parse_type(src).map_err(|source| LowerError::Parse {
            block: self.name.clone(),
            field: field.to_string(),
            source,
        })
    }

    fn parse_body(&self, src: &str, field: &str) -> Result<ol_ir::Expr, LowerError> {
        parser::parse_expr(src).map_err(|source| LowerError::Parse {
            block: self.name.clone(),
            field: field.to_string(),
            source,
        })
    }
}
