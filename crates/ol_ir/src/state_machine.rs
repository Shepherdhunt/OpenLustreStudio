//! State Machine IR and lowering to the dataflow IR.
//!
//! The plan's top-level architecture lists a "State Machine IR" alongside the
//! Dataflow IR. This module defines that surface — a finite-state machine with
//! per-state equations and guarded transitions — and lowers it to a plain
//! [`NodeDef`] plus an auto-generated state-enum [`TypeDef`], so every
//! downstream tool (typecheck, simulator, emitters) handles it without
//! per-tool changes.
//!
//! ## Lowering shape
//!
//! For a machine `M(in...) returns (out...)` with states `S1, S2, ...` and
//! initial state `S0`:
//!
//! ```text
//! type M_State = enum { S1, S2, ... };
//! operator M(in...) returns (out...);
//! var __sm_state, __sm_next_state: M_State;
//! let
//!   __sm_state = S0 -> pre __sm_next_state;
//!   __sm_next_state =
//!     if __sm_state = S1 then <transitions of S1, default __sm_state>
//!     else if __sm_state = S2 then <transitions of S2, default __sm_state>
//!     ...
//!     else __sm_state;
//!   out_k =
//!     if __sm_state = S1 then <rhs of out_k in S1>
//!     else if __sm_state = S2 then <rhs of out_k in S2>
//!     ...
//!     else <type default>;
//! tel
//! ```
//!
//! Every output is required to be assigned in every state — matching SCADE's
//! strictness — so the chain never falls through to a default at runtime.

use serde::{Deserialize, Serialize};

use crate::expr::{BinOp, Expr, Literal};
use crate::node::{Equation, Local, NodeDef, NodeKind, Port};
use crate::project::{EnumDef, TypeBody, TypeDef};
use crate::types::Type;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// Boolean expression. When it holds in the source state, the machine
    /// moves to `target` on the next cycle.
    pub guard: Expr,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateDef {
    pub name: String,
    /// Equations active while the machine is in this state. Each must assign
    /// exactly one variable; one equation per output is required.
    #[serde(default)]
    pub equations: Vec<Equation>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateMachineDef {
    pub name: String,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    #[serde(default)]
    pub locals: Vec<Local>,
    pub initial_state: String,
    pub states: Vec<StateDef>,
    #[serde(default)]
    pub contract: Option<String>,
}

/// Lowering result: the auto-generated state-enum type, the resulting
/// dataflow node, and the conventional name of the state local (so callers
/// can inspect or label it in a UI).
#[derive(Debug)]
pub struct LoweredMachine {
    pub state_type: TypeDef,
    pub node: NodeDef,
    pub state_local: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LowerError {
    #[error("machine `{0}` declares no states")]
    NoStates(String),
    #[error("machine `{0}`: initial state `{1}` is not declared")]
    UnknownInitialState(String, String),
    #[error("machine `{0}`: state `{1}` declares an unknown target state `{2}`")]
    UnknownTarget(String, String, String),
    #[error("machine `{0}`: output `{1}` is not assigned in state `{2}`")]
    OutputUnassigned(String, String, String),
}

const STATE_LOCAL: &str = "__sm_state";
const NEXT_STATE_LOCAL: &str = "__sm_next_state";

pub fn lower(sm: &StateMachineDef) -> Result<LoweredMachine, LowerError> {
    if sm.states.is_empty() {
        return Err(LowerError::NoStates(sm.name.clone()));
    }
    if !sm.states.iter().any(|s| s.name == sm.initial_state) {
        return Err(LowerError::UnknownInitialState(
            sm.name.clone(),
            sm.initial_state.clone(),
        ));
    }
    for s in &sm.states {
        for t in &s.transitions {
            if !sm.states.iter().any(|x| x.name == t.target) {
                return Err(LowerError::UnknownTarget(
                    sm.name.clone(),
                    s.name.clone(),
                    t.target.clone(),
                ));
            }
        }
    }
    for out in &sm.outputs {
        for s in &sm.states {
            if !state_assigns(s, &out.name) {
                return Err(LowerError::OutputUnassigned(
                    sm.name.clone(),
                    out.name.clone(),
                    s.name.clone(),
                ));
            }
        }
    }

    let state_type_name = format!("{}_State", sm.name);
    let state_ty = Type::Named(state_type_name.clone());

    let state_type = TypeDef {
        body: TypeBody::Enum(EnumDef {
            name: state_type_name.clone(),
            variants: sm.states.iter().map(|s| s.name.clone()).collect(),
        }),
    };

    let state_eq = Equation {
        lhs: vec![STATE_LOCAL.into()],
        rhs: Expr::arrow(
            Expr::var(&sm.initial_state),
            Expr::pre(Expr::var(NEXT_STATE_LOCAL)),
        ),
    };

    let next_state_eq = Equation {
        lhs: vec![NEXT_STATE_LOCAL.into()],
        rhs: build_next_state_expr(sm),
    };

    let mut equations = vec![state_eq, next_state_eq];

    for out in &sm.outputs {
        equations.push(Equation {
            lhs: vec![out.name.clone()],
            rhs: build_output_chain(sm, &out.name, &out.ty),
        });
    }

    let mut locals = sm.locals.clone();
    locals.push(Local {
        name: STATE_LOCAL.into(),
        ty: state_ty.clone(),
    });
    locals.push(Local {
        name: NEXT_STATE_LOCAL.into(),
        ty: state_ty,
    });

    let node = NodeDef {
        name: sm.name.clone(),
        kind: NodeKind::Operator,
        inputs: sm.inputs.clone(),
        outputs: sm.outputs.clone(),
        locals,
        equations,
        contract: sm.contract.clone(),
        diagram: Default::default(),
    };

    Ok(LoweredMachine {
        state_type,
        node,
        state_local: STATE_LOCAL.into(),
    })
}

fn state_assigns(state: &StateDef, name: &str) -> bool {
    state
        .equations
        .iter()
        .any(|eq| eq.lhs.len() == 1 && eq.lhs[0] == name)
}

fn build_next_state_expr(sm: &StateMachineDef) -> Expr {
    // Innermost else: stay in the current state.
    let stay = Expr::var(STATE_LOCAL);
    let mut chain = stay.clone();
    for s in sm.states.iter().rev() {
        // For each transition in declaration order, build a guarded chain
        // ending in `stay`. The first transition whose guard is true wins.
        let mut inner = stay.clone();
        for t in s.transitions.iter().rev() {
            inner = Expr::if_then_else(t.guard.clone(), Expr::var(&t.target), inner);
        }
        chain = Expr::if_then_else(
            Expr::bin(BinOp::Eq, Expr::var(STATE_LOCAL), Expr::var(&s.name)),
            inner,
            chain,
        );
    }
    chain
}

fn build_output_chain(sm: &StateMachineDef, output: &str, ty: &Type) -> Expr {
    let mut chain = default_expr_for_type(ty);
    for s in sm.states.iter().rev() {
        let assigned = s
            .equations
            .iter()
            .find(|eq| eq.lhs.len() == 1 && eq.lhs[0] == output)
            .map(|eq| eq.rhs.clone())
            .unwrap_or_else(|| default_expr_for_type(ty));
        chain = Expr::if_then_else(
            Expr::bin(BinOp::Eq, Expr::var(STATE_LOCAL), Expr::var(&s.name)),
            assigned,
            chain,
        );
    }
    chain
}

fn default_expr_for_type(ty: &Type) -> Expr {
    match ty {
        Type::Bool => Expr::bool_lit(false),
        Type::Float32 | Type::Float64 => Expr::Const {
            lit: Literal::Float { value: 0.0 },
        },
        Type::Int8
        | Type::Int16
        | Type::Int32
        | Type::Int64
        | Type::Uint8
        | Type::Uint16
        | Type::Uint32
        | Type::Uint64 => Expr::int_lit(0),
        // Compound / named types fall back to the integer-zero literal; if
        // every state assigns the output (which we require above) this branch
        // is unreachable at runtime, but lowering still has to produce
        // something type-shaped for the chain's terminal else.
        Type::Array { .. } | Type::Named(_) => Expr::int_lit(0),
    }
}
