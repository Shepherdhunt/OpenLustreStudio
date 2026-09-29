//! Conditional activation IR ("activate if") and lowering to dataflow.
//!
//! SCADE's IfBlock is a prioritized decision tree owned by an operator:
//! `if c1` / `elsif c2` / … / `else`, where each branch is a small scope
//! assigning the block's outputs. OpenLustre models it as an
//! [`ActivationDef`] owned by an operator (exactly like a
//! [`crate::StateMachineDef`]): the conditions are evaluated in order, the
//! first that holds selects its branch, and SCADE strictness applies —
//! **every branch (and the else branch) must assign every output**, so the
//! selection can never fall through undefined.
//!
//! ## Lowering shape
//!
//! For an activation `A` with outputs `o1..ok` and branches
//! `(c1, B1) … (cn, Bn)` plus `else E`, lowering merges into the owner:
//!
//! ```text
//! var __act_A_b1, …, __act_A_bn : bool;   -- "branch selected this cycle"
//! let
//!   __act_A_b1 = c1;
//!   __act_A_b2 = not c1 and c2;
//!   …
//!   o_j = if __act_A_b1 then <o_j in B1>
//!         else if __act_A_b2 then <o_j in B2>
//!         … else <o_j in E>;
//! tel
//! ```
//!
//! The branch-selected flags are ordinary named locals so the simulator's
//! step table (and the generated C) show which branch fired on every cycle.
//!
//! ## Semantics note (stage 1)
//!
//! Branch bodies are *selected*, not *clocked*: a temporal expression inside
//! a branch (`pre`, `->`) advances every cycle regardless of which branch is
//! active — `pre x` reads the previous cycle, like SCADE's `last 'x`, not the
//! previous activation of the branch. SCADE's frozen-branch clocks are a
//! planned refinement (see `docs/scade-parity-roadmap.md`).

use serde::{Deserialize, Serialize};

use crate::expr::Expr;
use crate::node::{Equation, Local, Port};
use crate::types::Type;

/// One prioritized `if` / `elsif` branch of an activation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivationBranch {
    /// Branch label shown on the decision tree ("Engaged", "Fault", …).
    pub name: String,
    /// Boolean selection condition, evaluated after every earlier branch's
    /// condition has failed.
    pub condition: Expr,
    /// The branch scope: each equation assigns exactly one activation output.
    #[serde(default)]
    pub equations: Vec<Equation>,
}

/// A SCADE-style "activate if" decision tree owned by an operator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivationDef {
    pub name: String,
    /// The variables this activation drives. Each must exist on the owner
    /// (as an output or a local); the activation becomes their definition.
    pub outputs: Vec<Port>,
    /// The prioritized `if` / `elsif` chain — at least one branch.
    pub branches: Vec<ActivationBranch>,
    /// The `else` scope, mandatory so the tree is exhaustive by construction.
    #[serde(default)]
    pub else_equations: Vec<Equation>,
    /// The operator whose body this activation lowers into.
    pub owner: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ActLowerError {
    #[error("activation `{0}` declares no branches (needs at least `if <condition>`)")]
    NoBranches(String),
    #[error("activation `{0}` declares no outputs")]
    NoOutputs(String),
    #[error("activation `{0}`: output `{1}` is not assigned in branch `{2}` (every branch must assign every output)")]
    OutputUnassigned(String, String, String),
    #[error("activation `{0}`: branch name `{1}` is used more than once")]
    DuplicateBranch(String, String),
    #[error("activation `{0}` is owned by operator `{1}`, which does not exist")]
    UnknownOwner(String, String),
}

/// The dataflow an activation lowers to: locals (the per-branch selected
/// flags) and equations (flags + one selection chain per output), ready to
/// merge into the owner operator's body.
#[derive(Debug)]
pub struct LoweredActivation {
    pub locals: Vec<Local>,
    pub equations: Vec<Equation>,
}

/// The conventional name of an activation's branch-selected flag.
pub fn branch_flag(activation: &str, branch_index: usize) -> String {
    format!("__act_{activation}_b{}", branch_index + 1)
}

/// Validate and lower one activation. Whether the owner exists is checked by
/// [`crate::Project::lower_activations`], which sees the whole project; whether
/// each driven variable is an output/local of the owner is the type checker's
/// job (E0020 / E0022) on the merged body.
pub fn lower(act: &ActivationDef) -> Result<LoweredActivation, ActLowerError> {
    if act.branches.is_empty() {
        return Err(ActLowerError::NoBranches(act.name.clone()));
    }
    if act.outputs.is_empty() {
        return Err(ActLowerError::NoOutputs(act.name.clone()));
    }
    let mut seen = std::collections::HashSet::new();
    for b in &act.branches {
        if !seen.insert(b.name.clone()) {
            return Err(ActLowerError::DuplicateBranch(act.name.clone(), b.name.clone()));
        }
    }
    // SCADE strictness: every branch (and else) assigns every output.
    let assigns = |eqs: &[Equation], o: &str| eqs.iter().any(|e| e.lhs.len() == 1 && e.lhs[0] == o);
    for out in &act.outputs {
        for b in &act.branches {
            if !assigns(&b.equations, &out.name) {
                return Err(ActLowerError::OutputUnassigned(
                    act.name.clone(),
                    out.name.clone(),
                    b.name.clone(),
                ));
            }
        }
        if !assigns(&act.else_equations, &out.name) {
            return Err(ActLowerError::OutputUnassigned(
                act.name.clone(),
                out.name.clone(),
                "else".to_string(),
            ));
        }
    }

    let mut locals = Vec::new();
    let mut equations = Vec::new();

    // Branch-selected flags, prioritized: b_i = not c_1 and … and not c_(i-1) and c_i.
    let mut none_before: Option<Expr> = None;
    for (i, b) in act.branches.iter().enumerate() {
        let selected = match &none_before {
            None => b.condition.clone(),
            Some(prior) => Expr::and(prior.clone(), b.condition.clone()),
        };
        let flag = branch_flag(&act.name, i);
        locals.push(Local { name: flag.clone(), ty: Type::Bool });
        equations.push(Equation { lhs: vec![flag], rhs: selected });
        let not_this = Expr::not(b.condition.clone());
        none_before = Some(match none_before {
            None => not_this,
            Some(prior) => Expr::and(prior, not_this),
        });
    }

    // One selection chain per output, keyed off the flags.
    for out in &act.outputs {
        let value_in = |eqs: &[Equation]| -> Expr {
            eqs.iter()
                .find(|e| e.lhs.len() == 1 && e.lhs[0] == out.name)
                .map(|e| e.rhs.clone())
                .expect("assignment checked above")
        };
        let mut chain = value_in(&act.else_equations);
        for (i, b) in act.branches.iter().enumerate().rev() {
            chain = Expr::if_then_else(
                Expr::var(branch_flag(&act.name, i)),
                value_in(&b.equations),
                chain,
            );
        }
        equations.push(Equation { lhs: vec![out.name.clone()], rhs: chain });
    }

    Ok(LoweredActivation { locals, equations })
}
