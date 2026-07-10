//! OpenLustre Studio: CoCoSpec contract well-formedness checks.
//!
//! These checks correspond to Phase 3 of the implementation plan. They are
//! deliberately conservative — they verify *structural* properties that Kind 2
//! itself does not check (or only checks lazily). Semantic checks like
//! realizability are delegated to the Kind 2 adapter.

use std::collections::{BTreeMap, HashMap};

use ol_contract_ir::{parse_contracts, Assumption, ContractDef, ContractImport, Guarantee, Mode};
use ol_ir::{Diagnostic, Expr, Literal, NodeDef, Project, Severity, Type, UnaryOp};

#[derive(Debug, Clone)]
pub struct ContractReport {
    pub diagnostics: Vec<Diagnostic>,
    pub contracts: Vec<ContractDef>,
}

impl ContractReport {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| matches!(d.severity, Severity::Error))
    }
}

pub fn check_project(project: &Project) -> ContractReport {
    let mut diags = Vec::new();
    let mut all_contracts = Vec::new();

    for pkg in &project.packages {
        let (contracts, parse_errors) = parse_contracts(&pkg.contracts);
        for e in parse_errors {
            diags.push(Diagnostic::error("C0001", e).with_context(format!("package {}", pkg.name)));
        }

        let by_name: HashMap<String, &ContractDef> =
            contracts.iter().map(|c| (c.name.clone(), c)).collect();

        for c in &contracts {
            check_contract(c, &by_name, &mut diags);
        }

        for node in &pkg.nodes {
            if let Some(ref cname) = node.contract {
                match by_name.get(cname) {
                    None => diags.push(
                        Diagnostic::error(
                            "C0010",
                            format!(
                                "node `{}` references unknown contract `{}`",
                                node.name, cname
                            ),
                        )
                        .with_context(format!("node {}", node.name)),
                    ),
                    Some(c) => verify_contract_matches_node(node, c, &mut diags),
                }
            } else if !node.is_imported() {
                diags.push(
                    Diagnostic::warning(
                        "C0099",
                        format!("public operator `{}` has no contract", node.name),
                    )
                    .with_context(format!("node {}", node.name)),
                );
            }
        }

        all_contracts.extend(contracts);
    }

    ContractReport {
        diagnostics: diags,
        contracts: all_contracts,
    }
}

fn check_contract(
    contract: &ContractDef,
    by_name: &HashMap<String, &ContractDef>,
    diags: &mut Vec<Diagnostic>,
) {
    let ctx = format!("contract {}", contract.name);

    let mut env: BTreeMap<String, Type> = BTreeMap::new();
    for p in &contract.inputs {
        env.insert(p.name.clone(), p.ty.clone());
    }
    for p in &contract.outputs {
        env.insert(p.name.clone(), p.ty.clone());
    }
    for g in &contract.ghost_vars {
        env.insert(g.name.clone(), g.ty.clone());
    }
    let input_names: std::collections::HashSet<&str> =
        contract.inputs.iter().map(|p| p.name.as_str()).collect();
    let output_names: std::collections::HashSet<&str> =
        contract.outputs.iter().map(|p| p.name.as_str()).collect();

    for (i, a) in contract.assumptions.iter().enumerate() {
        check_assumption(a, i, &input_names, &output_names, &ctx, diags);
    }
    for (i, g) in contract.guarantees.iter().enumerate() {
        check_guarantee(g, i, &ctx, diags);
    }
    for m in &contract.modes {
        check_mode(m, &ctx, diags);
    }
    check_mode_overlap(&contract.modes, &ctx, diags);
    for imp in &contract.imports {
        check_contract_import(imp, by_name, &ctx, diags);
    }

    if !contract.modes.is_empty() {
        diags.push(
            Diagnostic::info(
                "C0200",
                format!(
                    "contract {} has {} modes; Kind 2 will check mode exhaustiveness at prove time",
                    contract.name,
                    contract.modes.len()
                ),
            )
            .with_context(ctx.clone()),
        );
    }
}

fn check_assumption(
    a: &Assumption,
    idx: usize,
    input_names: &std::collections::HashSet<&str>,
    output_names: &std::collections::HashSet<&str>,
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let label = a.name.clone().unwrap_or_else(|| format!("#{idx}"));
    for v in a.expr.free_vars() {
        if output_names.contains(v.as_str()) && !is_temporal_reference(&a.expr, &v) {
            diags.push(
                Diagnostic::error(
                    "C0020",
                    format!(
                        "assumption `{label}` depends on current output `{v}`; assumptions may only refer to inputs or previous outputs"
                    ),
                )
                .with_context(ctx.to_string()),
            );
        }
        if !input_names.contains(v.as_str()) && !output_names.contains(v.as_str()) {
            diags.push(
                Diagnostic::warning(
                    "C0021",
                    format!(
                        "assumption `{label}` references `{v}`, which is not in the contract interface"
                    ),
                )
                .with_context(ctx.to_string()),
            );
        }
    }
    if static_truth(&a.expr) == Some(false) {
        diags.push(
            Diagnostic::warning(
                "C0063",
                format!(
                    "assumption `{label}` is statically false — the contract can never be entered"
                ),
            )
            .with_context(ctx.to_string()),
        );
    }
}

fn check_guarantee(g: &Guarantee, idx: usize, ctx: &str, diags: &mut Vec<Diagnostic>) {
    let label = g.name.clone().unwrap_or_else(|| format!("#{idx}"));
    if !is_boolean_shape(&g.expr) {
        diags.push(
            Diagnostic::warning(
                "C0030",
                format!("guarantee `{label}` does not appear to be a Boolean expression"),
            )
            .with_context(ctx.to_string()),
        );
    }
    if static_truth(&g.expr) == Some(true) || is_tautology(&g.expr) {
        diags.push(
            Diagnostic::warning(
                "C0062",
                format!(
                    "guarantee `{label}` is statically true and therefore vacuous"
                ),
            )
            .with_context(ctx.to_string()),
        );
    }
}

fn check_mode(mode: &Mode, ctx: &str, diags: &mut Vec<Diagnostic>) {
    for (i, r) in mode.requires.iter().enumerate() {
        if !is_boolean_shape(r) {
            diags.push(
                Diagnostic::warning(
                    "C0040",
                    format!("mode `{}` require #{i} is not Boolean-shaped", mode.name),
                )
                .with_context(ctx.to_string()),
            );
        }
        if static_truth(r) == Some(false) {
            diags.push(
                Diagnostic::warning(
                    "C0060",
                    format!(
                        "mode `{}` require #{i} is statically false — mode is unreachable",
                        mode.name
                    ),
                )
                .with_context(ctx.to_string()),
            );
        }
    }
    // A mode whose requires contain `e` AND `not e` can never be active.
    for (i, r1) in mode.requires.iter().enumerate() {
        for r2 in mode.requires.iter().skip(i + 1) {
            if is_negation_of(r1, r2) {
                diags.push(
                    Diagnostic::warning(
                        "C0060",
                        format!(
                            "mode `{}` is unreachable: its requires contain an expression and its negation",
                            mode.name
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
            }
        }
    }
    for (i, e) in mode.ensures.iter().enumerate() {
        if !is_boolean_shape(e) {
            diags.push(
                Diagnostic::warning(
                    "C0041",
                    format!("mode `{}` ensure #{i} is not Boolean-shaped", mode.name),
                )
                .with_context(ctx.to_string()),
            );
        }
        if static_truth(e) == Some(true) || is_tautology(e) {
            diags.push(
                Diagnostic::warning(
                    "C0061",
                    format!(
                        "mode `{}` ensure #{i} is statically true and therefore vacuous",
                        mode.name
                    ),
                )
                .with_context(ctx.to_string()),
            );
        }
    }
    if mode.requires.is_empty() {
        diags.push(
            Diagnostic::warning(
                "C0042",
                format!("mode `{}` has no requires; it is always active", mode.name),
            )
            .with_context(ctx.to_string()),
        );
    }
}

/// Two modes with identical requires can never be distinguished — the later
/// one is dead in any execution where the earlier already fired.
fn check_mode_overlap(modes: &[Mode], ctx: &str, diags: &mut Vec<Diagnostic>) {
    for (i, m1) in modes.iter().enumerate() {
        for m2 in modes.iter().skip(i + 1) {
            if m1.requires == m2.requires && !m1.requires.is_empty() {
                diags.push(
                    Diagnostic::warning(
                        "C0064",
                        format!(
                            "modes `{}` and `{}` share identical requires — one shadows the other",
                            m1.name, m2.name
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
            }
        }
    }
}

fn check_contract_import(
    imp: &ContractImport,
    by_name: &HashMap<String, &ContractDef>,
    ctx: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let target = match by_name.get(imp.contract.as_str()) {
        Some(c) => *c,
        None => {
            diags.push(
                Diagnostic::error(
                    "C0070",
                    format!("contract import references unknown contract `{}`", imp.contract),
                )
                .with_context(ctx.to_string()),
            );
            return;
        }
    };

    let mapped_inputs: std::collections::HashSet<&str> =
        imp.input_map.iter().map(|(n, _)| n.as_str()).collect();
    let expected_inputs: std::collections::HashSet<&str> =
        target.inputs.iter().map(|p| p.name.as_str()).collect();
    for name in expected_inputs.difference(&mapped_inputs) {
        diags.push(
            Diagnostic::error(
                "C0071",
                format!(
                    "contract import of `{}` does not map input `{name}`",
                    imp.contract
                ),
            )
            .with_context(ctx.to_string()),
        );
    }
    for (n, _) in &imp.input_map {
        if !expected_inputs.contains(n.as_str()) {
            diags.push(
                Diagnostic::error(
                    "C0071",
                    format!(
                        "contract import of `{}` maps unknown input `{n}`",
                        imp.contract
                    ),
                )
                .with_context(ctx.to_string()),
            );
        }
    }

    let mapped_outputs: std::collections::HashSet<&str> =
        imp.output_map.iter().map(|(n, _)| n.as_str()).collect();
    let expected_outputs: std::collections::HashSet<&str> =
        target.outputs.iter().map(|p| p.name.as_str()).collect();
    for name in expected_outputs.difference(&mapped_outputs) {
        diags.push(
            Diagnostic::error(
                "C0072",
                format!(
                    "contract import of `{}` does not map output `{name}`",
                    imp.contract
                ),
            )
            .with_context(ctx.to_string()),
        );
    }
    for (n, _) in &imp.output_map {
        if !expected_outputs.contains(n.as_str()) {
            diags.push(
                Diagnostic::error(
                    "C0072",
                    format!(
                        "contract import of `{}` maps unknown output `{n}`",
                        imp.contract
                    ),
                )
                .with_context(ctx.to_string()),
            );
        }
    }
}

fn verify_contract_matches_node(
    node: &NodeDef,
    contract: &ContractDef,
    diags: &mut Vec<Diagnostic>,
) {
    let ctx = format!("node {} / contract {}", node.name, contract.name);
    if node.inputs.len() != contract.inputs.len() {
        diags.push(
            Diagnostic::error(
                "C0050",
                format!(
                    "node `{}` has {} inputs but contract `{}` has {}",
                    node.name,
                    node.inputs.len(),
                    contract.name,
                    contract.inputs.len()
                ),
            )
            .with_context(ctx.clone()),
        );
    } else {
        for (np, cp) in node.inputs.iter().zip(contract.inputs.iter()) {
            if np.name != cp.name || np.ty != cp.ty {
                diags.push(
                    Diagnostic::error(
                        "C0051",
                        format!(
                            "input `{}: {:?}` of node `{}` does not match contract input `{}: {:?}`",
                            np.name, np.ty, node.name, cp.name, cp.ty
                        ),
                    )
                    .with_context(ctx.clone()),
                );
            }
        }
    }
    if node.outputs.len() != contract.outputs.len() {
        diags.push(
            Diagnostic::error(
                "C0052",
                format!(
                    "node `{}` has {} outputs but contract `{}` has {}",
                    node.name,
                    node.outputs.len(),
                    contract.name,
                    contract.outputs.len()
                ),
            )
            .with_context(ctx.clone()),
        );
    } else {
        for (np, cp) in node.outputs.iter().zip(contract.outputs.iter()) {
            if np.name != cp.name || np.ty != cp.ty {
                diags.push(
                    Diagnostic::error(
                        "C0053",
                        format!(
                            "output `{}: {:?}` of node `{}` does not match contract output `{}: {:?}`",
                            np.name, np.ty, node.name, cp.name, cp.ty
                        ),
                    )
                    .with_context(ctx.clone()),
                );
            }
        }
    }
}

fn is_boolean_shape(e: &Expr) -> bool {
    use ol_ir::BinOp;
    match e {
        Expr::Const { lit: ol_ir::Literal::Bool { .. } } => true,
        Expr::Unary {
            op: ol_ir::UnaryOp::Not,
            ..
        } => true,
        Expr::Binary { op, .. } => matches!(
            op,
            BinOp::And
                | BinOp::Or
                | BinOp::Xor
                | BinOp::Implies
                | BinOp::Eq
                | BinOp::Neq
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
        ),
        Expr::Var { .. } | Expr::IfThenElse { .. } | Expr::Call { .. } | Expr::Field { .. } => true,
        _ => false,
    }
}

/// Conservative compile-time evaluation of a Boolean expression. Returns
/// `Some(true)`/`Some(false)` only when the value is structurally determined;
/// any reference to a variable or anything more complex returns `None`.
fn static_truth(expr: &Expr) -> Option<bool> {
    use ol_ir::BinOp;
    match expr {
        Expr::Const { lit: Literal::Bool { value } } => Some(*value),
        Expr::Unary { op: UnaryOp::Not, arg } => static_truth(arg).map(|b| !b),
        Expr::Binary { op, lhs, rhs } => {
            let l = static_truth(lhs);
            let r = static_truth(rhs);
            match op {
                BinOp::And => match (l, r) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                },
                BinOp::Or => match (l, r) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                },
                BinOp::Implies => match (l, r) {
                    (Some(false), _) | (_, Some(true)) => Some(true),
                    (Some(true), Some(false)) => Some(false),
                    _ => None,
                },
                BinOp::Eq => {
                    if lhs == rhs {
                        Some(true)
                    } else {
                        match (l, r) {
                            (Some(a), Some(b)) => Some(a == b),
                            _ => None,
                        }
                    }
                }
                BinOp::Neq => {
                    if lhs == rhs {
                        Some(false)
                    } else {
                        match (l, r) {
                            (Some(a), Some(b)) => Some(a != b),
                            _ => None,
                        }
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// True if `a` is `not b` (or vice versa), structurally.
fn is_negation_of(a: &Expr, b: &Expr) -> bool {
    if let Expr::Unary { op: UnaryOp::Not, arg } = a {
        if arg.as_ref() == b {
            return true;
        }
    }
    if let Expr::Unary { op: UnaryOp::Not, arg } = b {
        if arg.as_ref() == a {
            return true;
        }
    }
    false
}

/// Recognize a handful of common tautology shapes: `e or not e`, `e implies
/// e`, `e = e`. Everything else returns false (no false positives).
fn is_tautology(expr: &Expr) -> bool {
    use ol_ir::BinOp;
    match expr {
        Expr::Binary { op: BinOp::Or, lhs, rhs } => {
            is_negation_of(lhs, rhs) || lhs == rhs && static_truth(lhs) == Some(true)
        }
        Expr::Binary { op: BinOp::Implies, lhs, rhs } => lhs == rhs,
        Expr::Binary { op: BinOp::Eq, lhs, rhs } => lhs == rhs,
        _ => false,
    }
}

fn is_temporal_reference(expr: &Expr, name: &str) -> bool {
    fn walk(e: &Expr, name: &str, in_pre: bool) -> bool {
        match e {
            Expr::Var { name: n } if n == name => in_pre,
            Expr::Const { .. } | Expr::Var { .. } | Expr::Last { .. } => true,
            Expr::Pre { arg } => walk(arg, name, true),
            Expr::Arrow { init, body } => walk(init, name, in_pre) && walk(body, name, in_pre),
            Expr::Unary { arg, .. } | Expr::Cast { arg, .. } => walk(arg, name, in_pre),
            Expr::Binary { lhs, rhs, .. } => walk(lhs, name, in_pre) && walk(rhs, name, in_pre),
            Expr::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                walk(cond, name, in_pre)
                    && walk(then_branch, name, in_pre)
                    && walk(else_branch, name, in_pre)
            }
            Expr::Call { args, .. } => args.iter().all(|a| walk(a, name, in_pre)),
            Expr::Field { base, .. } => walk(base, name, in_pre),
            Expr::Index { base, index } => walk(base, name, in_pre) && walk(index, name, in_pre),
            Expr::Tuple { items } | Expr::Array { items } => {
                items.iter().all(|i| walk(i, name, in_pre))
            }
            Expr::Struct { fields, .. } => fields.iter().all(|fi| walk(&fi.value, name, in_pre)),
            Expr::When { arg, .. } => walk(arg, name, in_pre),
            Expr::Merge { on_true, on_false, .. } => {
                walk(on_true, name, in_pre) && walk(on_false, name, in_pre)
            }
            Expr::Iterate { init, arrays, .. } => {
                init.as_deref().map(|i| walk(i, name, in_pre)).unwrap_or(true)
                    && arrays.iter().all(|a| walk(a, name, in_pre))
            }
            Expr::FloatIntrinsic { args, .. }
            | Expr::ArrayOp { args, .. }
            | Expr::Printout { args } => args.iter().all(|a| walk(a, name, in_pre)),
            Expr::Case { sel, arms, default } => {
                walk(sel, name, in_pre)
                    && arms.iter().all(|a| walk(&a.value, name, in_pre))
                    && default.as_deref().map(|d| walk(d, name, in_pre)).unwrap_or(true)
            }
        }
    }
    walk(expr, name, false)
}
