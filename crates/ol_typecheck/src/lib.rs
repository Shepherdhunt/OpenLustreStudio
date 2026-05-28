//! OpenLustre Studio: type and well-formedness checker for the dataflow IR.
//!
//! Responsibilities:
//! * Validate every wire type, with type aliases resolved transitively
//! * Resolve and validate every node call against a signature
//! * Resolve record-field access against the record's declared schema
//! * Resolve enum-variant references against the enum's declared variants
//! * Enforce the function-vs-operator distinction (no `pre`, `->`, or stateful
//!   node calls inside `function`s)
//! * Enforce single assignment for every output and local
//! * Detect combinational cycles that don't cross a temporal break
//! * Report uninitialized `pre` (i.e. `pre` not under an `->`)
//! * Range-check integer literals when the expected type is known, so that
//!   `uint8_var = 5` succeeds but `uint8_var = 500` fails

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ol_ir::{
    BinOp, Diagnostic, Expr, Literal, NodeDef, NodeKind, Port, Project, RecordField, Severity,
    Type, TypeBody, UnaryOp,
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

/// Project-wide type information collected once and threaded through every
/// expression-level check.
#[derive(Debug, Default, Clone)]
pub struct TypeContext {
    aliases: HashMap<String, Type>,
    records: HashMap<String, Vec<RecordField>>,
    enums: HashMap<String, Vec<String>>,
    enum_variant_to_name: HashMap<String, String>,
}

impl TypeContext {
    pub fn from_project(project: &Project) -> Self {
        let mut ctx = TypeContext::default();
        for pkg in &project.packages {
            for t in &pkg.types {
                match &t.body {
                    TypeBody::Alias { name, target } => {
                        ctx.aliases.insert(name.clone(), target.clone());
                    }
                    TypeBody::Record { name, fields } => {
                        ctx.records.insert(name.clone(), fields.clone());
                    }
                    TypeBody::Enum(e) => {
                        ctx.enums.insert(e.name.clone(), e.variants.clone());
                        for v in &e.variants {
                            ctx.enum_variant_to_name
                                .insert(v.clone(), e.name.clone());
                        }
                    }
                }
            }
        }
        ctx
    }

    /// Resolve named types through the alias chain. Self-referential aliases
    /// terminate at a fixed depth rather than looping forever.
    pub fn resolve(&self, ty: &Type) -> Type {
        let mut cur = ty.clone();
        for _ in 0..64 {
            match cur {
                Type::Named(ref n) => match self.aliases.get(n) {
                    Some(target) => {
                        cur = target.clone();
                    }
                    None => return cur,
                },
                Type::Array { elem, len } => {
                    return Type::Array {
                        elem: Box::new(self.resolve(&elem)),
                        len,
                    };
                }
                other => return other,
            }
        }
        cur
    }

    pub fn record_fields(&self, name: &str) -> Option<&Vec<RecordField>> {
        self.records.get(name)
    }

    pub fn enum_for_variant(&self, variant: &str) -> Option<&str> {
        self.enum_variant_to_name.get(variant).map(|s| s.as_str())
    }
}

pub fn check_project(project: &Project) -> CheckReport {
    let mut diags = Vec::new();
    let tctx = TypeContext::from_project(project);

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
        check_node(n, &signatures, &tctx, &mut diags);
    }

    CheckReport { diagnostics: diags }
}

fn check_node(
    node: &NodeDef,
    sigs: &HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)>,
    tctx: &TypeContext,
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

        // For single-output equations we pass the LHS's declared type as a
        // bidirectional hint so integer literals adopt the target type when
        // they fit (no implicit narrowing — only "untyped literal becomes
        // typed in context").
        let lhs_hint: Option<Type> = if eq.lhs.len() == 1 {
            env.get(&eq.lhs[0]).cloned()
        } else {
            None
        };
        let inferred = infer_expr_type(
            &eq.rhs,
            &env,
            sigs,
            node,
            diags,
            &ctx,
            tctx,
            lhs_hint.as_ref(),
        );

        if let Some(rhs_ty) = inferred {
            match eq.lhs.len() {
                1 => {
                    if let Some(expected) = env.get(&eq.lhs[0]) {
                        let is_tuple = matches!(&rhs_ty, Type::Named(n) if n == "__tuple__");
                        if !types_compatible(tctx, expected, &rhs_ty) && !is_tuple {
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
                    let is_tuple = matches!(&rhs_ty, Type::Named(n) if n == "__tuple__");
                    if !is_tuple {
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

fn types_compatible(tctx: &TypeContext, a: &Type, b: &Type) -> bool {
    tctx.resolve(a) == tctx.resolve(b)
}

fn fits_in_integer(value: i64, ty: &Type) -> bool {
    match ty {
        Type::Int8 => (i8::MIN as i64..=i8::MAX as i64).contains(&value),
        Type::Int16 => (i16::MIN as i64..=i16::MAX as i64).contains(&value),
        Type::Int32 => (i32::MIN as i64..=i32::MAX as i64).contains(&value),
        Type::Int64 => true,
        Type::Uint8 => (0..=u8::MAX as i64).contains(&value),
        Type::Uint16 => (0..=u16::MAX as i64).contains(&value),
        Type::Uint32 => (0..=u32::MAX as i64).contains(&value),
        Type::Uint64 => value >= 0,
        _ => false,
    }
}

pub fn infer_expr_type(
    expr: &Expr,
    env: &BTreeMap<String, Type>,
    sigs: &HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)>,
    node: &NodeDef,
    diags: &mut Vec<Diagnostic>,
    ctx: &str,
    tctx: &TypeContext,
    hint: Option<&Type>,
) -> Option<Type> {
    match expr {
        Expr::Const { lit } => Some(match lit {
            Literal::Bool { .. } => Type::Bool,
            Literal::Int { value } => integer_literal_type(*value, hint, tctx),
            Literal::Float { .. } => match hint.map(|h| tctx.resolve(h)) {
                Some(t) if t.is_float() => t,
                _ => Type::Float64,
            },
        }),
        Expr::Var { name } => match env.get(name) {
            Some(t) => Some(t.clone()),
            None => match tctx.enum_for_variant(name) {
                Some(enum_name) => Some(Type::Named(enum_name.to_string())),
                None => {
                    diags.push(
                        Diagnostic::error("E0080", format!("unknown identifier `{name}`"))
                            .with_context(ctx.to_string()),
                    );
                    None
                }
            },
        },
        Expr::Unary { op, arg } => {
            // -Const{n} is a signed integer literal; type it directly so the
            // hint applies to the signed value rather than the unsigned magnitude.
            if let (UnaryOp::Neg, Expr::Const { lit: Literal::Int { value } }) = (op, arg.as_ref()) {
                return Some(integer_literal_type(-*value, hint, tctx));
            }
            let a = infer_expr_type(arg, env, sigs, node, diags, ctx, tctx, hint)?;
            match op {
                UnaryOp::Not => {
                    if !tctx.resolve(&a).is_bool() {
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
                    if !tctx.resolve(&a).is_numeric() {
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
            // Logical and equality ops use a bool hint for sub-terms only
            // when the op preserves bool; arithmetic/comparison ops forward
            // the surrounding hint so integer literals adopt the target type.
            let sub_hint = match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => hint,
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Neq => None,
                BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Implies => None,
            };
            let l = infer_expr_type(lhs, env, sigs, node, diags, ctx, tctx, sub_hint)?;
            // If LHS is a typed integer, pass it as a hint so a literal RHS
            // takes the same width.
            let rhs_hint = match (&l, sub_hint) {
                (lt, _) if lt.is_integer() => Some(lt.clone()),
                _ => sub_hint.cloned(),
            };
            let r = infer_expr_type(rhs, env, sigs, node, diags, ctx, tctx, rhs_hint.as_ref())?;
            let lr = tctx.resolve(&l);
            let rr = tctx.resolve(&r);
            match op {
                BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Implies => {
                    if !(lr.is_bool() && rr.is_bool()) {
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
                    if !types_compatible(tctx, &l, &r) {
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
                    if !(lr.is_numeric() && rr.is_numeric() && types_compatible(tctx, &l, &r)) {
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
                    if !(lr.is_numeric() && rr.is_numeric() && types_compatible(tctx, &l, &r)) {
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
            let c = infer_expr_type(cond, env, sigs, node, diags, ctx, tctx, Some(&Type::Bool))?;
            if !tctx.resolve(&c).is_bool() {
                diags.push(
                    Diagnostic::error("E0090", format!("if-condition must be bool, got {c:?}"))
                        .with_context(ctx.to_string()),
                );
                return None;
            }
            let t = infer_expr_type(then_branch, env, sigs, node, diags, ctx, tctx, hint)?;
            // Hint the else branch with the then-branch's type so literals on
            // one side match a typed value on the other.
            let else_hint = if hint.is_some() { hint } else { Some(&t) };
            let e = infer_expr_type(else_branch, env, sigs, node, diags, ctx, tctx, else_hint)?;
            if !types_compatible(tctx, &t, &e) {
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
        Expr::Pre { arg } => infer_expr_type(arg, env, sigs, node, diags, ctx, tctx, hint),
        Expr::Arrow { init, body } => {
            let i = infer_expr_type(init, env, sigs, node, diags, ctx, tctx, hint)?;
            let body_hint = if hint.is_some() { hint } else { Some(&i) };
            let b = infer_expr_type(body, env, sigs, node, diags, ctx, tctx, body_hint)?;
            if !types_compatible(tctx, &i, &b) {
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
                if let Some(t) =
                    infer_expr_type(a, env, sigs, node, diags, ctx, tctx, Some(&p.ty))
                {
                    if !types_compatible(tctx, &p.ty, &t) {
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
            let bt = infer_expr_type(base, env, sigs, node, diags, ctx, tctx, None)?;
            let resolved = tctx.resolve(&bt);
            match resolved {
                Type::Named(ref rec_name) => match tctx.record_fields(rec_name) {
                    Some(fields) => match fields.iter().find(|f| f.name == *field) {
                        Some(f) => Some(f.ty.clone()),
                        None => {
                            diags.push(
                                Diagnostic::error(
                                    "E0120",
                                    format!(
                                        "record `{rec_name}` has no field `{field}`"
                                    ),
                                )
                                .with_context(ctx.to_string()),
                            );
                            None
                        }
                    },
                    None => {
                        diags.push(
                            Diagnostic::error(
                                "E0121",
                                format!(
                                    "cannot access field `{field}`: `{rec_name}` is not a record type"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        None
                    }
                },
                other => {
                    diags.push(
                        Diagnostic::error(
                            "E0122",
                            format!("cannot access field `{field}` on non-record type {other:?}"),
                        )
                        .with_context(ctx.to_string()),
                    );
                    None
                }
            }
        }
        Expr::Index { base, index } => {
            let bt = infer_expr_type(base, env, sigs, node, diags, ctx, tctx, None)?;
            // Array index must be an integer expression; default-hint Int32
            // so literal indices like `xs[3]` type correctly without
            // forcing the user to annotate.
            let it = infer_expr_type(
                index,
                env,
                sigs,
                node,
                diags,
                ctx,
                tctx,
                Some(&Type::Int32),
            )?;
            if !tctx.resolve(&it).is_integer() {
                diags.push(
                    Diagnostic::error(
                        "E0111",
                        format!("array index must be an integer, got {it:?}"),
                    )
                    .with_context(ctx.to_string()),
                );
            }
            match tctx.resolve(&bt) {
                Type::Array { elem, .. } => Some(*elem),
                other => {
                    diags.push(
                        Diagnostic::error(
                            "E0110",
                            format!("indexing a non-array of type {other:?}"),
                        )
                        .with_context(ctx.to_string()),
                    );
                    None
                }
            }
        }
        Expr::Tuple { .. } => Some(Type::Named("__tuple__".into())),
    }
}

fn integer_literal_type(value: i64, hint: Option<&Type>, tctx: &TypeContext) -> Type {
    if let Some(h) = hint {
        let resolved = tctx.resolve(h);
        if resolved.is_integer() && fits_in_integer(value, &resolved) {
            return resolved;
        }
    }
    Type::Int32
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
