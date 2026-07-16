//! `openlustre scade-check`: the portability gate for the design-here,
//! qualify-in-SCADE workflow. It walks a model and reports every construct
//! that has **no 1:1 Ansys SCADE equivalent**, so a green report means the
//! model can be redrawn in SCADE block-for-block. The predefined operators
//! OpenLustre shares with SCADE (arithmetic, logic, bitwise, casts, clocks,
//! `pre`/`fby`/`->`, records/enums/arrays, iterators, `#`, replication,
//! slice, transpose, dynamic projection, functional update, state machines,
//! generics) are all portable and never flagged; only OpenLustre extensions
//! are.

use ol_ir::{BinOp, Expr, Project, Type};

/// One portability finding: where it is, the offending construct, and how to
/// make it SCADE-portable.
pub struct Finding {
    pub node: String,
    pub location: String,
    pub construct: String,
    pub suggestion: String,
}

/// Collect every non-SCADE-portable construct in the project.
pub fn check(project: &Project) -> Vec<Finding> {
    let mut out = Vec::new();

    for pkg in &project.packages {
        // Constants of a fixed-point type.
        for c in &pkg.constants {
            flag_type(&c.ty, &format!("constant `{}`", c.name), "constants", &mut out);
        }
        for n in &pkg.nodes {
            let here = &n.name;
            for p in n.inputs.iter().chain(n.outputs.iter()) {
                flag_type(&p.ty, &format!("port `{}`", p.name), here, &mut out);
            }
            for l in &n.locals {
                flag_type(&l.ty, &format!("local `{}`", l.name), here, &mut out);
            }
            for (i, eq) in n.equations.iter().enumerate() {
                let loc = format!("equation #{i} (`{}`)", eq.lhs.join(", "));
                flag_expr(&eq.rhs, &loc, here, &mut out);
            }
        }
    }
    out
}

/// SCADE has no Q-format fixed-point type; flag any `sfix`/`ufix`.
fn flag_type(ty: &Type, where_: &str, node: &str, out: &mut Vec<Finding>) {
    let mut found = false;
    walk_type(ty, &mut |t| {
        if matches!(t, Type::Fixed { .. }) {
            found = true;
        }
    });
    if found {
        out.push(Finding {
            node: node.to_string(),
            location: where_.to_string(),
            construct: "fixed-point type (sfix/ufix)".to_string(),
            suggestion: "SCADE has no Q-format fixed-point type — redraw as an integer with \
                         explicit scaling, or use a SCADE fixed-point library type"
                .to_string(),
        });
    }
}

fn walk_type(ty: &Type, f: &mut impl FnMut(&Type)) {
    f(ty);
    if let Type::Array { elem, .. } = ty {
        walk_type(elem, f);
    }
}

fn flag_expr(expr: &Expr, location: &str, node: &str, out: &mut Vec<Finding>) {
    expr.visit(|e| match e {
        Expr::Printout { .. } => out.push(Finding {
            node: node.to_string(),
            location: location.to_string(),
            construct: "printout block".to_string(),
            suggestion: "printout is an OpenLustre debug block with no SCADE equivalent — \
                         remove it, or use a SCADE probe/output for observation"
                .to_string(),
        }),
        Expr::Binary { op, .. }
            if matches!(op, BinOp::SatAdd | BinOp::SatSub | BinOp::SatMul | BinOp::SatDiv) =>
        {
            out.push(Finding {
                node: node.to_string(),
                location: location.to_string(),
                construct: format!("saturating operator ({op:?})"),
                suggestion: "SCADE has no core saturating arithmetic operator — redraw with an \
                             explicit clamp (min/max) or a SCADE saturation library block"
                    .to_string(),
            })
        }
        Expr::Cast { to, .. } if matches!(to, Type::Fixed { .. }) => out.push(Finding {
            node: node.to_string(),
            location: location.to_string(),
            construct: "cast to a fixed-point type".to_string(),
            suggestion: "SCADE has no Q-format fixed-point cast — use integer scaling instead"
                .to_string(),
        }),
        _ => {}
    });
}

/// Render the report; returns `true` when the model is fully portable.
pub fn report(findings: &[Finding]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    if findings.is_empty() {
        let _ = writeln!(
            s,
            "scade-check: PORTABLE — every construct has a 1:1 SCADE equivalent; this model \
             can be redrawn in SCADE block-for-block."
        );
        return s;
    }
    let _ = writeln!(
        s,
        "scade-check: {} non-portable construct(s) — these have no 1:1 SCADE equivalent:\n",
        findings.len()
    );
    for f in findings {
        let _ = writeln!(s, "  [{}] {} — {}", f.node, f.location, f.construct);
        let _ = writeln!(s, "      → {}", f.suggestion);
    }
    let _ = writeln!(
        s,
        "\nThese are OpenLustre design-time conveniences. Replace them before redrawing \
         in SCADE for the qualified artifact."
    );
    s
}
