//! Clock elimination: rewrite a node that uses `when` / `merge` into an
//! equivalent node on the base clock only.
//!
//! Kind 2 rejects clocked local variables (`x: int when c`), so the Lustre
//! handed to the prover cannot carry the clocks the simulator and the C
//! backend execute. This pass produces the base-clock program with *exactly*
//! the held-value semantics those two backends implement (see
//! [`crate::clocks`]):
//!
//! * an equation on clock `C` becomes `x = if act_C then e else (d -> pre x)`
//!   — it updates on `C`'s active cycles and holds its value otherwise,
//!   starting from its type's default `d`;
//! * `pre x` on `C` stays `pre x`: `x` holds between ticks, so its previous
//!   base-cycle value *is* its value at `C`'s previous tick;
//! * `init -> body` on `C` becomes `if first_C then init else body`, where
//!   `first_C` is true until `C` has ticked once;
//! * `merge c a b` becomes `if c then a else b`, `e when c` becomes `e`;
//! * a stateful call on `C` becomes `condact(act_C, N(args), d₁, …)`, so the
//!   callee steps only on `C`'s active cycles (its outputs hold otherwise).
//!
//! `act_C` and `first_C` are fresh locals, one per clock. `condact` is the
//! Lustre V6 / Kind 2 conditional activation; the IR simulator understands
//! it as a built-in so the rewrite can be checked against the original node
//! cycle by cycle.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::clocks::{infer_clocks, node_uses_clocks, Clock, ClockInfo};
use crate::expr::{Expr, FieldInit, Literal};
use crate::node::{Equation, Local, NodeDef};
use crate::project::{Project, TypeBody};
use crate::types::Type;

/// The pseudo-callee name of conditional activation.
pub const CONDACT: &str = "condact";

/// Why a node could not be rewritten.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclockError {
    pub node: String,
    pub message: String,
}

impl std::fmt::Display for DeclockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "node `{}`: {}", self.node, self.message)
    }
}

/// Rewrite every clocked node of `project` onto the base clock.
pub fn declock_project(project: &Project) -> Result<Project, Vec<DeclockError>> {
    let mut out = project.clone();
    let mut errors = Vec::new();
    for pkg in &mut out.packages {
        for node in &mut pkg.nodes {
            if !node_uses_clocks(node) {
                continue;
            }
            match declock_node(node, project) {
                Ok(n) => *node = n,
                Err(e) => errors.push(e),
            }
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Rewrite one node onto the base clock. A node without `when`/`merge` is
/// returned unchanged.
pub fn declock_node(node: &NodeDef, project: &Project) -> Result<NodeDef, DeclockError> {
    if !node_uses_clocks(node) {
        return Ok(node.clone());
    }
    let info = infer_clocks(node);
    if let Some(e) = info.errors.first() {
        return Err(DeclockError { node: node.name.clone(), message: e.message.clone() });
    }
    let mut cx = Cx::new(node, project, &info);
    let mut equations = Vec::new();
    for (i, eq) in node.equations.iter().enumerate() {
        let clock = &info.equation_clocks[i];
        equations.push(cx.equation(eq, clock)?);
    }

    let mut out = node.clone();
    // Clock bookkeeping first so a reader sees what gates the rest.
    let mut prelude = Vec::new();
    for (key, name) in &cx.act_names {
        let (cond, parent) = match &cx.clocks[key] {
            Clock::On { clock, on, parent } => {
                let c = Expr::var(clock.clone());
                (if *on { c } else { Expr::not(c) }, parent.as_ref())
            }
            Clock::Base => unreachable!("base clock has no activation flag"),
        };
        // Parents were registered first, and their keys sort first.
        let rhs = match cx.act_names.get(&parent.key()) {
            Some(p) => Expr::and(Expr::var(p.clone()), cond),
            None => cond,
        };
        prelude.push(Equation { lhs: vec![name.clone()], rhs });
        out.locals.push(Local { name: name.clone(), ty: Type::Bool });
    }
    for (key, name) in &cx.first_names {
        let act = Expr::var(cx.act_names[key].clone());
        // True until the clock's first active cycle has passed (`pre` of
        // variables only, which every backend supports).
        let rhs = Expr::arrow(
            Expr::bool_lit(true),
            Expr::and(Expr::pre(Expr::var(name.clone())), Expr::not(Expr::pre(act))),
        );
        prelude.push(Equation { lhs: vec![name.clone()], rhs });
        out.locals.push(Local { name: name.clone(), ty: Type::Bool });
    }
    for (name, ty, rhs) in cx.holds.drain(..) {
        prelude.push(Equation { lhs: vec![name.clone()], rhs });
        out.locals.push(Local { name, ty });
    }
    prelude.extend(equations);
    out.equations = prelude;
    Ok(out)
}

struct Cx<'a> {
    node: &'a NodeDef,
    project: &'a Project,
    info: &'a ClockInfo,
    taken: HashSet<String>,
    types: HashMap<String, Type>,
    /// Clock key → clock, for every non-base clock that needs a flag.
    clocks: HashMap<String, Clock>,
    /// Clock key → activation flag name (parents sort before children).
    act_names: BTreeMap<String, String>,
    /// Clock key → first-tick flag name.
    first_names: BTreeMap<String, String>,
    /// Hold locals for `pre` of a compound operand: (name, type, rhs).
    holds: Vec<(String, Type, Expr)>,
}

impl<'a> Cx<'a> {
    fn new(node: &'a NodeDef, project: &'a Project, info: &'a ClockInfo) -> Self {
        let mut taken = HashSet::new();
        let mut types = HashMap::new();
        for p in node.inputs.iter().chain(&node.outputs) {
            taken.insert(p.name.clone());
            types.insert(p.name.clone(), p.ty.clone());
        }
        for l in &node.locals {
            taken.insert(l.name.clone());
            types.insert(l.name.clone(), l.ty.clone());
        }
        Cx {
            node,
            project,
            info,
            taken,
            types,
            clocks: HashMap::new(),
            act_names: BTreeMap::new(),
            first_names: BTreeMap::new(),
            holds: Vec::new(),
        }
    }

    fn err(&self, message: impl Into<String>) -> DeclockError {
        DeclockError { node: self.node.name.clone(), message: message.into() }
    }

    fn fresh(&mut self, stem: &str) -> String {
        let mut n = 0usize;
        loop {
            let name = format!("__{stem}{n}");
            if self.taken.insert(name.clone()) {
                return name;
            }
            n += 1;
        }
    }

    /// The activation flag of `clock` (registering it and its parents), or
    /// `None` for the base clock.
    fn act_expr(&mut self, clock: &Clock) -> Option<Expr> {
        let Clock::On { parent, .. } = clock else { return None };
        self.act_expr(parent);
        let key = clock.key();
        if !self.act_names.contains_key(&key) {
            let name = self.fresh("ck");
            self.act_names.insert(key.clone(), name);
            self.clocks.insert(key.clone(), clock.clone());
        }
        Some(Expr::var(self.act_names[&key].clone()))
    }

    fn first_expr(&mut self, clock: &Clock) -> Expr {
        self.act_expr(clock);
        let key = clock.key();
        if !self.first_names.contains_key(&key) {
            let name = self.fresh("first");
            self.first_names.insert(key.clone(), name);
        }
        Expr::var(self.first_names[&key].clone())
    }

    fn equation(&mut self, eq: &Equation, clock: &Clock) -> Result<Equation, DeclockError> {
        let rhs = self.expr(&eq.rhs)?;
        let Some(act) = self.act_expr(clock) else {
            return Ok(Equation { lhs: eq.lhs.clone(), rhs });
        };
        if eq.lhs.len() == 1 {
            let x = &eq.lhs[0];
            let ty = self.types.get(x).cloned().ok_or_else(|| self.err(format!("`{x}` is not declared")))?;
            let d = default_expr(&ty, self.project)
                .ok_or_else(|| self.err(format!("no default value for the type of `{x}`")))?;
            let hold = Expr::arrow(d, Expr::pre(Expr::var(x.clone())));
            return Ok(Equation { lhs: eq.lhs.clone(), rhs: Expr::if_then_else(act, rhs, hold) });
        }
        // Several outputs come from one call: `condact` already holds them.
        match rhs {
            Expr::Call { ref node, .. } if node == CONDACT => Ok(Equation { lhs: eq.lhs.clone(), rhs }),
            Expr::Call { node, args } => {
                let mut cargs = vec![act, Expr::call(node, args)];
                for x in &eq.lhs {
                    let ty = self.types.get(x).cloned().ok_or_else(|| self.err(format!("`{x}` is not declared")))?;
                    cargs.push(
                        default_expr(&ty, self.project)
                            .ok_or_else(|| self.err(format!("no default value for the type of `{x}`")))?,
                    );
                }
                Ok(Equation { lhs: eq.lhs.clone(), rhs: Expr::call(CONDACT, cargs) })
            }
            _ => Err(self.err("a clocked equation with several outputs must be a single call")),
        }
    }

    fn site_clock(&self, e: &Expr) -> Clock {
        self.info
            .site_clocks
            .get(&(e as *const Expr as usize))
            .cloned()
            .unwrap_or(Clock::Base)
    }

    fn expr(&mut self, e: &Expr) -> Result<Expr, DeclockError> {
        Ok(match e {
            Expr::Const { .. } | Expr::Var { .. } => e.clone(),
            Expr::When { arg, .. } => self.expr(arg)?,
            Expr::Merge { clock, on_true, on_false } => Expr::if_then_else(
                Expr::var(clock.clone()),
                self.expr(on_true)?,
                self.expr(on_false)?,
            ),
            Expr::Pre { arg } => {
                let clock = self.site_clock(e);
                let inner = self.expr(arg)?;
                match (&clock, arg.as_ref()) {
                    // Held variables: the previous base cycle is the last tick.
                    (_, Expr::Var { .. }) | (Clock::Base, _) => Expr::pre(inner),
                    (clock, _) => {
                        // `pre e` on a clock: hold `e` across inactive cycles in
                        // a fresh local, then take its previous value. The hold
                        // is never read before the clock's first tick, so its
                        // pre-first-tick value is unconstrained (`pre h`).
                        let act = self.act_expr(clock).expect("non-base clock");
                        let h = self.fresh("hold");
                        let ty = self.type_of(arg).ok_or_else(|| {
                            self.err("cannot determine the type of a clocked `pre` operand")
                        })?;
                        let rhs = Expr::if_then_else(act, inner, Expr::pre(Expr::var(h.clone())));
                        self.holds.push((h.clone(), ty, rhs));
                        Expr::pre(Expr::var(h))
                    }
                }
            }
            Expr::Arrow { init, body } => {
                let clock = self.site_clock(e);
                let (i, b) = (self.expr(init)?, self.expr(body)?);
                if clock.is_base() {
                    Expr::arrow(i, b)
                } else {
                    Expr::if_then_else(self.first_expr(&clock), i, b)
                }
            }
            Expr::Call { node, args } => {
                let clock = self
                    .info
                    .call_clocks
                    .get(&(e as *const Expr as usize))
                    .cloned()
                    .unwrap_or(Clock::Base);
                let args = args.iter().map(|a| self.expr(a)).collect::<Result<Vec<_>, _>>()?;
                let callee = self.project.find_node(node);
                let stateful = callee.is_some_and(|c| !c.is_function() && !c.is_imported());
                if clock.is_base() || !stateful {
                    Expr::call(node.clone(), args)
                } else {
                    let callee = callee.expect("stateful callee exists");
                    let act = self.act_expr(&clock).expect("non-base clock");
                    let mut cargs = vec![act, Expr::call(node.clone(), args)];
                    for p in &callee.outputs {
                        cargs.push(default_expr(&p.ty, self.project).ok_or_else(|| {
                            self.err(format!("no default value for output `{}` of `{node}`", p.name))
                        })?);
                    }
                    Expr::call(CONDACT, cargs)
                }
            }
            Expr::Unary { op, arg } => Expr::Unary { op: *op, arg: Box::new(self.expr(arg)?) },
            Expr::Binary { op, lhs, rhs } => Expr::bin(*op, self.expr(lhs)?, self.expr(rhs)?),
            Expr::IfThenElse { cond, then_branch, else_branch } => Expr::if_then_else(
                self.expr(cond)?,
                self.expr(then_branch)?,
                self.expr(else_branch)?,
            ),
            Expr::Field { base, field } => Expr::Field { base: Box::new(self.expr(base)?), field: field.clone() },
            Expr::Index { base, index } => Expr::Index {
                base: Box::new(self.expr(base)?),
                index: Box::new(self.expr(index)?),
            },
            Expr::Tuple { items } => Expr::Tuple { items: self.exprs(items)? },
            Expr::Array { items } => Expr::Array { items: self.exprs(items)? },
            Expr::Struct { ty, fields } => Expr::Struct {
                ty: ty.clone(),
                fields: fields
                    .iter()
                    .map(|f| Ok(FieldInit { field: f.field.clone(), value: self.expr(&f.value)? }))
                    .collect::<Result<Vec<_>, DeclockError>>()?,
            },
            Expr::Cast { to, arg } => Expr::cast(to.clone(), self.expr(arg)?),
            Expr::Iterate { kind, node, init, arrays } => Expr::Iterate {
                kind: *kind,
                node: node.clone(),
                init: match init {
                    Some(i) => Some(Box::new(self.expr(i)?)),
                    None => None,
                },
                arrays: self.exprs(arrays)?,
            },
        })
    }

    fn exprs(&mut self, items: &[Expr]) -> Result<Vec<Expr>, DeclockError> {
        items.iter().map(|i| self.expr(i)).collect()
    }

    /// The type of a simple operand: a variable, a sampled variable, or a
    /// field/index of one. Enough for the `pre` holds the backends accept.
    fn type_of(&self, e: &Expr) -> Option<Type> {
        match e {
            Expr::Var { name } => self.types.get(name).cloned(),
            Expr::When { arg, .. } | Expr::Pre { arg } => self.type_of(arg),
            Expr::Arrow { init, body } => self.type_of(body).or_else(|| self.type_of(init)),
            Expr::Index { base, .. } => match self.type_of(base)? {
                Type::Array { elem, .. } => Some(*elem),
                _ => None,
            },
            Expr::Const { lit } => Some(match lit {
                Literal::Bool { .. } => Type::Bool,
                Literal::Int { .. } => Type::Int32,
                Literal::Float { .. } => Type::Float64,
                Literal::Char { .. } => Type::Char,
            }),
            _ => None,
        }
    }
}

/// The default value of a type as an expression — what an inactive clocked
/// variable holds before its first activation (the simulator's and the C
/// state's zero value): `false`, `0`, `0.0`, the first enum variant, and
/// records/arrays of defaults.
pub fn default_expr(ty: &Type, project: &Project) -> Option<Expr> {
    Some(match ty {
        Type::Bool => Expr::bool_lit(false),
        Type::Float32 | Type::Float64 => Expr::Const { lit: Literal::Float { value: 0.0 } },
        Type::Char => Expr::Const { lit: Literal::Char { value: 0 } },
        t if t.is_integer() => Expr::int_lit(0),
        Type::Array { elem, len } => {
            let d = default_expr(elem, project)?;
            Expr::Array { items: vec![d; *len as usize] }
        }
        Type::Named { name } => {
            let def = project.packages.iter().find_map(|p| p.find_type(name))?;
            match &def.body {
                TypeBody::Enum(e) => Expr::var(e.variants.first()?.clone()),
                TypeBody::Record { name, fields } => Expr::Struct {
                    ty: name.clone(),
                    fields: fields
                        .iter()
                        .map(|f| Some(FieldInit { field: f.name.clone(), value: default_expr(&f.ty, project)? }))
                        .collect::<Option<Vec<_>>>()?,
                },
                TypeBody::Alias { target, .. } => default_expr(target, project)?,
            }
        }
        _ => return None,
    })
}
