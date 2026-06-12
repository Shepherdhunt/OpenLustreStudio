//! Same-cycle evaluation order for a node's equations.
//!
//! Lustre equations are declarative: `n = constant1 + 1; constant1 = 1;` is
//! a perfectly well-formed model, and drawing tools routinely append
//! definitions after their consumers. Any executor that walks equations in
//! declaration order silently reads stale defaults for forward references —
//! both the IR simulator and the C emitter must walk in dependency order.

use std::collections::{BTreeSet, HashMap};

use crate::expr::Expr;
use crate::node::NodeDef;

/// Variables an expression reads *in the current cycle*: every `Var` not
/// under a `pre`. Both arms of `->` count — the body arm is evaluated with
/// current values from cycle 1 on.
fn same_cycle_reads(e: &Expr, out: &mut BTreeSet<String>) {
    match e {
        Expr::Var { name } => {
            out.insert(name.clone());
        }
        Expr::Const { .. } | Expr::Pre { .. } => {}
        Expr::Unary { arg, .. } | Expr::Cast { arg, .. } | Expr::Field { base: arg, .. } => {
            same_cycle_reads(arg, out)
        }
        Expr::Binary { lhs, rhs, .. } => {
            same_cycle_reads(lhs, out);
            same_cycle_reads(rhs, out);
        }
        Expr::Arrow { init, body } => {
            same_cycle_reads(init, out);
            same_cycle_reads(body, out);
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            same_cycle_reads(cond, out);
            same_cycle_reads(then_branch, out);
            same_cycle_reads(else_branch, out);
        }
        Expr::Call { args, .. } => {
            for a in args {
                same_cycle_reads(a, out);
            }
        }
        Expr::Index { base, index } => {
            same_cycle_reads(base, out);
            same_cycle_reads(index, out);
        }
        Expr::Tuple { items } => {
            for i in items {
                same_cycle_reads(i, out);
            }
        }
    }
}

/// The order in which a node's equations must execute so that every
/// same-cycle read of an equation-defined variable sees this cycle's value.
/// Deterministic: among simultaneously-ready equations, declaration order
/// wins. Errors when a combinational cycle admits no order at all.
pub fn evaluation_order(node: &NodeDef) -> Result<Vec<usize>, String> {
    let n = node.equations.len();
    let mut def_by: HashMap<&str, usize> = HashMap::new();
    for (i, eq) in node.equations.iter().enumerate() {
        for l in &eq.lhs {
            def_by.insert(l.as_str(), i);
        }
    }
    let mut deps: Vec<BTreeSet<usize>> = Vec::with_capacity(n);
    for eq in &node.equations {
        let mut reads = BTreeSet::new();
        same_cycle_reads(&eq.rhs, &mut reads);
        deps.push(
            reads
                .iter()
                .filter_map(|r| def_by.get(r.as_str()).copied())
                .collect(),
        );
    }
    let mut order = Vec::with_capacity(n);
    let mut done = vec![false; n];
    while order.len() < n {
        let mut progressed = false;
        for i in 0..n {
            if !done[i] && deps[i].iter().all(|&d| done[d]) {
                order.push(i);
                done[i] = true;
                progressed = true;
            }
        }
        if !progressed {
            let stuck: Vec<String> = (0..n)
                .filter(|&i| !done[i])
                .map(|i| node.equations[i].lhs.join(", "))
                .collect();
            return Err(format!(
                "combinational cycle among the equations defining: {}",
                stuck.join("; ")
            ));
        }
    }
    Ok(order)
}
