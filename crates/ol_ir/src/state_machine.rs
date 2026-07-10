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
//! type M_StateEnum = enum { S1, S2, ... };
//! operator M(in...) returns (out...);
//! var __sm_state, __sm_next_state: M_StateEnum;
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
    /// exactly one variable; every output must be assigned along every path.
    #[serde(default)]
    pub equations: Vec<Equation>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
    /// Nested automata (SCADE hierarchy) that run while this state is active.
    /// A region drives the outputs it assigns; on (re-)entry it restarts at its
    /// initial state unless `history` is set. Empty for a flat state.
    #[serde(default)]
    pub regions: Vec<Region>,
    /// "Refine" this state into another state machine by name: while the state
    /// is active that machine runs as a nested region. Resolved against the
    /// project's machines at lowering time (so edits to the sub-machine
    /// propagate), then validated like any nested region. `None` for a state
    /// that does not delegate.
    #[serde(default)]
    pub refines: Option<String>,
    /// SCADE history on the `refines` delegation: when set, the inlined
    /// sub-machine resumes at the sub-state it held on exit instead of
    /// restarting at its initial state. Meaningless without `refines`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refine_history: bool,
    /// Signals this state emits while active (SCADE signal emission). Each
    /// must be declared in the machine's `signals` list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emits: Vec<String>,
}

/// A nested automaton inside a state: its own initial state and states (each of
/// which may nest further). Active only while the containing state is active.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub initial_state: String,
    pub states: Vec<StateDef>,
    /// Resume at the sub-state held on exit when the region re-activates
    /// (SCADE history), instead of restarting at `initial_state`.
    #[serde(default)]
    pub history: bool,
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
    /// SCADE-style signals: boolean events local to the automaton. A signal
    /// is `true` exactly while a state that `emits` it is active (same-cycle
    /// broadcast), `false` otherwise; guards and state equations may read it.
    /// Each lowers to a bool local of the node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<String>,
    #[serde(default)]
    pub contract: Option<String>,
    /// The operator this machine belongs to. `Some(op)` machines are
    /// operator-owned: lowering merges the automaton into operator `op`'s body
    /// (it drives `op`'s outputs) rather than emitting a standalone node. `None`
    /// machines (e.g. stdlib library blocks) lower to their own node.
    #[serde(default)]
    pub owner: Option<String>,
    /// Canvas positions of the machine's states in the graphical automaton
    /// editor, keyed by state name. Purely presentational — lowering and
    /// diffing ignore it, exactly like an operator's diagram layout.
    #[serde(default, skip_serializing_if = "layout_is_empty")]
    pub diagram: crate::node::DiagramLayout,
}

fn layout_is_empty(d: &crate::node::DiagramLayout) -> bool {
    d.positions.is_empty() && d.notes.is_none() && d.grid.is_none()
}

/// Lowering result: the auto-generated state-enum types (one per region — the
/// top automaton and every nested one), the resulting dataflow node, and the
/// conventional name of the top state local (so callers can label it in a UI).
#[derive(Debug)]
pub struct LoweredMachine {
    pub state_types: Vec<TypeDef>,
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
    #[error("machine `{0}`: state name `{1}` is used more than once (state names must be unique across all regions)")]
    DuplicateState(String, String),
    #[error("machine `{0}`: state `{1}` refines unknown machine `{2}`")]
    UnknownRefine(String, String, String),
    #[error("machine `{0}`: refinement cycle through machine `{1}`")]
    RefineCycle(String, String),
    #[error("machine `{0}` is owned by operator `{1}`, which does not exist")]
    UnknownOwner(String, String),
    #[error("machine `{0}`: state `{1}` emits undeclared signal `{2}` (declare it in the machine's signals)")]
    UnknownSignal(String, String, String),
    #[error("machine `{0}`: signal `{1}` is declared more than once")]
    DuplicateSignal(String, String),
    #[error("machine `{0}`: signal `{1}` collides with an input, output, local, or state name")]
    SignalClash(String, String),
    #[error("machine `{0}`: `last {1}` — `{1}` is not an input, output, or local of the machine")]
    UnknownLastVar(String, String),
}

/// Resolve every `refines` reference in `sm` into an inlined nested [`Region`]
/// built from the referenced machine's (recursively resolved) states. Looks
/// machines up in `by_name`; reports an unknown reference or a refinement
/// cycle. The result has `refines: None` everywhere and is ready for [`lower`].
/// Name collisions between a parent and a sub-machine surface later as
/// `DuplicateState` (state names must be unique across the whole machine).
pub fn resolve_refines(
    sm: &StateMachineDef,
    by_name: &std::collections::HashMap<String, StateMachineDef>,
) -> Result<StateMachineDef, LowerError> {
    let mut stack = vec![sm.name.clone()];
    // Signals declared by inlined sub-machines merge (by name) into the
    // resolved machine, so their states' `emits` stay valid after inlining.
    let mut signals = sm.signals.clone();
    let states = resolve_states(&sm.name, &sm.states, by_name, &mut stack, &mut signals)?;
    Ok(StateMachineDef { states, signals, ..sm.clone() })
}

/// Prefix every state name (and the transition targets / region initial-states
/// that reference them) throughout a resolved subtree, so an inlined
/// sub-machine's states are globally unique. Equations and guards reference
/// only inputs/outputs/locals, so they are untouched.
fn prefix_states(states: Vec<StateDef>, prefix: &str) -> Vec<StateDef> {
    states
        .into_iter()
        .map(|s| StateDef {
            name: format!("{prefix}{}", s.name),
            equations: s.equations,
            transitions: s
                .transitions
                .into_iter()
                .map(|t| Transition { guard: t.guard, target: format!("{prefix}{}", t.target) })
                .collect(),
            regions: s
                .regions
                .into_iter()
                .map(|r| Region {
                    initial_state: format!("{prefix}{}", r.initial_state),
                    states: prefix_states(r.states, prefix),
                    history: r.history,
                })
                .collect(),
            refines: None,
            refine_history: false,
            // Signal names are machine-scoped (merged, not renamed), so
            // emissions travel unprefixed with the state.
            emits: s.emits,
        })
        .collect()
}

fn resolve_states(
    machine: &str,
    states: &[StateDef],
    by_name: &std::collections::HashMap<String, StateMachineDef>,
    stack: &mut Vec<String>,
    signals: &mut Vec<String>,
) -> Result<Vec<StateDef>, LowerError> {
    let mut out = Vec::with_capacity(states.len());
    for st in states {
        // Resolve any already-inline regions, then the `refines` delegation.
        let mut regions = Vec::with_capacity(st.regions.len());
        for r in &st.regions {
            regions.push(Region {
                initial_state: r.initial_state.clone(),
                states: resolve_states(machine, &r.states, by_name, stack, signals)?,
                history: r.history,
            });
        }
        if let Some(target) = &st.refines {
            if stack.contains(target) {
                return Err(LowerError::RefineCycle(machine.to_string(), target.clone()));
            }
            let sub = by_name.get(target).ok_or_else(|| {
                LowerError::UnknownRefine(machine.to_string(), st.name.clone(), target.clone())
            })?;
            stack.push(target.clone());
            let sub_states = resolve_states(machine, &sub.states, by_name, stack, signals)?;
            stack.pop();
            for s in &sub.signals {
                if !signals.contains(s) {
                    signals.push(s.clone());
                }
            }
            // Qualify the inlined sub-machine's state names per refinement site
            // (`<parent state>_`) so they collide with neither the standalone
            // version of the sub-machine nor another refinement of it.
            let prefix = format!("{}_", st.name);
            regions.push(Region {
                initial_state: format!("{prefix}{}", sub.initial_state),
                states: prefix_states(sub_states, &prefix),
                history: st.refine_history,
            });
        }
        out.push(StateDef {
            name: st.name.clone(),
            equations: st.equations.clone(),
            transitions: st.transitions.clone(),
            regions,
            refines: None,
            refine_history: false,
            emits: st.emits.clone(),
        });
    }
    Ok(out)
}

const STATE_LOCAL: &str = "__sm_state";
const NEXT_STATE_LOCAL: &str = "__sm_next_state";

/// Resolve every `last x` in the machine's expressions to
/// `default(ty) -> pre x` — the value `x` held at the previous cycle, with
/// the type's default on the first one. `x` must be a port or local of the
/// machine (it needs a per-cycle value to look back at).
fn resolve_last(sm: &StateMachineDef) -> Result<StateMachineDef, LowerError> {
    let mut types: std::collections::BTreeMap<&str, &Type> = Default::default();
    for p in sm.inputs.iter().chain(sm.outputs.iter()) {
        types.insert(&p.name, &p.ty);
    }
    for l in &sm.locals {
        types.insert(&l.name, &l.ty);
    }
    fn rewrite(
        e: &mut Expr,
        machine: &str,
        types: &std::collections::BTreeMap<&str, &Type>,
    ) -> Result<(), LowerError> {
        let mut err: Option<LowerError> = None;
        e.visit_mut(&mut |sub: &mut Expr| {
            if err.is_some() {
                return;
            }
            if let Expr::Last { name } = sub {
                match types.get(name.as_str()) {
                    Some(ty) => {
                        *sub = Expr::arrow(
                            default_expr_for_type(ty),
                            Expr::pre(Expr::var(name.clone())),
                        );
                    }
                    None => {
                        err = Some(LowerError::UnknownLastVar(
                            machine.to_string(),
                            name.clone(),
                        ));
                    }
                }
            }
        });
        err.map_or(Ok(()), Err)
    }
    let mut out = sm.clone();
    fn walk_states(
        states: &mut [StateDef],
        machine: &str,
        types: &std::collections::BTreeMap<&str, &Type>,
    ) -> Result<(), LowerError> {
        for st in states {
            for eq in &mut st.equations {
                rewrite(&mut eq.rhs, machine, types)?;
            }
            for t in &mut st.transitions {
                rewrite(&mut t.guard, machine, types)?;
            }
            for r in &mut st.regions {
                walk_states(&mut r.states, machine, types)?;
            }
        }
        Ok(())
    }
    walk_states(&mut out.states, &sm.name, &types)?;
    Ok(out)
}

pub fn lower(sm: &StateMachineDef) -> Result<LoweredMachine, LowerError> {
    let sm = &resolve_last(sm)?;
    if sm.states.is_empty() {
        return Err(LowerError::NoStates(sm.name.clone()));
    }
    // Validate the whole tree before emitting: unique state names, known
    // per-region initial states and transition targets, and SCADE strictness
    // (every output assigned along every path).
    let mut names: Vec<String> = Vec::new();
    collect_state_names(&sm.states, &mut names);
    let mut seen = std::collections::HashSet::new();
    for n in &names {
        if !seen.insert(n.clone()) {
            return Err(LowerError::DuplicateState(sm.name.clone(), n.clone()));
        }
    }
    validate_region(sm, &sm.initial_state, &sm.states)?;
    for out in &sm.outputs {
        require_cover(sm, &out.name, &sm.states)?;
    }
    validate_signals(sm, &names)?;

    let mut lo = Lower {
        machine: sm,
        enums: Vec::new(),
        locals: sm.locals.clone(),
        equations: Vec::new(),
        next_id: 0,
        signal_conds: std::collections::BTreeMap::new(),
    };
    let top = lo.emit_region(&sm.initial_state, &sm.states, Expr::bool_lit(true), false);
    for out in &sm.outputs {
        let rhs = lo.value_of(&out.name, &top, &out.ty);
        lo.equations.push(Equation { lhs: vec![out.name.clone()], rhs });
    }
    // A signal is true exactly while some emitting state is active: the
    // or-chain of the (region-active and state = S) conditions collected
    // during region emission; never emitted means constantly false.
    for sig in &sm.signals {
        let rhs = match lo.signal_conds.remove(sig) {
            Some(conds) => {
                let mut it = conds.into_iter();
                let first = it.next().expect("non-empty by construction");
                it.fold(first, Expr::or)
            }
            None => Expr::bool_lit(false),
        };
        lo.locals.push(Local { name: sig.clone(), ty: Type::Bool });
        lo.equations.push(Equation { lhs: vec![sig.clone()], rhs });
    }

    let node = NodeDef {
        name: sm.name.clone(),
        kind: NodeKind::Operator,
        inputs: sm.inputs.clone(),
        outputs: sm.outputs.clone(),
        locals: lo.locals,
        equations: lo.equations,
        contract: sm.contract.clone(),
        diagram: Default::default(),
        probes: Vec::new(),
        requirements: Vec::new(),
        sysml: None,
    };
    Ok(LoweredMachine {
        state_types: lo.enums,
        node,
        state_local: STATE_LOCAL.into(),
    })
}

// --- recursive lowering ------------------------------------------------------

/// The lowered shape of one region: the name of its state variable and, per
/// state, the state's definition plus the regions nested inside it (paired with
/// their lowered handles) — enough to build each output's selection chain.
struct RegionInfo<'a> {
    state_var: String,
    states: Vec<StateNode<'a>>,
}
struct StateNode<'a> {
    def: &'a StateDef,
    nested: Vec<(&'a Region, RegionInfo<'a>)>,
}

struct Lower<'a> {
    machine: &'a StateMachineDef,
    enums: Vec<TypeDef>,
    locals: Vec<Local>,
    equations: Vec<Equation>,
    next_id: usize,
    /// Per signal, the activation conditions of the states that emit it,
    /// collected while regions are lowered (a nested emitter contributes
    /// `region_active and state = S`; a top-level one just `state = S`).
    signal_conds: std::collections::BTreeMap<String, Vec<Expr>>,
}

impl<'a> Lower<'a> {
    /// Emit the state-enum, state/next locals and equations for one region (and
    /// recursively its nested regions), and return its handle. `active` is the
    /// boolean condition under which the region runs (the constant `true` for
    /// the top automaton; `parent_active and parent_state = S` for a region
    /// nested in state `S`).
    fn emit_region(
        &mut self,
        initial: &str,
        states: &'a [StateDef],
        active: Expr,
        history: bool,
    ) -> RegionInfo<'a> {
        let id = self.next_id;
        self.next_id += 1;
        let (state_var, next_var, enum_name) = if id == 0 {
            (
                STATE_LOCAL.to_string(),
                NEXT_STATE_LOCAL.to_string(),
                format!("{}_StateEnum", self.machine.name),
            )
        } else {
            (
                format!("__sm_r{id}_state"),
                format!("__sm_r{id}_next"),
                format!("{}_r{id}_StateEnum", self.machine.name),
            )
        };
        let state_ty = Type::named(enum_name.clone());
        self.enums.push(TypeDef {
            body: TypeBody::Enum(EnumDef {
                name: enum_name,
                variants: states.iter().map(|s| s.name.clone()).collect(),
            }),
        });
        self.locals.push(Local { name: state_var.clone(), ty: state_ty.clone() });
        self.locals.push(Local { name: next_var.clone(), ty: state_ty });

        // next-state transition chain over this region's states.
        let chain = build_next_state_chain(states, &state_var);
        let top_level = matches!(&active, Expr::Const { lit: Literal::Bool { value: true } });

        // For a nested region, materialise the activation condition into a bool
        // local so it can be `pre`'d (the profile only allows `pre <var>`); the
        // top automaton runs every cycle and needs none.
        let active_var = if top_level {
            None
        } else {
            let av = format!("__sm_r{id}_active");
            self.locals.push(Local { name: av.clone(), ty: Type::Bool });
            self.equations.push(Equation { lhs: vec![av.clone()], rhs: active });
            Some(av)
        };

        if let Some(av) = &active_var {
            // Nested: advance only while active, else freeze; restart at the
            // initial state on (re-)entry unless this region keeps history.
            let next_rhs = Expr::if_then_else(Expr::var(av), chain, Expr::var(&state_var));
            let state_rhs = if history {
                Expr::arrow(Expr::var(initial), Expr::pre(Expr::var(&next_var)))
            } else {
                let just_entered = Expr::and(
                    Expr::var(av),
                    Expr::arrow(Expr::bool_lit(true), Expr::not(Expr::pre(Expr::var(av)))),
                );
                Expr::if_then_else(
                    just_entered,
                    Expr::var(initial),
                    Expr::arrow(Expr::var(initial), Expr::pre(Expr::var(&next_var))),
                )
            };
            self.equations.push(Equation { lhs: vec![state_var.clone()], rhs: state_rhs });
            self.equations.push(Equation { lhs: vec![next_var.clone()], rhs: next_rhs });
        } else {
            // Top automaton: identical to the flat lowering.
            self.equations.push(Equation {
                lhs: vec![state_var.clone()],
                rhs: Expr::arrow(Expr::var(initial), Expr::pre(Expr::var(&next_var))),
            });
            self.equations.push(Equation { lhs: vec![next_var.clone()], rhs: chain });
        }

        let mut nodes = Vec::with_capacity(states.len());
        for st in states {
            if !st.emits.is_empty() {
                let in_state = Expr::bin(BinOp::Eq, Expr::var(&state_var), Expr::var(&st.name));
                let emitting = match &active_var {
                    Some(av) => Expr::and(Expr::var(av), in_state),
                    None => in_state,
                };
                for sig in &st.emits {
                    self.signal_conds.entry(sig.clone()).or_default().push(emitting.clone());
                }
            }
            let mut nested = Vec::new();
            for region in &st.regions {
                let in_state = Expr::bin(BinOp::Eq, Expr::var(&state_var), Expr::var(&st.name));
                let nested_active = match &active_var {
                    Some(av) => Expr::and(Expr::var(av), in_state),
                    None => in_state,
                };
                let info = self.emit_region(
                    &region.initial_state,
                    &region.states,
                    nested_active,
                    region.history,
                );
                nested.push((region, info));
            }
            nodes.push(StateNode { def: st, nested });
        }
        RegionInfo { state_var, states: nodes }
    }

    /// The value of output `o` selected across this region's state tree:
    /// `if state = S1 then <value in S1> else if state = S2 then … else default`.
    fn value_of(&self, o: &str, region: &RegionInfo, ty: &Type) -> Expr {
        let mut chain = default_expr_for_type(ty);
        for node in region.states.iter().rev() {
            let val = self.value_in_state(o, node, ty);
            chain = Expr::if_then_else(
                Expr::bin(BinOp::Eq, Expr::var(&region.state_var), Expr::var(&node.def.name)),
                val,
                chain,
            );
        }
        chain
    }

    fn value_in_state(&self, o: &str, node: &StateNode, ty: &Type) -> Expr {
        // A nested region that drives `o` on every path takes precedence (the
        // sub-automaton refines this state's behaviour); otherwise the state's
        // own equation.
        for (rdef, rinfo) in &node.nested {
            if region_covers(o, rdef) {
                return self.value_of(o, rinfo, ty);
            }
        }
        node.def
            .equations
            .iter()
            .find(|e| e.lhs.len() == 1 && e.lhs[0] == o)
            .map(|e| e.rhs.clone())
            .unwrap_or_else(|| default_expr_for_type(ty))
    }
}

/// The next-state chain for one region's states, keyed off `state_var`.
fn build_next_state_chain(states: &[StateDef], state_var: &str) -> Expr {
    let stay = Expr::var(state_var);
    let mut chain = stay.clone();
    for s in states.iter().rev() {
        let mut inner = stay.clone();
        for t in s.transitions.iter().rev() {
            inner = Expr::if_then_else(t.guard.clone(), Expr::var(&t.target), inner);
        }
        chain = Expr::if_then_else(
            Expr::bin(BinOp::Eq, Expr::var(state_var), Expr::var(&s.name)),
            inner,
            chain,
        );
    }
    chain
}

// --- validation (operates on the raw definition tree) ------------------------

fn collect_state_names(states: &[StateDef], into: &mut Vec<String>) {
    for s in states {
        into.push(s.name.clone());
        for r in &s.regions {
            collect_state_names(&r.states, into);
        }
    }
}

/// Every state name in the tree (all regions, all depths) — the id space of
/// the graphical automaton editor's layout.
pub fn collect_state_names_of(states: &[StateDef], into: &mut Vec<String>) {
    collect_state_names(states, into);
}

/// Signals must be unique, must not shadow the machine's interface, locals,
/// or state names (states are enum variants referenced by bare name), and
/// every emission must name a declared signal.
fn validate_signals(sm: &StateMachineDef, state_names: &[String]) -> Result<(), LowerError> {
    let mut seen = std::collections::HashSet::new();
    for sig in &sm.signals {
        if !seen.insert(sig.clone()) {
            return Err(LowerError::DuplicateSignal(sm.name.clone(), sig.clone()));
        }
        let clashes = sm.inputs.iter().any(|p| &p.name == sig)
            || sm.outputs.iter().any(|p| &p.name == sig)
            || sm.locals.iter().any(|l| &l.name == sig)
            || state_names.iter().any(|n| n == sig);
        if clashes {
            return Err(LowerError::SignalClash(sm.name.clone(), sig.clone()));
        }
    }
    fn check_emits(sm: &StateMachineDef, states: &[StateDef]) -> Result<(), LowerError> {
        for st in states {
            for sig in &st.emits {
                if !sm.signals.contains(sig) {
                    return Err(LowerError::UnknownSignal(
                        sm.name.clone(),
                        st.name.clone(),
                        sig.clone(),
                    ));
                }
            }
            for r in &st.regions {
                check_emits(sm, &r.states)?;
            }
        }
        Ok(())
    }
    check_emits(sm, &sm.states)
}

fn validate_region(sm: &StateMachineDef, initial: &str, states: &[StateDef]) -> Result<(), LowerError> {
    if !states.iter().any(|s| s.name == initial) {
        return Err(LowerError::UnknownInitialState(sm.name.clone(), initial.to_string()));
    }
    for s in states {
        for t in &s.transitions {
            if !states.iter().any(|x| x.name == t.target) {
                return Err(LowerError::UnknownTarget(
                    sm.name.clone(),
                    s.name.clone(),
                    t.target.clone(),
                ));
            }
        }
        for r in &s.regions {
            validate_region(sm, &r.initial_state, &r.states)?;
        }
    }
    Ok(())
}

/// Does `state` assign `o` (directly, or via a nested region that covers it on
/// every path)?
fn state_covers(o: &str, state: &StateDef) -> bool {
    state.equations.iter().any(|e| e.lhs.len() == 1 && e.lhs[0] == o)
        || state.regions.iter().any(|r| region_covers(o, r))
}
fn region_covers(o: &str, region: &Region) -> bool {
    region.states.iter().all(|s| state_covers(o, s))
}

/// SCADE strictness: every state in the list must cover `o`, recursively, with
/// a precise error pointing at the offending state.
fn require_cover(sm: &StateMachineDef, o: &str, states: &[StateDef]) -> Result<(), LowerError> {
    for st in states {
        if st.equations.iter().any(|e| e.lhs.len() == 1 && e.lhs[0] == o) {
            continue;
        }
        if st.regions.is_empty() {
            return Err(LowerError::OutputUnassigned(sm.name.clone(), o.to_string(), st.name.clone()));
        }
        let mut covered = false;
        let mut first_err = None;
        for r in &st.regions {
            match require_cover(sm, o, &r.states) {
                Ok(()) => { covered = true; break; }
                Err(e) => { first_err.get_or_insert(e); }
            }
        }
        if !covered {
            return Err(first_err.unwrap_or_else(|| {
                LowerError::OutputUnassigned(sm.name.clone(), o.to_string(), st.name.clone())
            }));
        }
    }
    Ok(())
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
        Type::Char => Expr::Const { lit: Literal::Char { value: 0 } },
        // Compound / named types fall back to the integer-zero literal; if
        // every state assigns the output (which we require above) this branch
        // is unreachable at runtime, but lowering still has to produce
        // something type-shaped for the chain's terminal else.
        Type::Array { .. } | Type::Named { .. } => Expr::int_lit(0),
    }
}
