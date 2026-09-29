//! Runtime-error checks: proving the model free of machine-integer overflow,
//! division by zero, out-of-bounds indexing and out-of-range real-to-integer
//! conversion — in the context of the operator being proved.
//!
//! The Kind 2 view computes over mathematical integers. That is exact for the
//! simulator and the generated C *as long as no integer value ever leaves the
//! range C gives it*, so these checks turn the "no overflow" assumption into
//! proof obligations:
//!
//! * **Operations** (`+ - * / %`, unary `-`) are checked at the width C
//!   evaluates them in: the operand type, promoted to `int` (int32) when
//!   narrower — so `a + b` on two `int8` can never overflow, `p * q` on two
//!   `int32` can, and so can `x * y` on two `uint16` (C's `int` promotion).
//! * **Stores** of a narrow value (`int8`, `uint16`, …) computed in `int` —
//!   into a variable, an argument, an array or record element, a `pre` — are
//!   checked to fit: C would silently wrap it.
//! * **Division and remainder** by zero; **indexing** outside the array;
//!   **real → integer** conversion outside the target range.
//!
//! Explicit integer casts are not checked: they wrap by definition.
//! Operations inside an `if` (or `->`) branch are checked only when the branch
//! is taken, as C evaluates them; both operands of `and` / `or` are checked,
//! as the simulator evaluates both. Calls are first hoisted to their own
//! equations — C evaluates call arguments unconditionally — so a call's
//! arguments are checked on every cycle of the call's clock, and so is a
//! `pre`'s operand (its memory is written on every tick).
//!
//! Kind 2 checks properties of the analysed node only (not of the nodes it
//! calls), so each node's checks become extra boolean outputs, passed up
//! through every call instance, and asserted as named properties at the
//! root: each check is proved for the inputs the root can actually give it.
//! The root's integer inputs are assumed to lie within their C types.
//!
//! Static interval reasoning over types, literals and constants drops the
//! checks that cannot fail (`a + b` on `int8`, `x / 1000` on `int32`, …).

use std::collections::{BTreeMap, BTreeSet};

use ol_ir::declock::CONDACT;
use ol_ir::{BinOp, Equation, Expr, Literal, Local, NodeDef, NodeKind, Port, Project, Type, UnaryOp};
use ol_typecheck::ExprTyper;

/// What a runtime-error check guards against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RteKind {
    /// An integer operation's result leaves the range C computes it in.
    Overflow,
    /// A narrow integer value is stored where it does not fit.
    Narrowing,
    DivisionByZero,
    IndexOutOfBounds,
    /// A real converted to an integer type does not fit it.
    Conversion,
}

impl RteKind {
    pub fn label(self) -> &'static str {
        match self {
            RteKind::Overflow => "overflow",
            RteKind::Narrowing => "narrowing",
            RteKind::DivisionByZero => "division by zero",
            RteKind::IndexOutOfBounds => "index out of bounds",
            RteKind::Conversion => "conversion",
        }
    }
}

/// One check, as proved at the root.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RteCheck {
    /// The Kind 2 property name (`rte1`, `rte2`, …).
    pub name: String,
    pub kind: RteKind,
    /// The operator the checked expression is in.
    pub node: String,
    /// The call instances it is reached through, from the root:
    /// `PlanRelease#1 › Candidate#3` (empty in the root itself).
    pub path: String,
    /// What must hold, in model syntax: `p * q fits int32`.
    pub what: String,
}

impl RteCheck {
    /// `overflow in PlanRelease#1 › Candidate#3: roll - droll fits int32` —
    /// the call path, or the root's own name for its own checks.
    pub fn describe(&self) -> String {
        let at = if self.path.is_empty() { &self.node } else { &self.path };
        format!("{} in {at}: {}", self.kind.label(), self.what)
    }
}

/// A node's own checks: the boolean local holding each, what it guards
/// against, and what must hold.
pub(crate) type NodeChecks = BTreeMap<String, Vec<(String, RteKind, String)>>;

fn range(t: &Type) -> Option<(i128, i128)> {
    Some(match t {
        Type::Int8 => (i8::MIN as i128, i8::MAX as i128),
        Type::Int16 => (i16::MIN as i128, i16::MAX as i128),
        Type::Int32 => (i32::MIN as i128, i32::MAX as i128),
        Type::Int64 => (i64::MIN as i128, i64::MAX as i128),
        Type::Uint8 | Type::Char => (0, u8::MAX as i128),
        Type::Uint16 => (0, u16::MAX as i128),
        Type::Uint32 => (0, u32::MAX as i128),
        Type::Uint64 => (0, u64::MAX as i128),
        _ => return None,
    })
}

/// The type C evaluates an operation on `t` in (the usual promotions).
fn promoted(t: &Type) -> Type {
    if is_narrow(t) {
        Type::Int32
    } else {
        t.clone()
    }
}

fn is_narrow(t: &Type) -> bool {
    matches!(t, Type::Int8 | Type::Int16 | Type::Uint8 | Type::Uint16 | Type::Char)
}

fn type_name(t: &Type) -> &'static str {
    match t {
        Type::Int8 => "int8",
        Type::Int16 => "int16",
        Type::Int32 => "int32",
        Type::Int64 => "int64",
        Type::Uint8 => "uint8",
        Type::Uint16 => "uint16",
        Type::Uint32 => "uint32",
        Type::Uint64 => "uint64",
        Type::Char => "char",
        _ => "?",
    }
}

fn int_lit(v: i128) -> Expr {
    if v < 0 {
        Expr::neg(int_lit(-v))
    } else if v > i64::MAX as i128 {
        // uint64's top half has no i64 literal: write it as a sum.
        Expr::bin(BinOp::Add, Expr::int_lit(i64::MAX), int_lit(v - i64::MAX as i128))
    } else {
        Expr::int_lit(v as i64)
    }
}

fn real_lit(v: f64) -> Expr {
    if v < 0.0 {
        Expr::neg(real_lit(-v))
    } else {
        Expr::Const { lit: Literal::Float { value: v } }
    }
}

fn within(e: &Expr, (lo, hi): (i128, i128)) -> Expr {
    Expr::and(Expr::bin(BinOp::Le, int_lit(lo), e.clone()), Expr::bin(BinOp::Le, e.clone(), int_lit(hi)))
}

/// An expression in model syntax, shortened for a report line.
fn show(e: &Expr) -> String {
    let s = ol_lustre_emit::format_expr(e);
    if s.chars().count() > 72 {
        format!("{}…", s.chars().take(71).collect::<String>())
    } else {
        s
    }
}

/// Integer constants by name, for interval reasoning.
fn integer_constants(project: &Project) -> BTreeMap<String, i128> {
    fn value(e: &Expr) -> Option<i128> {
        match e {
            Expr::Const { lit: Literal::Int { value } } => Some(*value as i128),
            Expr::Unary { op: UnaryOp::Neg, arg } => value(arg).map(|v| -v),
            _ => None,
        }
    }
    project.packages.iter().flat_map(|p| &p.constants).filter_map(|c| Some((c.name.clone(), value(&c.value)?))).collect()
}

/// Instrument every node of `project` (clock-free, iterators unrolled) with
/// its runtime-error checks: calls hoisted to equations, one boolean local
/// per check. Returns each node's checks.
pub(crate) fn instrument(project: &mut Project) -> NodeChecks {
    let typer = ExprTyper::new(project);
    let consts = integer_constants(project);
    let snapshot = project.clone();
    let mut out = NodeChecks::new();
    for pkg in &mut project.packages {
        for node in &mut pkg.nodes {
            if node.kind == NodeKind::Imported {
                continue;
            }
            let mut cx = NodeRte::new(&typer, &snapshot, &consts, node.clone());
            cx.run();
            *node = cx.node;
            out.insert(node.name.clone(), cx.checks);
        }
    }
    out
}

/// A condition an expression is evaluated under; `clock` when it is a clock
/// (a `pre`'s memory, too, is written only then).
#[derive(Clone)]
struct Guard {
    cond: Expr,
    clock: bool,
}

struct NodeRte<'a> {
    typer: &'a ExprTyper,
    project: &'a Project,
    consts: &'a BTreeMap<String, i128>,
    node: NodeDef,
    env: BTreeMap<String, Type>,
    taken: BTreeSet<String>,
    checks: Vec<(String, RteKind, String)>,
    new_eqs: Vec<Equation>,
}

impl<'a> NodeRte<'a> {
    fn new(typer: &'a ExprTyper, project: &'a Project, consts: &'a BTreeMap<String, i128>, node: NodeDef) -> Self {
        let env = ExprTyper::env(&node);
        let taken = env.keys().cloned().collect();
        NodeRte { typer, project, consts, node, env, taken, checks: vec![], new_eqs: vec![] }
    }

    fn run(&mut self) {
        // 1. Calls to their own equations.
        let mut eqs = std::mem::take(&mut self.node.equations);
        for eq in &mut eqs {
            self.hoist(&mut eq.rhs, true, &[]);
        }
        eqs.append(&mut self.new_eqs);
        // 2. Every operation, under the conditions C evaluates it under, and
        //    every store of a narrow value.
        for eq in &eqs {
            self.walk(&eq.rhs, &[]);
            match (&eq.rhs, eq.lhs.as_slice()) {
                (Expr::Tuple { items }, lhs) if items.len() == lhs.len() => {
                    for (x, e) in lhs.iter().zip(items) {
                        self.store(e, Some(x), &[]);
                    }
                }
                (e, [x]) => self.store(e, Some(x), &[]),
                _ => {}
            }
        }
        eqs.append(&mut self.new_eqs);
        self.node.equations = eqs;
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

    fn type_of(&self, e: &Expr) -> Option<Type> {
        self.typer.type_of(&self.node, &self.env, e)
    }

    fn int_type(&self, e: &Expr) -> Option<Type> {
        self.type_of(e).filter(|t| range(t).is_some())
    }

    /// A sound interval for an integer expression, from literals, constants
    /// and declared types; `None` when not an integer. Every stored value is
    /// checked to fit its type, and every operation to fit the type C
    /// computes it in, so both bound what the operation's users see.
    fn interval(&self, e: &Expr) -> Option<(i128, i128)> {
        let raw = self.raw_interval(e);
        let is_op = matches!(e, Expr::Unary { op: UnaryOp::Neg, .. })
            || matches!(e, Expr::Binary { op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod, .. });
        if !is_op {
            return raw;
        }
        // An operation is checked to fit the type C computes it in.
        match (raw, self.int_type(e).and_then(|t| range(&promoted(&t)))) {
            (Some(c), Some(k)) => Some((c.0.max(k.0), c.1.min(k.1))),
            (c, k) => c.or(k),
        }
    }

    /// [`Self::interval`] of the operation itself, before its own check.
    fn raw_interval(&self, e: &Expr) -> Option<(i128, i128)> {
        let ty = || self.int_type(e).and_then(|t| range(&t));
        let abs_max = |(l, h): (i128, i128)| l.abs().max(h.abs());
        match e {
            Expr::Const { lit: Literal::Int { value } } => Some((*value as i128, *value as i128)),
            Expr::Const { lit: Literal::Char { value } } => Some((*value as i128, *value as i128)),
            Expr::Var { name } if !self.env.contains_key(name) && self.consts.contains_key(name) => {
                self.consts.get(name).map(|v| (*v, *v))
            }
            Expr::Unary { op: UnaryOp::Neg, arg } => self.interval(arg).map(|(l, h)| (-h, -l)),
            Expr::Binary { op, lhs, rhs }
                if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod) =>
            {
                let (Some(a), Some(b)) = (self.interval(lhs), self.interval(rhs)) else { return ty() };
                let corners = |f: &dyn Fn(i128, i128) -> i128| {
                    let p = [f(a.0, b.0), f(a.0, b.1), f(a.1, b.0), f(a.1, b.1)];
                    (*p.iter().min().expect("4"), *p.iter().max().expect("4"))
                };
                Some(match op {
                    BinOp::Add => (a.0 + b.0, a.1 + b.1),
                    BinOp::Sub => (a.0 - b.1, a.1 - b.0),
                    BinOp::Mul => corners(&|x, y| x * y),
                    // Truncating division is monotone in each operand on a
                    // divisor range of one sign: the corners bound it.
                    BinOp::Div if b.0 > 0 || b.1 < 0 => corners(&|x, y| x / y),
                    // Otherwise |a / b| <= |a| (a zero divisor is a check of
                    // its own).
                    BinOp::Div => (-abs_max(a), abs_max(a)),
                    // C's remainder takes the dividend's sign; |r| < |b|.
                    _ => {
                        let m = abs_max(a).min((abs_max(b) - 1).max(0));
                        (if a.0 < 0 { -m } else { 0 }, if a.1 > 0 { m } else { 0 })
                    }
                })
            }
            Expr::IfThenElse { then_branch: x, else_branch: y, .. } | Expr::Arrow { init: x, body: y } => {
                match (self.interval(x), self.interval(y)) {
                    (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
                    _ => ty(),
                }
            }
            _ => ty(),
        }
    }

    fn fits(&self, e: &Expr, r: (i128, i128)) -> bool {
        self.interval(e).is_some_and(|(l, h)| r.0 <= l && h <= r.1)
    }

    fn add_check(&mut self, guards: &[Guard], cond: Expr, kind: RteKind, what: String) {
        let body = guards.iter().rev().fold(cond, |acc, g| Expr::implies(g.cond.clone(), acc));
        let local = self.fresh("rte");
        self.env.insert(local.clone(), Type::Bool);
        self.node.locals.push(Local { name: local.clone(), ty: Type::Bool });
        self.new_eqs.push(Equation { lhs: vec![local.clone()], rhs: body });
        self.checks.push((local, kind, what));
    }

    /// Hoist every call nested inside a larger expression into its own
    /// equation — C evaluates call arguments unconditionally, and a call at
    /// equation level is where its checks are passed up. A call under a
    /// clock stays under it (`condact`).
    fn hoist(&mut self, e: &mut Expr, top: bool, clocks: &[Expr]) {
        match e {
            Expr::IfThenElse { cond, then_branch, else_branch } => {
                self.hoist(cond, false, clocks);
                let mut inner = clocks.to_vec();
                if is_clock(cond) {
                    inner.push((**cond).clone());
                }
                self.hoist(then_branch, false, &inner);
                self.hoist(else_branch, false, clocks);
            }
            Expr::Call { node, args } if node == CONDACT => {
                let mut inner = clocks.to_vec();
                if let Some(act) = args.first_mut() {
                    self.hoist(act, false, clocks);
                    inner.push(act.clone());
                }
                for (i, a) in args.iter_mut().enumerate().skip(1) {
                    match a {
                        Expr::Call { args: call_args, .. } if i == 1 => {
                            for x in call_args.iter_mut() {
                                self.hoist(x, false, &inner);
                            }
                        }
                        _ => self.hoist(a, false, clocks),
                    }
                }
                if !top {
                    self.bind(e);
                }
            }
            Expr::Call { node, args } => {
                let user = self.project.find_node(node).is_some();
                for a in args.iter_mut() {
                    self.hoist(a, false, clocks);
                }
                if !top && user {
                    if !clocks.is_empty() {
                        let default = self.type_of(e).and_then(|t| ol_ir::default_expr(&t, self.project));
                        let Some(d) = default else { return };
                        let act = clocks.iter().cloned().reduce(Expr::and).expect("a clock");
                        let call = std::mem::replace(e, Expr::bool_lit(false));
                        *e = Expr::call(CONDACT, vec![act, call, d]);
                    }
                    self.bind(e);
                }
            }
            other => other.for_each_child_mut(&mut |k| self.hoist(k, false, clocks)),
        }
    }

    fn bind(&mut self, e: &mut Expr) {
        let Some(ty) = self.type_of(e) else { return };
        let v = self.fresh("rtc");
        self.env.insert(v.clone(), ty.clone());
        self.node.locals.push(Local { name: v.clone(), ty });
        let call = std::mem::replace(e, Expr::var(v.clone()));
        self.new_eqs.push(Equation { lhs: vec![v], rhs: call });
    }

    /// A narrow value stored (into `into`, or an argument, element, memory):
    /// it must fit its type.
    fn store(&mut self, e: &Expr, into: Option<&str>, guards: &[Guard]) {
        let Some(t) = self.int_type(e) else { return };
        if !is_narrow(&t) {
            return;
        }
        let r = range(&t).expect("narrow is integer");
        if self.fits(e, r) {
            return;
        }
        let (subject, what) = match into {
            Some(x) => (Expr::var(x), format!("{x} = {} fits {}", show(e), type_name(&t))),
            None => (e.clone(), format!("{} fits {}", show(e), type_name(&t))),
        };
        self.add_check(guards, within(&subject, r), RteKind::Narrowing, what);
    }

    /// Check every operation of `e`, each under the conditions under which C
    /// evaluates it.
    fn walk(&mut self, e: &Expr, guards: &[Guard]) {
        let with = |cond: Expr, clock: bool| {
            let mut v = guards.to_vec();
            v.push(Guard { cond, clock });
            v
        };
        let clocks = || guards.iter().filter(|g| g.clock).cloned().collect::<Vec<_>>();
        match e {
            Expr::IfThenElse { cond, then_branch, else_branch } => {
                self.walk(cond, guards);
                self.walk(then_branch, &with((**cond).clone(), is_clock(cond)));
                self.walk(else_branch, &with(Expr::not((**cond).clone()), false));
                return;
            }
            Expr::Arrow { init, body } => {
                let first = Expr::arrow(Expr::bool_lit(true), Expr::bool_lit(false));
                self.walk(init, &with(first.clone(), false));
                self.walk(body, &with(Expr::not(first), false));
                return;
            }
            // A `pre`'s memory is written on every tick of its clock.
            Expr::Pre { arg } => {
                let clocks = clocks();
                self.walk(arg, &clocks);
                self.store(arg, None, &clocks);
                return;
            }
            // A hoisted call's arguments are evaluated on every tick of its
            // clock.
            Expr::Call { node, args } if node == CONDACT => {
                if let [act, call, defaults @ ..] = args.as_slice() {
                    self.walk(act, guards);
                    let mut inner = clocks();
                    inner.push(Guard { cond: act.clone(), clock: true });
                    self.walk(call, &inner);
                    for d in defaults {
                        self.walk(d, guards);
                    }
                }
                return;
            }
            _ => {}
        }
        match e {
            Expr::Binary { op: op @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod), lhs, rhs } => {
                if let Some(t) = self.int_type(e) {
                    let mut guards = guards.to_vec();
                    if matches!(op, BinOp::Div | BinOp::Mod) && self.interval(rhs).is_none_or(|(l, h)| l <= 0 && 0 <= h) {
                        let nonzero = Expr::bin(BinOp::Neq, (**rhs).clone(), Expr::int_lit(0));
                        let what = format!("{} <> 0 in {}", show(rhs), show(e));
                        self.add_check(&guards, nonzero.clone(), RteKind::DivisionByZero, what);
                        guards.push(Guard { cond: nonzero, clock: false });
                    }
                    let w = promoted(&t);
                    let r = range(&w).expect("integer");
                    // `a % b` overflows exactly when `a / b` does (MIN % -1).
                    let quotient = Expr::bin(BinOp::Div, (**lhs).clone(), (**rhs).clone());
                    let subject = if *op == BinOp::Mod { &quotient } else { e };
                    if !self.raw_interval(subject).is_some_and(|(l, h)| r.0 <= l && h <= r.1) {
                        let what = format!("{} fits {}", show(e), type_name(&w));
                        self.add_check(&guards, within(subject, r), RteKind::Overflow, what);
                    }
                }
            }
            Expr::Unary { op: UnaryOp::Neg, .. } => {
                if let Some(t) = self.int_type(e) {
                    let w = promoted(&t);
                    let r = range(&w).expect("integer");
                    if !self.raw_interval(e).is_some_and(|(l, h)| r.0 <= l && h <= r.1) {
                        self.add_check(guards, within(e, r), RteKind::Overflow, format!("{} fits {}", show(e), type_name(&w)));
                    }
                }
            }
            Expr::Index { base, index } => {
                if let Some(Type::Array { len, .. }) = self.type_of(base) {
                    let r = (0, len as i128 - 1);
                    if !self.fits(index, r) {
                        let what = format!("{} indexes {} (0..{})", show(index), show(base), len.saturating_sub(1));
                        self.add_check(guards, within(index, r), RteKind::IndexOutOfBounds, what);
                    }
                }
            }
            Expr::Cast { to, arg } => {
                let to = self.typer.resolve(to);
                if let (Some((lo, hi)), true) = (range(&to), self.type_of(arg).is_some_and(|t| t.is_float())) {
                    let cond = Expr::and(
                        Expr::bin(BinOp::Lt, real_lit(lo as f64 - 1.0), (**arg).clone()),
                        Expr::bin(BinOp::Lt, (**arg).clone(), real_lit(hi as f64 + 1.0)),
                    );
                    let what = format!("{} converts to {}", show(arg), type_name(&to));
                    self.add_check(guards, cond, RteKind::Conversion, what);
                }
            }
            // Elements, fields and arguments are stored at their type.
            Expr::Array { items } => {
                for i in items {
                    self.store(i, None, guards);
                }
            }
            Expr::Struct { fields, .. } => {
                for f in fields {
                    self.store(&f.value, None, guards);
                }
            }
            Expr::Call { node, args } if self.project.find_node(node).is_some() => {
                for a in args {
                    self.store(a, None, guards);
                }
            }
            _ => {}
        }
        let mut kids: Vec<&Expr> = Vec::new();
        children(e, &mut kids);
        for k in kids {
            self.walk(k, guards);
        }
    }
}

/// A clock condition introduced by clock elimination.
fn is_clock(e: &Expr) -> bool {
    matches!(e, Expr::Var { name } if name.starts_with("__ck"))
}

fn children<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match e {
        Expr::Const { .. } | Expr::Var { .. } => {}
        Expr::Unary { arg, .. } | Expr::Pre { arg } | Expr::Cast { arg, .. } | Expr::When { arg, .. } => out.push(arg),
        Expr::Field { base, .. } => out.push(base),
        Expr::Binary { lhs, rhs, .. } => {
            out.push(lhs);
            out.push(rhs);
        }
        Expr::Arrow { init, body } => {
            out.push(init);
            out.push(body);
        }
        Expr::IfThenElse { cond, then_branch, else_branch } => {
            out.push(cond);
            out.push(then_branch);
            out.push(else_branch);
        }
        Expr::Merge { on_true, on_false, .. } => {
            out.push(on_true);
            out.push(on_false);
        }
        Expr::Index { base, index } => {
            out.push(base);
            out.push(index);
        }
        Expr::Call { args, .. } | Expr::Tuple { items: args } | Expr::Array { items: args } => out.extend(args.iter()),
        Expr::Struct { fields, .. } => out.extend(fields.iter().map(|f| &f.value)),
        Expr::Iterate { init, arrays, .. } => {
            if let Some(i) = init {
                out.push(i);
            }
            out.extend(arrays.iter());
        }
    }
}

/// A check on its way up to the root.
struct Pending {
    local: String,
    kind: RteKind,
    node: String,
    path: Vec<String>,
    what: String,
}

/// Pass every node's checks up to `root` as extra outputs of each call
/// instance (after its own outputs, so contract imports still name the
/// right ones). Returns the root's checks — the local holding each, and the
/// check as reported — in property order.
pub(crate) fn propagate(project: &mut Project, root: &str, local: &NodeChecks) -> Vec<(String, RteCheck)> {
    let mut done: BTreeMap<String, Vec<Pending>> = BTreeMap::new();
    for name in callee_first(project, root) {
        let Some((pi, ni)) = find(project, &name) else { continue };
        let node = &mut project.packages[pi].nodes[ni];
        let mut mine: Vec<Pending> = local
            .get(&name)
            .into_iter()
            .flatten()
            .map(|(l, k, w)| Pending { local: l.clone(), kind: *k, node: name.clone(), path: vec![], what: w.clone() })
            .collect();
        let mut taken: BTreeSet<String> = ExprTyper::env(node).keys().cloned().collect();
        let mut instances: BTreeMap<String, usize> = BTreeMap::new();
        let mut new_locals = Vec::new();
        for eq in &mut node.equations {
            let (callee, clocked) = match &eq.rhs {
                Expr::Call { node: c, args } if c == CONDACT => match args.get(1) {
                    Some(Expr::Call { node: inner, .. }) => (inner.clone(), true),
                    _ => continue,
                },
                Expr::Call { node: c, .. } => (c.clone(), false),
                _ => continue,
            };
            let k = instances.entry(callee.clone()).or_insert(0);
            *k += 1;
            let Some(sub) = done.get(&callee).filter(|s| !s.is_empty()) else { continue };
            let tag = format!("{callee}#{k}");
            for c in sub {
                let mut n = 0usize;
                let fresh = loop {
                    let f = format!("__rtp{n}");
                    if taken.insert(f.clone()) {
                        break f;
                    }
                    n += 1;
                };
                eq.lhs.push(fresh.clone());
                // Between ticks a clocked call's checks hold vacuously.
                if clocked {
                    if let Expr::Call { args, .. } = &mut eq.rhs {
                        args.push(Expr::bool_lit(true));
                    }
                }
                new_locals.push(Local { name: fresh.clone(), ty: Type::Bool });
                let mut path = vec![tag.clone()];
                path.extend(c.path.iter().cloned());
                mine.push(Pending { local: fresh, kind: c.kind, node: c.node.clone(), path, what: c.what.clone() });
            }
        }
        node.locals.extend(new_locals);
        if name != root && !mine.is_empty() {
            let names: BTreeSet<&str> = mine.iter().map(|c| c.local.as_str()).collect();
            node.locals.retain(|l| !names.contains(l.name.as_str()));
            node.outputs.extend(mine.iter().map(|c| Port { name: c.local.clone(), ty: Type::Bool }));
        }
        done.insert(name, mine);
    }
    done.remove(root)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let check = RteCheck {
                name: format!("rte{}", i + 1),
                kind: c.kind,
                node: c.node,
                path: c.path.join(" › "),
                what: c.what,
            };
            (c.local, check)
        })
        .collect()
}

fn find(project: &Project, name: &str) -> Option<(usize, usize)> {
    project
        .packages
        .iter()
        .enumerate()
        .find_map(|(pi, p)| p.nodes.iter().position(|n| n.name == name).map(|ni| (pi, ni)))
}

/// The nodes `root` reaches, every callee before its callers.
fn callee_first(project: &Project, root: &str) -> Vec<String> {
    fn visit(name: &str, project: &Project, seen: &mut BTreeSet<String>, out: &mut Vec<String>) {
        if !seen.insert(name.to_string()) {
            return;
        }
        let Some(n) = project.find_node(name) else { return };
        let mut callees = Vec::new();
        for eq in &n.equations {
            eq.rhs.visit(|e| {
                if let Expr::Call { node, .. } = e {
                    if node != CONDACT {
                        callees.push(node.clone());
                    }
                }
            });
        }
        for c in callees {
            visit(&c, project, seen, out);
        }
        out.push(name.to_string());
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    visit(root, project, &mut seen, &mut out);
    out
}

/// The root's integer inputs lie within their C types: one expression each
/// (per element for arrays), for `assert`.
pub(crate) fn input_ranges(root: &NodeDef, typer: &ExprTyper) -> Vec<Expr> {
    let mut out = Vec::new();
    for p in &root.inputs {
        match typer.resolve(&p.ty) {
            Type::Array { elem, len } => {
                if let Some(r) = range(&typer.resolve(&elem)) {
                    for i in 0..len {
                        let at = Expr::Index {
                            base: Box::new(Expr::var(p.name.clone())),
                            index: Box::new(Expr::int_lit(i as i64)),
                        };
                        out.push(within(&at, r));
                    }
                }
            }
            t => {
                if let Some(r) = range(&t) {
                    out.push(within(&Expr::var(p.name.clone()), r));
                }
            }
        }
    }
    out
}
