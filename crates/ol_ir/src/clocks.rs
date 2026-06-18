//! Clock inference for `when` / `merge` — shared by the typechecker, the IR
//! simulator, and the C emitter so all three agree on which cycles every
//! equation runs.
//!
//! The model: every expression runs on a clock — the base clock (every
//! cycle) or a chain of boolean conditions `base when c [when not d …]`.
//! Clock conditions are variable names. Inputs and outputs are always on
//! the base clock; a local's clock is inferred from its defining equation.
//! `e when c` moves a stream one level down (present only on `c`'s true
//! cycles); `merge(c, a, b)` joins two complementary streams back up.
//!
//! Inactive cycles hold the previous value of a clocked variable — the
//! deterministic "every signal has exactly one value per step" trace the
//! simulator and the generated C both produce. Reads still cannot cross
//! clocks: the checker rejects mixing streams on different clocks, so held
//! values are never consumed where Lustre would see absence.

use std::collections::HashMap;

use crate::expr::Expr;
use crate::node::NodeDef;

/// A clock: the base clock, or a parent clock sampled by a boolean variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clock {
    Base,
    On {
        clock: String,
        on: bool,
        parent: Box<Clock>,
    },
}

impl Clock {
    pub fn on(clock: &str, polarity: bool, parent: Clock) -> Clock {
        Clock::On {
            clock: clock.to_string(),
            on: polarity,
            parent: Box::new(parent),
        }
    }

    pub fn is_base(&self) -> bool {
        matches!(self, Clock::Base)
    }

    /// Stable identity for state keys: `base`, `base/c+`, `base/c+/d-` …
    pub fn key(&self) -> String {
        match self {
            Clock::Base => "base".to_string(),
            Clock::On { clock, on, parent } => {
                format!("{}/{clock}{}", parent.key(), if *on { "+" } else { "-" })
            }
        }
    }

    /// The boolean tests from the base outward: `[(var, polarity), …]`.
    pub fn conditions(&self) -> Vec<(String, bool)> {
        match self {
            Clock::Base => Vec::new(),
            Clock::On { clock, on, parent } => {
                let mut v = parent.conditions();
                v.push((clock.clone(), *on));
                v
            }
        }
    }

    /// Human-readable form for diagnostics: `the base clock`,
    /// `clock \`when c\``, `clock \`when c when not d\``.
    pub fn describe(&self) -> String {
        let conds = self.conditions();
        if conds.is_empty() {
            return "the base clock".to_string();
        }
        let parts: Vec<String> = conds
            .iter()
            .map(|(v, on)| {
                if *on {
                    format!("when {v}")
                } else {
                    format!("when not {v}")
                }
            })
            .collect();
        format!("clock `{}`", parts.join(" "))
    }
}

/// A clock-discipline violation, pinned to an equation when possible.
#[derive(Debug, Clone)]
pub struct ClockError {
    pub equation: Option<usize>,
    pub message: String,
}

/// Everything a backend needs to execute a node's clocks faithfully.
#[derive(Debug, Default)]
pub struct ClockInfo {
    /// One clock per equation, in declaration order. An equation runs (and
    /// its lhs updates) only on its clock's active cycles.
    pub equation_clocks: Vec<Clock>,
    /// Clock of every `pre`/`->` site, keyed by the expression's address in
    /// the IR. Temporal operators count cycles of *their* clock, not the
    /// base clock — a clocked `init -> body` takes `init` on its first tick.
    pub site_clocks: HashMap<usize, Clock>,
    /// Clock of every call site, keyed the same way. Stateful callees step
    /// once per active cycle of this clock.
    pub call_clocks: HashMap<usize, Clock>,
    /// The distinct non-base clocks that host `pre`/`->` sites — the chains
    /// whose tick counts a backend must track. Parents precede children.
    pub chains: Vec<Clock>,
    pub errors: Vec<ClockError>,
}

impl Default for Clock {
    fn default() -> Self {
        Clock::Base
    }
}

/// True if the node uses `when`/`merge` anywhere (cheap syntactic test).
pub fn node_uses_clocks(node: &NodeDef) -> bool {
    let mut found = false;
    for eq in &node.equations {
        eq.rhs.visit(|e| {
            if matches!(e, Expr::When { .. } | Expr::Merge { .. }) {
                found = true;
            }
        });
    }
    found
}

/// Infer and check the clocks of one node. Never fails: discipline
/// violations land in [`ClockInfo::errors`] and the offending expressions
/// fall back to deterministic clocks, so the simulator stays runnable while
/// the diagnostics are red.
pub fn infer_clocks(node: &NodeDef) -> ClockInfo {
    let mut info = ClockInfo::default();

    // Phase 1 — natural clocks for every declared variable. Inputs and
    // outputs are base-clocked by definition; locals take the clock of
    // their defining equation, found by fixpoint since equations may
    // reference each other in any order.
    let mut var_clocks: HashMap<String, Clock> = HashMap::new();
    for p in node.inputs.iter().chain(node.outputs.iter()) {
        var_clocks.insert(p.name.clone(), Clock::Base);
    }
    let declared_io: std::collections::HashSet<&str> = node
        .inputs
        .iter()
        .map(|p| p.name.as_str())
        .chain(node.outputs.iter().map(|p| p.name.as_str()))
        .collect();
    for _ in 0..=node.equations.len() {
        let mut changed = false;
        for eq in &node.equations {
            if let Some(ck) = natural_clock(&eq.rhs, &var_clocks) {
                for l in &eq.lhs {
                    if !declared_io.contains(l.as_str())
                        && var_clocks.get(l) != Some(&ck)
                    {
                        var_clocks.insert(l.clone(), ck.clone());
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Phase 2 — per equation: the equation's clock is its rhs's natural
    // clock; every lhs must live on it, and the rhs must be internally
    // clock-consistent (checked top-down).
    for (i, eq) in node.equations.iter().enumerate() {
        let eq_clock = natural_clock(&eq.rhs, &var_clocks).unwrap_or(Clock::Base);
        for l in &eq.lhs {
            if let Some(lck) = var_clocks.get(l) {
                if *lck != eq_clock {
                    info.errors.push(ClockError {
                        equation: Some(i),
                        message: format!(
                            "`{l}` is on {} but its defining equation runs on {}{}",
                            lck.describe(),
                            eq_clock.describe(),
                            if declared_io.contains(l.as_str()) {
                                " — inputs and outputs are always base-clocked; \
                                 use merge(...) to bring the stream back up"
                            } else {
                                ""
                            }
                        ),
                    });
                }
            }
        }
        check_expr(&eq.rhs, &eq_clock, &var_clocks, i, &mut info);
        info.equation_clocks.push(eq_clock);
    }

    // The chains a backend must count ticks for: clocks hosting pre/-> sites.
    let mut chains: Vec<Clock> = Vec::new();
    for ck in info.site_clocks.values() {
        if !ck.is_base() && !chains.contains(ck) {
            chains.push(ck.clone());
        }
    }
    // Deterministic order, parents before children: a parent's key is a
    // strict prefix of its children's keys, so lexicographic sort works.
    chains.sort_by_key(|c| c.key());
    info.chains = chains;
    info
}

/// The clock an expression naturally produces, when determinable without
/// full checking: `when` wraps its argument's clock, `merge` returns its
/// clock variable's clock, everything else takes the first determined
/// child. `None` means clock-polymorphic (constants, unknown names).
fn natural_clock(expr: &Expr, var_clocks: &HashMap<String, Clock>) -> Option<Clock> {
    match expr {
        Expr::Var { name } => var_clocks.get(name).cloned(),
        Expr::Const { .. } => None,
        Expr::When { arg, clock, on } => {
            let parent = natural_clock(arg, var_clocks)
                .or_else(|| var_clocks.get(clock).cloned())
                .unwrap_or(Clock::Base);
            Some(Clock::on(clock, *on, parent))
        }
        Expr::Merge { clock, on_true, on_false } => {
            var_clocks.get(clock).cloned().or_else(|| {
                // Unknown clock variable: derive from a branch by stripping
                // one sampling level, so inference still converges.
                for b in [on_true, on_false] {
                    if let Some(Clock::On { parent, .. }) = natural_clock(b, var_clocks) {
                        return Some(*parent);
                    }
                }
                None
            })
        }
        Expr::Unary { arg, .. } | Expr::Pre { arg } | Expr::Cast { arg, .. } => {
            natural_clock(arg, var_clocks)
        }
        Expr::Field { base, .. } => natural_clock(base, var_clocks),
        Expr::Binary { lhs, rhs, .. } | Expr::Arrow { init: lhs, body: rhs } => {
            natural_clock(lhs, var_clocks).or_else(|| natural_clock(rhs, var_clocks))
        }
        Expr::IfThenElse { cond, then_branch, else_branch } => {
            natural_clock(cond, var_clocks)
                .or_else(|| natural_clock(then_branch, var_clocks))
                .or_else(|| natural_clock(else_branch, var_clocks))
        }
        Expr::Index { base, index } => {
            natural_clock(base, var_clocks).or_else(|| natural_clock(index, var_clocks))
        }
        Expr::Call { args, .. } => {
            args.iter().find_map(|a| natural_clock(a, var_clocks))
        }
        Expr::Tuple { items } | Expr::Array { items } => {
            items.iter().find_map(|i| natural_clock(i, var_clocks))
        }
        Expr::Struct { fields, .. } => {
            fields.iter().find_map(|fi| natural_clock(&fi.value, var_clocks))
        }
        Expr::Iterate { init, arrays, .. } => init
            .as_deref()
            .and_then(|i| natural_clock(i, var_clocks))
            .or_else(|| arrays.iter().find_map(|a| natural_clock(a, var_clocks))),
        Expr::Intrinsic { args, .. } => {
            args.iter().find_map(|a| natural_clock(a, var_clocks))
        }
    }
}

/// Top-down check: `expr` must run on `expected`. Records the clock of every
/// temporal and call site on the way down.
fn check_expr(
    expr: &Expr,
    expected: &Clock,
    var_clocks: &HashMap<String, Clock>,
    eq: usize,
    info: &mut ClockInfo,
) {
    match expr {
        Expr::Const { .. } => {}
        Expr::Var { name } => {
            // Unknown names (constants, enum variants, unbound pins) are
            // clock-polymorphic; the typechecker already reports unknowns.
            if let Some(ck) = var_clocks.get(name) {
                if ck != expected {
                    info.errors.push(ClockError {
                        equation: Some(eq),
                        message: format!(
                            "`{name}` is on {} but is used here on {} — sample it \
                             with `when` or join it with `merge` first",
                            ck.describe(),
                            expected.describe()
                        ),
                    });
                }
            }
        }
        Expr::When { arg, clock, on } => {
            match expected {
                Clock::On { clock: c, on: o, parent } if c == clock && o == on => {
                    check_clock_var(clock, parent, var_clocks, eq, info);
                    check_expr(arg, parent, var_clocks, eq, info);
                }
                other => {
                    info.errors.push(ClockError {
                        equation: Some(eq),
                        message: format!(
                            "`when {}{clock}` produces a sampled stream, but this \
                             position expects {} — every operand of an operator \
                             must be on the same clock",
                            if *on { "" } else { "not " },
                            other.describe()
                        ),
                    });
                    // Recover: check the argument on its own natural parent.
                    let parent = natural_clock(arg, var_clocks).unwrap_or(Clock::Base);
                    check_expr(arg, &parent, var_clocks, eq, info);
                }
            }
        }
        Expr::Merge { clock, on_true, on_false } => {
            check_clock_var(clock, expected, var_clocks, eq, info);
            check_expr(on_true, &Clock::on(clock, true, expected.clone()), var_clocks, eq, info);
            check_expr(
                on_false,
                &Clock::on(clock, false, expected.clone()),
                var_clocks,
                eq,
                info,
            );
        }
        Expr::Pre { arg } => {
            info.site_clocks.insert(expr as *const Expr as usize, expected.clone());
            check_expr(arg, expected, var_clocks, eq, info);
        }
        Expr::Arrow { init, body } => {
            info.site_clocks.insert(expr as *const Expr as usize, expected.clone());
            check_expr(init, expected, var_clocks, eq, info);
            check_expr(body, expected, var_clocks, eq, info);
        }
        Expr::Call { args, .. } => {
            info.call_clocks.insert(expr as *const Expr as usize, expected.clone());
            for a in args {
                check_expr(a, expected, var_clocks, eq, info);
            }
        }
        Expr::Unary { arg, .. } | Expr::Cast { arg, .. } => {
            check_expr(arg, expected, var_clocks, eq, info)
        }
        Expr::Field { base, .. } => check_expr(base, expected, var_clocks, eq, info),
        Expr::Binary { lhs, rhs, .. } => {
            check_expr(lhs, expected, var_clocks, eq, info);
            check_expr(rhs, expected, var_clocks, eq, info);
        }
        Expr::IfThenElse { cond, then_branch, else_branch } => {
            check_expr(cond, expected, var_clocks, eq, info);
            check_expr(then_branch, expected, var_clocks, eq, info);
            check_expr(else_branch, expected, var_clocks, eq, info);
        }
        Expr::Index { base, index } => {
            check_expr(base, expected, var_clocks, eq, info);
            check_expr(index, expected, var_clocks, eq, info);
        }
        Expr::Tuple { items } | Expr::Array { items } => {
            for i in items {
                check_expr(i, expected, var_clocks, eq, info);
            }
        }
        Expr::Struct { fields, .. } => {
            for fi in fields {
                check_expr(&fi.value, expected, var_clocks, eq, info);
            }
        }
        // Iterators operate on whole arrays at the base clock; their operands
        // share the iterator's own clock (clocked iteration is roadmap).
        Expr::Iterate { init, arrays, .. } => {
            if let Some(i) = init {
                check_expr(i, expected, var_clocks, eq, info);
            }
            for a in arrays {
                check_expr(a, expected, var_clocks, eq, info);
            }
        }
        // Intrinsics are pure and run on their operands' shared clock.
        Expr::Intrinsic { args, .. } => {
            for a in args {
                check_expr(a, expected, var_clocks, eq, info);
            }
        }
    }
}

/// A clock variable must itself live on the parent clock it samples.
fn check_clock_var(
    clock: &str,
    parent: &Clock,
    var_clocks: &HashMap<String, Clock>,
    eq: usize,
    info: &mut ClockInfo,
) {
    if let Some(ck) = var_clocks.get(clock) {
        if ck != parent {
            info.errors.push(ClockError {
                equation: Some(eq),
                message: format!(
                    "clock variable `{clock}` is on {} but samples a stream on {}",
                    ck.describe(),
                    parent.describe()
                ),
            });
        }
    }
}
