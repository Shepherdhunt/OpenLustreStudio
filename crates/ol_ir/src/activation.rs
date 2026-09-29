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
//! ## Semantics (SCADE activate-if, clocked)
//!
//! Each branch runs on its own clock: it is evaluated only on the cycles it
//! is selected, and everything stateful inside it is frozen while it isn't.
//! `pre v` inside a branch is `v` at the branch's previous activation; an
//! `init -> body` takes `init` on the branch's first activation; a stateful
//! call inside a branch steps only when the branch runs. To read a variable's
//! previous-cycle value whichever branch defined it — SCADE's `last 'v`, the
//! "hold" pattern — write `last(v)` (or `last(v, init)`; without `init` the
//! first cycle reads the type's default value).
//!
//! ## Lowering shape
//!
//! For an activation `A` with branches `(c1, B1) … (cn, Bn)` plus `else E`:
//!
//! ```text
//! __act_A_b1 = c1;                      -- "branch selected this cycle",
//! __act_A_n1 = not c1;                  -- base clock (b shown in the UI)
//! __act_A_b2 = __act_A_n1 and c2;
//! __act_A_n2 = __act_A_n1 and not c2; …
//! __act_A_g2 = __act_A_b2 when not __act_A_b1;            -- nested guards
//! __act_A_g3 = __act_A_b3 when not __act_A_b1 when not __act_A_g2; …
//! -- branch k runs on  base when not b1 … when not g(k-1) when gk,
//! -- else on           base when not b1 … when not gn.
//! __act_A_s1_v = v when __act_A_b1;     -- every variable a branch reads,
//! __act_A_x1_o = <o in B1 over s1_*>;   -- sampled onto its clock
//! …
//! o = merge(__act_A_b1, __act_A_x1_o,
//!           merge(__act_A_g2, __act_A_x2_o, … __act_A_xe_o));
//! __act_A_last_v = init -> pre v;       -- for last(v)
//! ```
//!
//! A sampled local holds its value while its branch is inactive, so
//! `pre __act_A_s1_v` is exactly `v` at the previous activation — the same
//! held-value clock semantics the simulator and the generated C already
//! share for `when` / `merge`.

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use crate::expr::Expr;
use crate::node::{Equation, Local, NodeDef, Port};
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
    #[error("activation `{0}`: `last` takes a variable and an optional initial value — `last(v)` or `last(v, init)`")]
    LastArgs(String),
    #[error("activation `{0}`: `last({1})` names no variable of the owner")]
    LastUnknown(String, String),
    #[error("activation `{0}`: `last({1})` needs an initial value for this type — write `last({1}, init)`")]
    LastNeedsInit(String, String),
}

/// The dataflow an activation lowers to: locals (branch flags, guards,
/// sampled reads, per-branch values, `last` holders) and their equations,
/// ready to merge into the owner operator's body.
#[derive(Debug)]
pub struct LoweredActivation {
    pub locals: Vec<Local>,
    pub equations: Vec<Equation>,
}

/// The conventional name of an activation's branch-selected flag.
pub fn branch_flag(activation: &str, branch_index: usize) -> String {
    format!("__act_{activation}_b{}", branch_index + 1)
}

/// Validate and lower one activation into its owner `owner` (whose variable
/// types the sampled locals take). `first_variant` maps each enum type to
/// its first variant — the default a `last(v)` of that type starts from.
/// Whether each driven variable is an output/local of the owner is the type
/// checker's job (E0020 / E0022) on the merged body.
pub fn lower(
    act: &ActivationDef,
    owner: &NodeDef,
    first_variant: &HashMap<String, String>,
) -> Result<LoweredActivation, ActLowerError> {
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

    let a = &act.name;
    let n = act.branches.len();
    // Types of what a branch may read: the owner's variables, plus the
    // activation's own outputs (in case the owner has since lost one).
    let mut types: HashMap<String, Type> = HashMap::new();
    for p in owner.inputs.iter().chain(&owner.outputs) {
        types.insert(p.name.clone(), p.ty.clone());
    }
    for l in &owner.locals {
        types.insert(l.name.clone(), l.ty.clone());
    }
    for o in &act.outputs {
        types.entry(o.name.clone()).or_insert_with(|| o.ty.clone());
    }

    let mut locals = Vec::new();
    let mut equations = Vec::new();
    let mut define = |locals: &mut Vec<Local>, name: String, ty: Type, rhs: Expr| {
        locals.push(Local { name: name.clone(), ty });
        equations.push(Equation { lhs: vec![name], rhs });
    };

    // `last(v)` / `last(v, init)` → a base-clock holder of v's previous value.
    let mut lasts = LastHolders::default();
    let conditions: Vec<Expr> = act
        .branches
        .iter()
        .map(|b| lasts.rewrite(a, &b.condition))
        .collect::<Result<_, _>>()?;
    let scopes: Vec<Vec<Equation>> = act
        .branches
        .iter()
        .map(|b| &b.equations)
        .chain(std::iter::once(&act.else_equations))
        .map(|eqs| {
            eqs.iter()
                .map(|e| Ok(Equation { lhs: e.lhs.clone(), rhs: lasts.rewrite(a, &e.rhs)? }))
                .collect::<Result<Vec<_>, ActLowerError>>()
        })
        .collect::<Result<_, _>>()?;
    for (v, init) in &lasts.found {
        let ty = types
            .get(v)
            .cloned()
            .ok_or_else(|| ActLowerError::LastUnknown(a.clone(), v.clone()))?;
        let init = match init {
            Some(i) => i.clone(),
            None => default_of(&ty, first_variant)
                .ok_or_else(|| ActLowerError::LastNeedsInit(a.clone(), v.clone()))?,
        };
        let holder = last_holder(a, v);
        types.insert(holder.clone(), ty.clone());
        define(&mut locals, holder, ty, Expr::arrow(init, Expr::pre(Expr::var(v.clone()))));
    }

    // Branch-selected flags on the base clock, prioritized, as a chain:
    // n_i = n_(i-1) and not c_i ("no branch up to i taken"), and
    // b_i = n_(i-1) and c_i. Two conditions per decision — the decision
    // tree's own shape, so each branch condition's coverage reads directly.
    let mut none_before: Option<Expr> = None;
    for (i, c) in conditions.iter().enumerate() {
        let selected = match &none_before {
            None => c.clone(),
            Some(prior) => Expr::and(prior.clone(), c.clone()),
        };
        define(&mut locals, branch_flag(a, i), Type::Bool, selected);
        if i + 1 < conditions.len() {
            let not_this = Expr::not(c.clone());
            let none = format!("__act_{a}_n{}", i + 1);
            let rhs = match none_before {
                None => not_this,
                Some(prior) => Expr::and(prior, not_this),
            };
            define(&mut locals, none.clone(), Type::Bool, rhs);
            none_before = Some(Expr::var(none));
        }
    }

    // The clock variable of branch k (0-based): its flag for the first, a
    // guard sampled under every earlier branch's "not taken" for the rest.
    let clock_var = |k: usize| if k == 0 { branch_flag(a, 0) } else { format!("__act_{a}_g{}", k + 1) };
    // `e` sampled onto the clock where branches 0..k all failed.
    let under = |k: usize, mut e: Expr| {
        for j in 0..k {
            e = Expr::When { arg: Box::new(e), clock: clock_var(j), on: false };
        }
        e
    };
    // `e` sampled onto scope k's clock (k == n: the else scope).
    let onto = |k: usize, e: Expr| {
        if k < n {
            Expr::When { arg: Box::new(under(k, e)), clock: clock_var(k), on: true }
        } else {
            under(n, e)
        }
    };
    for k in 1..n {
        define(&mut locals, clock_var(k), Type::Bool, under(k, Expr::var(branch_flag(a, k))));
    }

    // Each scope's equations, over sampled copies of what they read.
    let tag = |k: usize| if k < n { (k + 1).to_string() } else { "e".to_string() };
    for (k, eqs) in scopes.iter().enumerate() {
        let mut sampled: Vec<String> = Vec::new();
        for eq in eqs {
            let mut rhs = eq.rhs.clone();
            let reads: Vec<String> = rhs.free_vars().into_iter().filter(|v| types.contains_key(v)).collect();
            for v in &reads {
                let s = format!("__act_{a}_s{}_{v}", tag(k));
                if !sampled.contains(v) {
                    sampled.push(v.clone());
                    define(&mut locals, s.clone(), types[v].clone(), onto(k, Expr::var(v.clone())));
                }
                rhs.rename_var(v, &s);
            }
            // A scope value reading no variable (`0`, `Tick(1)`) still has
            // to live on the scope's clock: sample its constants, so a call
            // among them sits on the scope's clock too and steps only while
            // the scope runs. (No constant at all — an argument-less call —
            // samples the whole value.)
            if reads.is_empty() {
                let mut pinned = false;
                rhs.walk_mut_post(&mut |x| {
                    if let Expr::Const { .. } = x {
                        let c = std::mem::replace(x, Expr::bool_lit(false));
                        *x = onto(k, c);
                        pinned = true;
                    }
                });
                if !pinned {
                    rhs = onto(k, rhs);
                }
            }
            let o = &eq.lhs[0];
            let ty = act.outputs.iter().find(|p| &p.name == o).map(|p| p.ty.clone())
                .or_else(|| types.get(o).cloned())
                .unwrap_or(Type::Bool);
            define(&mut locals, format!("__act_{a}_x{}_{o}", tag(k)), ty, rhs);
        }
    }

    // Each output: the selected scope's value, merged back to the base clock.
    for out in &act.outputs {
        let o = &out.name;
        let mut chain = Expr::var(format!("__act_{a}_x{}_{o}", tag(n)));
        for k in (0..n).rev() {
            chain = Expr::Merge {
                clock: clock_var(k),
                on_true: Box::new(Expr::var(format!("__act_{a}_x{}_{o}", tag(k)))),
                on_false: Box::new(chain),
            };
        }
        equations.push(Equation { lhs: vec![o.clone()], rhs: chain });
    }

    Ok(LoweredActivation { locals, equations })
}

/// The base-clock local that holds `v`'s previous-cycle value for `last(v)`.
pub fn last_holder(activation: &str, v: &str) -> String {
    format!("__act_{activation}_last_{v}")
}

/// `last(v[, init])` occurrences, rewritten to their holders as found.
#[derive(Default)]
struct LastHolders {
    /// `(v, explicit init)` in first-seen order.
    found: Vec<(String, Option<Expr>)>,
}

impl LastHolders {
    fn rewrite(&mut self, act: &str, e: &Expr) -> Result<Expr, ActLowerError> {
        let mut e = e.clone();
        let mut err = None;
        e.walk_mut(&mut |x| {
            let Expr::Call { node, args } = x else { return };
            if node != "last" {
                return;
            }
            match args.as_slice() {
                [Expr::Var { name }] | [Expr::Var { name }, _] => {
                    let init = args.get(1).cloned();
                    match self.found.iter_mut().find(|(v, _)| v == name) {
                        Some((_, i)) => {
                            if i.is_none() {
                                *i = init;
                            }
                        }
                        None => self.found.push((name.clone(), init)),
                    }
                    *x = Expr::var(last_holder(act, name));
                }
                _ => err = Some(ActLowerError::LastArgs(act.to_string())),
            }
        });
        match err {
            Some(e) => Err(e),
            None => Ok(e),
        }
    }
}

/// A type's default value as an expression (what `last(v)` reads on the
/// first cycle); None for arrays and records, which need an explicit init.
fn default_of(ty: &Type, first_variant: &HashMap<String, String>) -> Option<Expr> {
    Some(match ty {
        Type::Bool => Expr::bool_lit(false),
        Type::Float32 | Type::Float64 => Expr::Const { lit: crate::expr::Literal::Float { value: 0.0 } },
        Type::Char => Expr::Const { lit: crate::expr::Literal::Char { value: 0 } },
        t if t.is_integer() => Expr::int_lit(0),
        Type::Named { name } => Expr::var(first_variant.get(name)?.clone()),
        _ => return None,
    })
}
