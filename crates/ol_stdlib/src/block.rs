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
use ol_ir::{
    state_machine::{lower as lower_state_machine, LowerError as SmLowerError},
    Equation, Local, NodeDef, NodeKind, Port, StateDef, StateMachineDef, Transition, TypeDef,
};

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
    #[error("block `{block}`: unknown kind `{kind}` (expected function, operator, imported, or state_machine)")]
    BadKind { block: String, kind: String },
    #[error("block `{block}`: state-machine lowering: {source}")]
    StateMachine {
        block: String,
        #[source]
        source: SmLowerError,
    },
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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawTransition {
    pub guard: String,
    pub target: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawState {
    pub name: String,
    #[serde(default)]
    pub equations: Vec<RawEquation>,
    #[serde(default)]
    pub transitions: Vec<RawTransition>,
    /// Signals this state emits while active (each declared in the block's
    /// `signals` list).
    #[serde(default)]
    pub emits: Vec<String>,
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
    /// Only set when `kind: state_machine`. Names the start state.
    #[serde(default)]
    pub initial_state: Option<String>,
    /// Only set when `kind: state_machine`.
    #[serde(default)]
    pub states: Vec<RawState>,
    /// Only set when `kind: state_machine`: boolean signals local to the
    /// automaton, true exactly while an emitting state is active.
    #[serde(default)]
    pub signals: Vec<String>,
    /// Generic TYPE parameters (`'T` in the port types), with an optional
    /// constraint (`any` / `numeric` / `integer` / `float`). Array SIZE
    /// parameters (`int32[N]`) need no declaration — they register
    /// implicitly. A block with generics is a template: it instantiates per
    /// call site and never reaches a backend itself.
    #[serde(default)]
    pub generics: Vec<RawGeneric>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawGeneric {
    pub name: String,
    #[serde(default)]
    pub constraint: ol_ir::TypeConstraint,
}

/// Lowering result for one block.
pub struct LoweredBlock {
    pub node: NodeDef,
    pub contract: Option<ContractDef>,
    pub category: Option<String>,
    /// Auxiliary types the block introduces — currently only the auto-generated
    /// `{Name}_StateEnum` from state-machine lowering.
    pub extra_types: Vec<TypeDef>,
}

impl LibBlock {
    fn contract_name(&self) -> String {
        format!("{}_contract", self.name)
    }

    pub fn lower(&self) -> Result<LoweredBlock, LowerError> {
        if self.kind == "state_machine" {
            return self.lower_state_machine();
        }
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
            probes: Vec::new(),
            requirements: Vec::new(),
            sysml: None,
            generics: self
                .generics
                .iter()
                .map(|g| ol_ir::GenericParam::Type {
                    name: g.name.clone(),
                    constraint: g.constraint,
                })
                .collect(),
        };

        Ok(LoweredBlock {
            node,
            contract,
            category: self.category.clone(),
            extra_types: vec![],
        })
    }

    fn lower_state_machine(&self) -> Result<LoweredBlock, LowerError> {
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

        let initial_state = self
            .initial_state
            .clone()
            .ok_or_else(|| LowerError::Parse {
                block: self.name.clone(),
                field: "initial_state".into(),
                source: ParseError::UnexpectedEof,
            })?;

        let mut states = Vec::new();
        for s in &self.states {
            let mut eqs = Vec::new();
            for eq in &s.equations {
                let rhs = self.parse_body(&eq.body, "states[].equations")?;
                eqs.push(Equation {
                    lhs: eq.lhs.clone(),
                    rhs,
                });
            }
            let mut transitions = Vec::new();
            for t in &s.transitions {
                let guard = self.parse_body(&t.guard, "states[].transitions.guard")?;
                transitions.push(Transition {
                    guard,
                    target: t.target.clone(),
                });
            }
            states.push(StateDef {
                name: s.name.clone(),
                equations: eqs,
                transitions,
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: s.emits.clone(),
            });
        }

        let contract = self.lower_contract(&inputs, &outputs)?;

        let sm = StateMachineDef {
            name: self.name.clone(),
            inputs,
            outputs,
            locals,
            initial_state,
            states,
            signals: self.signals.clone(),
            contract: contract.as_ref().map(|_| self.contract_name()),
            owner: None,
            diagram: Default::default(),
        };
        let lowered = lower_state_machine(&sm).map_err(|e| LowerError::StateMachine {
            block: self.name.clone(),
            source: e,
        })?;

        Ok(LoweredBlock {
            node: lowered.node,
            contract,
            category: self.category.clone(),
            extra_types: lowered.state_types,
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
                    requirements: vec![],
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
                    requirements: vec![],
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
                requirements: vec![],
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
