//! OpenLustre Studio: type and well-formedness checker for the dataflow IR.
//!
//! Responsibilities:
//! * Validate every wire type
//! * Resolve and validate every node call against a signature
//! * Enforce the function-vs-operator distinction (no `pre`, `->`, or stateful
//!   node calls inside `function`s)
//! * Enforce single assignment for every output and local
//! * Detect combinational cycles that don't cross a temporal break
//! * Report uninitialized `pre` (i.e. `pre` not under an `->`)

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ol_ir::{
    BinOp, Diagnostic, Expr, Literal, NodeDef, NodeKind, Port, Project, Severity, Type, UnaryOp,
};

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckReport {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| matches!(d.severity, Severity::Error))
    }
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| matches!(d.severity, Severity::Error))
    }
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| matches!(d.severity, Severity::Warning))
    }
    pub fn merge(&mut self, other: CheckReport) {
        self.diagnostics.extend(other.diagnostics);
    }
}

pub fn check_project(project: &Project) -> CheckReport {
    let mut diags = Vec::new();

    let mut signatures: HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)> = HashMap::new();
    for n in project.all_nodes() {
        if signatures
            .insert(
                n.name.clone(),
                (n.inputs.clone(), n.outputs.clone(), n.kind),
            )
            .is_some()
        {
            diags.push(
                Diagnostic::error("E0001", format!("duplicate node name `{}`", n.name))
                    .with_context(format!("node {}", n.name)),
            );
        }
    }

    for n in project.all_nodes() {
        check_node(n, &signatures, &mut diags);
    }

    CheckReport { diagnostics: diags }
}

fn check_node(
    node: &NodeDef,
    sigs: &HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)>,
    diags: &mut Vec<Diagnostic>,
) {
    let ctx = format!("node {}", node.name);

    if node.is_imported() {
        if !node.equations.is_empty() {
            diags.push(
                Diagnostic::error(
                    "E0002",
                    "imported nodes must not have equations; their body is external C",
                )
                .with_context(ctx.clone()),
            );
        }
        return;
    }

    let mut env: BTreeMap<String, Type> = BTreeMap::new();
    for p in &node.inputs {
        if env.insert(p.name.clone(), p.ty.clone()).is_some() {
            diags.push(
                Diagnostic::error("E0010", format!("duplicate input `{}`", p.name))
                    .with_context(ctx.clone()),
            );
        }
    }
    for p in &node.outputs {
        if env.insert(p.name.clone(), p.ty.clone()).is_some() {
            diags.push(
                Diagnostic::error("E0011", format!("output `{}` shadows another port", p.name))
                    .with_context(ctx.clone()),
            );
        }
    }
    for l in &node.locals {
        if env.insert(l.name.clone(), l.ty.clone()).is_some() {
            diags.push(
                Diagnostic::error("E0012", format!("local `{}` shadows another binding", l.name))
                    .with_context(ctx.clone()),
            );
        }
    }

    let mut assigned: BTreeSet<String> = BTreeSet::new();
    for eq in &node.equations {
        for lhs in &eq.lhs {
            if !env.contains_key(lhs) {
                diags.push(
                    Diagnostic::error("E0020", format!("equation defines unknown name `{lhs}`"))
                        .with_context(ctx.clone()),
                );
            }
            if !assigned.insert(lhs.clone()) {
                diags.push(
                    Diagnostic::error(
                        "E0021",
                        format!("name `{lhs}` is assigned by more than one equation"),
                    )
                    .with_context(ctx.clone()),
                );
            }
        }

        if node.is_function() && eq.rhs.contains_temporal() {
            diags.push(
                Diagnostic::error(
                    "E0030",
                    "function bodies may not use temporal operators (`pre`, `->`)",
                )
                .with_context(ctx.clone()),
            );
        }

        check_pre_initialization(&eq.rhs, false, diags, &ctx);

        if let Some(rhs_ty) = infer_expr_type(&eq.rhs, &env, sigs, node, diags, &ctx) {
            match eq.lhs.len() {
                1 => {
                    if let Some(expected) = env.get(&eq.lhs[0]) {
                        if !types_compatible(expected, &rhs_ty)
                            && rhs_ty != Type::Named("__tuple__".into())
                        {
                            diags.push(
                                Diagnostic::error(
                                    "E0040",
                                    format!(
                                        "equation `{} = ...` has type {:?} but `{}` is declared as {:?}",
                                        eq.lhs[0], rhs_ty, eq.lhs[0], expected
                                    ),
                                )
                                .with_context(ctx.clone()),
                            );
                        }
                    }
                }
                _ => {
                    if rhs_ty != Type::Named("__tuple__".into()) {
                        diags.push(
                            Diagnostic::error(
                                "E0041",
                                "multi-output equation must bind to a node call returning a tuple",
                            )
                            .with_context(ctx.clone()),
                        );
                    }
                }
            }
        }
    }

    for p in &node.outputs {
        if !assigned.contains(&p.name) {
            diags.push(
                Diagnostic::error("E0050", format!("output `{}` is never assigned", p.name))
                    .with_context(ctx.clone()),
            );
        }
    }
    for l in &node.locals {
        if !assigned.contains(&l.name) {
            diags.push(
                Diagnostic::warning(
                    "W0051",
                    format!("local `{}` is declared but never assigned", l.name),
                )
                .with_context(ctx.clone()),
            );
        }
    }

    if !node.is_function() {
        if let Some(cycle) = detect_combinational_cycle(node) {
            diags.push(
                Diagnostic::error(
                    "E0060",
                    format!(
                        "combinational cycle without a temporal break: {}",
                        cycle.join(" -> ")
                    ),
                )
                .with_context(ctx.clone()),
            );
        }
    }
}

fn check_pre_initialization(
    expr: &Expr,
    under_arrow_body: bool,
    diags: &mut Vec<Diagnostic>,
    ctx: &str,
) {
    match expr {
        Expr::Pre { arg } => {
            if !under_arrow_body {
                diags.push(
                    Diagnostic::error(
                        "E0070",
                        "`pre` must appear under an `->` providing an initial value",
                    )
                    .with_context(ctx.to_string()),
                );
            }
            check_pre_initialization(arg, false, diags, ctx);
        }
        Expr::Arrow { init, body } => {
            check_pre_initialization(init, under_arrow_body, diags, ctx);
            check_pre_initialization(body, true, diags, ctx);
        }
        Expr::Unary { arg, .. } => check_pre_initialization(arg, under_arrow_body, diags, ctx),
        Expr::Binary { lhs, rhs, .. } => {
            check_pre_initialization(lhs, under_arrow_body, diags, ctx);
            check_pre_initialization(rhs, under_arrow_body, diags, ctx);
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            check_pre_initialization(cond, under_arrow_body, diags, ctx);
            check_pre_initialization(then_branch, under_arrow_body, diags, ctx);
            check_pre_initialization(else_branch, under_arrow_body, diags, ctx);
        }
        Expr::Call { args, .. } => {
            for a in args {
                check_pre_initialization(a, under_arrow_body, diags, ctx);
            }
        }
        Expr::Field { base, .. } => check_pre_initialization(base, under_arrow_body, diags, ctx),
        Expr::Index { base, index } => {
            check_pre_initialization(base, under_arrow_body, diags, ctx);
            check_pre_initialization(index, under_arrow_body, diags, ctx);
        }
        Expr::Tuple { items } => {
            for i in items {
                check_pre_initialization(i, under_arrow_body, diags, ctx);
            }
        }
        Expr::Const { .. } | Expr::Var { .. } => {}
    }
}

fn types_compatible(a: &Type, b: &Type) -> bool {
    a == b
}

pub fn infer_expr_type(
    expr: &Expr,
    env: &BTreeMap<String, Type>,
    sigs: &HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)>,
    node: &NodeDef,
    diags: &mut Vec<Diagnostic>,
    ctx: &str,
) -> Option<Type> {
    match expr {
        Expr::Const { lit } => Some(match lit {
            Literal::Bool { .. } => Type::Bool,
            Literal::Int { .. } => Type::Int32,
            Literal::Float { .. } => Type::Float64,
        }),
        Expr::Var { name } => match env.get(name) {
            Some(t) => Some(t.clone()),
            None => {
                diags.push(
                    Diagnostic::error("E0080", format!("unknown identifier `{name}`"))
                        .with_context(ctx.to_string()),
                );
                None
            }
        },
        Expr::Unary { op, arg } => {
            let a = infer_expr_type(arg, env, sigs, node, diags, ctx)?;
            match op {
                UnaryOp::Not => {
                    if !a.is_bool() {
                        diags.push(
                            Diagnostic::error("E0081", format!("`not` requires bool, got {a:?}"))
                                .with_context(ctx.to_string()),
                        );
                        None
                    } else {
                        Some(Type::Bool)
                    }
                }
                UnaryOp::Neg => {
                    if !a.is_numeric() {
                        diags.push(
                            Diagnostic::error(
                                "E0082",
                                format!("unary `-` requires numeric, got {a:?}"),
                            )
                            .with_context(ctx.to_string()),
                        );
                        None
                    } else {
                        Some(a)
                    }
                }
            }
        }
        Expr::Binary { op, lhs, rhs } => {
            let l = infer_expr_type(lhs, env, sigs, node, diags, ctx)?;
            let r = infer_expr_type(rhs, env, sigs, node, diags, ctx)?;
            match op {
                BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Implies => {
                    if !(l.is_bool() && r.is_bool()) {
                        diags.push(
                            Diagnostic::error(
                                "E0083",
                                format!(
                                    "logical operator requires bool operands, got {l:?} and {r:?}"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                    Some(Type::Bool)
                }
                BinOp::Eq | BinOp::Neq => {
                    if l != r {
                        diags.push(
                            Diagnostic::error(
                                "E0084",
                                format!("equality requires matching types, got {l:?} and {r:?}"),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                    Some(Type::Bool)
                }
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    if !(l.is_numeric() && r.is_numeric() && l == r) {
                        diags.push(
                            Diagnostic::error(
                                "E0085",
                                format!(
                                    "ordering requires matching numeric types, got {l:?} and {r:?}"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                    Some(Type::Bool)
                }
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    if !(l.is_numeric() && r.is_numeric() && l == r) {
                        diags.push(
                            Diagnostic::error(
                                "E0086",
                                format!(
                                    "arithmetic requires matching numeric types, got {l:?} and {r:?}"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                    Some(l)
                }
            }
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            let c = infer_expr_type(cond, env, sigs, node, diags, ctx)?;
            if !c.is_bool() {
                diags.push(
                    Diagnostic::error("E0090", format!("if-condition must be bool, got {c:?}"))
                        .with_context(ctx.to_string()),
                );
                return None;
            }
            let t = infer_expr_type(then_branch, env, sigs, node, diags, ctx)?;
            let e = infer_expr_type(else_branch, env, sigs, node, diags, ctx)?;
            if t != e {
                diags.push(
                    Diagnostic::error(
                        "E0091",
                        format!("if branches must agree in type; got {t:?} vs {e:?}"),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            Some(t)
        }
        Expr::Pre { arg } => infer_expr_type(arg, env, sigs, node, diags, ctx),
        Expr::Arrow { init, body } => {
            let i = infer_expr_type(init, env, sigs, node, diags, ctx)?;
            let b = infer_expr_type(body, env, sigs, node, diags, ctx)?;
            if i != b {
                diags.push(
                    Diagnostic::error(
                        "E0092",
                        format!("`->` operands must have the same type; got {i:?} and {b:?}"),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            Some(i)
        }
        Expr::Call { node: callee, args } => {
            let Some((inputs, outputs, kind)) = sigs.get(callee) else {
                diags.push(
                    Diagnostic::error("E0100", format!("call to unknown node `{callee}`"))
                        .with_context(ctx.to_string()),
                );
                return None;
            };
            if node.is_function() && !matches!(kind, NodeKind::Function | NodeKind::Imported) {
                diags.push(
                    Diagnostic::error(
                        "E0101",
                        format!(
                            "function `{}` cannot call stateful operator `{}`",
                            node.name, callee
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
            }
            if args.len() != inputs.len() {
                diags.push(
                    Diagnostic::error(
                        "E0102",
                        format!(
                            "call to `{}` expects {} arguments, got {}",
                            callee,
                            inputs.len(),
                            args.len()
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            for (i, (a, p)) in args.iter().zip(inputs.iter()).enumerate() {
                if let Some(t) = infer_expr_type(a, env, sigs, node, diags, ctx) {
                    if !types_compatible(&p.ty, &t) {
                        diags.push(
                            Diagnostic::error(
                                "E0103",
                                format!(
                                    "call to `{}`: argument #{i} ({}) has type {:?}, expected {:?}",
                                    callee, p.name, t, p.ty
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                    }
                }
            }
            match outputs.len() {
                0 => Some(Type::Named("__unit__".into())),
                1 => Some(outputs[0].ty.clone()),
                _ => Some(Type::Named("__tuple__".into())),
            }
        }
        Expr::Field { base, field } => {
            let _ = infer_expr_type(base, env, sigs, node, diags, ctx)?;
            Some(Type::Named(field.clone()))
        }
        Expr::Index { base, index } => {
            let bt = infer_expr_type(base, env, sigs, node, diags, ctx)?;
            let _it = infer_expr_type(index, env, sigs, node, diags, ctx)?;
            if let Type::Array { elem, .. } = bt {
                Some(*elem)
            } else {
                diags.push(
                    Diagnostic::error("E0110", format!("indexing a non-array of type {bt:?}"))
                        .with_context(ctx.to_string()),
                );
                None
            }
        }
        Expr::Tuple { .. } => Some(Type::Named("__tuple__".into())),
    }
}

/// Build a per-variable dependency graph that ignores edges through `pre` /
/// `->` body. Returns a name-path describing the offending cycle if one is
/// found.
fn detect_combinational_cycle(node: &NodeDef) -> Option<Vec<String>> {
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for eq in &node.equations {
        for lhs in &eq.lhs {
            let entry = deps.entry(lhs.clone()).or_default();
            collect_immediate_deps(&eq.rhs, entry);
        }
    }
    for v in deps.keys() {
        let mut visiting = BTreeSet::new();
        let mut path = Vec::new();
        if let Some(cy) = dfs(v, &deps, &mut visiting, &mut path) {
            return Some(cy);
        }
    }
    None
}

fn dfs(
    node: &str,
    deps: &BTreeMap<String, BTreeSet<String>>,
    visiting: &mut BTreeSet<String>,
    path: &mut Vec<String>,
) -> Option<Vec<String>> {
    if visiting.contains(node) {
        let pos = path.iter().position(|p| p == node).unwrap_or(0);
        let mut cycle = path[pos..].to_vec();
        cycle.push(node.to_string());
        return Some(cycle);
    }
    visiting.insert(node.to_string());
    path.push(node.to_string());
    if let Some(succ) = deps.get(node) {
        for s in succ {
            if let Some(cy) = dfs(s, deps, visiting, path) {
                return Some(cy);
            }
        }
    }
    path.pop();
    visiting.remove(node);
    None
}

fn collect_immediate_deps(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Const { .. } => {}
        Expr::Var { name } => {
            out.insert(name.clone());
        }
        Expr::Unary { arg, .. } => collect_immediate_deps(arg, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_immediate_deps(lhs, out);
            collect_immediate_deps(rhs, out);
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_immediate_deps(cond, out);
            collect_immediate_deps(then_branch, out);
            collect_immediate_deps(else_branch, out);
        }
        Expr::Pre { .. } => {}
        Expr::Arrow { init, .. } => collect_immediate_deps(init, out),
        Expr::Call { args, .. } => {
            for a in args {
                collect_immediate_deps(a, out);
            }
        }
        Expr::Field { base, .. } => collect_immediate_deps(base, out),
        Expr::Index { base, index } => {
            collect_immediate_deps(base, out);
            collect_immediate_deps(index, out);
        }
        Expr::Tuple { items } => {
            for i in items {
                collect_immediate_deps(i, out);
            }
        }
    }
}
