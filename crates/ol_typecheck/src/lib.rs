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
    /// Names of project-wide constants, with their declared types. Looked up
    /// after the local env so a local always shadows a const of the same
    /// name.
    constants: HashMap<String, Type>,
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
            for c in &pkg.constants {
                ctx.constants.insert(c.name.clone(), c.ty.clone());
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
                Type::Named { name: ref n } => match self.aliases.get(n) {
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

    pub fn enum_variants(&self, name: &str) -> Option<&Vec<String>> {
        self.enums.get(name)
    }

    pub fn const_type(&self, name: &str) -> Option<&Type> {
        self.constants.get(name)
    }
}

pub mod generics;
pub use generics::monomorphize;

pub fn check_project(project: &Project) -> CheckReport {
    let mut diags = Vec::new();
    let tctx = TypeContext::from_project(project);

    check_constants(project, &tctx, &mut diags);

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
        if generics::is_template(n) {
            // A generic TEMPLATE checks against a representative
            // instantiation (numeric/integer -> int32, float -> float64,
            // unconstrained vars stay opaque, sizes -> 3): most template
            // errors surface immediately, and every real instantiation is
            // fully re-checked after monomorphization anyway.
            let mut rep_t: BTreeMap<String, Type> = BTreeMap::new();
            let mut rep_s: BTreeMap<String, u32> = BTreeMap::new();
            for g in generics::effective_generics(n) {
                match g {
                    ol_ir::GenericParam::Type { name, constraint } => {
                        let rep = match constraint {
                            ol_ir::TypeConstraint::Numeric
                            | ol_ir::TypeConstraint::Integer => Some(Type::Int32),
                            ol_ir::TypeConstraint::Float => Some(Type::Float64),
                            ol_ir::TypeConstraint::Any => None, // stays opaque
                        };
                        if let Some(rep) = rep {
                            rep_t.insert(name, rep);
                        }
                    }
                    ol_ir::GenericParam::Size { name } => {
                        rep_s.insert(name, 3);
                    }
                }
            }
            let mut rep = n.clone();
            for pp in rep.inputs.iter_mut().chain(rep.outputs.iter_mut()) {
                pp.ty = pp.ty.substitute(&rep_t, &rep_s);
            }
            for l in &mut rep.locals {
                l.ty = l.ty.substitute(&rep_t, &rep_s);
            }
            check_node(&rep, &signatures, &tctx, &mut diags);
            continue;
        }
        check_node(n, &signatures, &tctx, &mut diags);
    }

    CheckReport { diagnostics: diags }
}

fn check_constants(project: &Project, tctx: &TypeContext, diags: &mut Vec<Diagnostic>) {
    // Synthesize a tiny anonymous node that lets us reuse infer_expr_type for
    // constant-value typechecking. Const RHS can reference other constants
    // and enum variants but nothing in `env`.
    let dummy_node = NodeDef {
        name: "<consts>".into(),
        kind: NodeKind::Function,
        inputs: vec![],
        outputs: vec![],
        locals: vec![],
        equations: vec![],
        contract: None,
        diagram: Default::default(),
        probes: Vec::new(),
        requirements: Vec::new(),
        sysml: None,
        generics: vec![],
    };
    let empty_env: BTreeMap<String, Type> = BTreeMap::new();
    let empty_sigs: HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)> = HashMap::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for pkg in &project.packages {
        for c in &pkg.constants {
            let ctx = format!("constant {}", c.name);
            if !seen.insert(c.name.clone()) {
                diags.push(
                    Diagnostic::error("E0003", format!("duplicate constant `{}`", c.name))
                        .with_context(ctx.clone()),
                );
                continue;
            }
            if c.value.contains_temporal() {
                diags.push(
                    Diagnostic::error(
                        "E0004",
                        "constant values may not use temporal operators (`pre`, `->`)",
                    )
                    .with_context(ctx.clone()),
                );
            }
            if let Some(inferred) = infer_expr_type(
                &c.value,
                &empty_env,
                &empty_sigs,
                &dummy_node,
                diags,
                &ctx,
                tctx,
                Some(&c.ty),
            ) {
                if !types_compatible(tctx, &c.ty, &inferred) {
                    diags.push(
                        Diagnostic::error(
                            "E0005",
                            format!(
                                "constant `{}` declared as {:?} but value has type {:?}",
                                c.name, c.ty, inferred
                            ),
                        )
                        .with_context(ctx),
                    );
                }
            }
        }
    }
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
    for (eq_i, eq) in node.equations.iter().enumerate() {
        // Per-equation context: lets a GUI map any in-equation diagnostic
        // onto the exact diagram box that caused it.
        let eq_ctx = format!("{ctx} · equation {eq_i}");
        for lhs in &eq.lhs {
            if !env.contains_key(lhs) {
                diags.push(
                    Diagnostic::error("E0020", format!("equation defines unknown name `{lhs}`"))
                        .with_context(eq_ctx.clone()),
                );
            }
            if !assigned.insert(lhs.clone()) {
                diags.push(
                    Diagnostic::error(
                        "E0021",
                        format!("name `{lhs}` is assigned by more than one equation"),
                    )
                    .with_context(eq_ctx.clone()),
                );
            }
        }

        if node.is_function() && eq.rhs.contains_temporal() {
            diags.push(
                Diagnostic::error(
                    "E0030",
                    "function bodies may not use temporal operators (`pre`, `->`)",
                )
                .with_context(eq_ctx.clone()),
            );
        }

        check_pre_initialization(&eq.rhs, false, diags, &eq_ctx);
        check_iterator_placement(&eq.rhs, diags, &eq_ctx);
        check_mapfold_lhs(eq, &env, sigs, diags, &eq_ctx, tctx);

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
            &eq_ctx,
            tctx,
            lhs_hint.as_ref(),
        );

        if let Some(rhs_ty) = inferred {
            match eq.lhs.len() {
                1 => {
                    if let Some(expected) = env.get(&eq.lhs[0]) {
                        let is_tuple = matches!(&rhs_ty, Type::Named { name } if name == "__tuple__");
                        if !types_compatible(tctx, expected, &rhs_ty) && !is_tuple {
                            diags.push(
                                Diagnostic::error(
                                    "E0040",
                                    format!(
                                        "equation `{} = ...` has type {:?} but `{}` is declared as {:?}",
                                        eq.lhs[0], rhs_ty, eq.lhs[0], expected
                                    ),
                                )
                                .with_context(eq_ctx.clone()),
                            );
                        }
                    }
                }
                _ => {
                    let is_tuple = matches!(&rhs_ty, Type::Named { name } if name == "__tuple__");
                    if !is_tuple {
                        diags.push(
                            Diagnostic::error(
                                "E0041",
                                "multi-output equation must bind to a node call returning a tuple",
                            )
                            .with_context(eq_ctx.clone()),
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

    // Debug log probes must name a real variable in the node.
    for p in &node.probes {
        if !env.contains_key(&p.var) {
            diags.push(
                Diagnostic::error(
                    "E0150",
                    format!("log message references unknown variable `{}`", p.var),
                )
                .with_context(ctx.clone()),
            );
        }
    }

    // Clock discipline: every operand on one clock, clock variables sampling
    // the clock they live on, outputs back on the base clock. The same
    // inference drives the simulator and the C emitter, so anything that
    // passes here executes identically in both.
    if ol_ir::node_uses_clocks(node) {
        let cinfo = ol_ir::infer_clocks(node);
        for e in &cinfo.errors {
            let ectx = match e.equation {
                Some(i) => format!("{ctx} · equation {i}"),
                None => ctx.clone(),
            };
            diags.push(Diagnostic::error("E0132", e.message.clone()).with_context(ectx));
        }
        for (i, ck) in cinfo.equation_clocks.iter().enumerate() {
            if ck.is_base() {
                continue;
            }
            // A clocked equation holds its lhs through inactive cycles —
            // that is state, which functions do not have.
            if node.is_function() {
                diags.push(
                    Diagnostic::error(
                        "E0134",
                        format!(
                            "function `{}` has an equation on {} — holding a value \
                             through inactive cycles is state; make this a node",
                            node.name,
                            ck.describe()
                        ),
                    )
                    .with_context(format!("{ctx} · equation {i}")),
                );
            }
            // Held values are plain C assignments; arrays cannot be assigned.
            for l in &node.equations[i].lhs {
                if let Some(t) = env.get(l) {
                    if matches!(tctx.resolve(t), Type::Array { .. }) {
                        diags.push(
                            Diagnostic::error(
                                "E0135",
                                format!(
                                    "`{l}` has an array type and cannot be clocked yet — \
                                     held array values are roadmap"
                                ),
                            )
                            .with_context(format!("{ctx} · equation {i}")),
                        );
                    }
                }
            }
        }
        // Stateful callees must run on their whole equation's clock: that is
        // the granularity the generated C can guard. Finer placement (a node
        // call inside one merge branch) would silently step state on the
        // wrong cycles in C, so it is rejected loudly here.
        let mut call_kinds: HashMap<usize, String> = HashMap::new();
        for (i, eq) in node.equations.iter().enumerate() {
            eq.rhs.visit(|e| {
                if let Expr::Call { node: callee, .. } = e {
                    if matches!(sigs.get(callee), Some((_, _, NodeKind::Operator))) {
                        call_kinds.insert(e as *const Expr as usize, format!("{i}:{callee}"));
                    }
                }
            });
        }
        for (site, tag) in &call_kinds {
            if let Some(call_clock) = cinfo.call_clocks.get(site) {
                let (i, callee) = tag.split_once(':').unwrap_or(("0", tag));
                let eq_i: usize = i.parse().unwrap_or(0);
                if let Some(eq_clock) = cinfo.equation_clocks.get(eq_i) {
                    if call_clock != eq_clock {
                        diags.push(
                            Diagnostic::error(
                                "E0133",
                                format!(
                                    "stateful operator `{callee}` is called on {} inside an \
                                     equation on {} — move the call into its own equation \
                                     so its activation clock is explicit",
                                    call_clock.describe(),
                                    eq_clock.describe()
                                ),
                            )
                            .with_context(format!("{ctx} · equation {eq_i}")),
                        );
                    }
                }
            }
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
        Expr::Last { .. } => {}
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
        Expr::Unary { arg, .. } | Expr::Cast { arg, .. } => {
            check_pre_initialization(arg, under_arrow_body, diags, ctx)
        }
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
        Expr::Tuple { items } | Expr::Array { items } => {
            for i in items {
                check_pre_initialization(i, under_arrow_body, diags, ctx);
            }
        }
        Expr::Struct { fields, .. } => {
            for fi in fields {
                check_pre_initialization(&fi.value, under_arrow_body, diags, ctx);
            }
        }
        // Sampling does not initialize anything: a `pre` under a `when` or a
        // merge branch still needs its own `->`.
        Expr::When { arg, .. } => check_pre_initialization(arg, under_arrow_body, diags, ctx),
        Expr::Merge { on_true, on_false, .. } => {
            check_pre_initialization(on_true, under_arrow_body, diags, ctx);
            check_pre_initialization(on_false, under_arrow_body, diags, ctx);
        }
        Expr::Iterate { init, arrays, .. } => {
            if let Some(i) = init {
                check_pre_initialization(i, under_arrow_body, diags, ctx);
            }
            for a in arrays {
                check_pre_initialization(a, under_arrow_body, diags, ctx);
            }
        }
        Expr::FloatIntrinsic { args, .. }
        | Expr::ArrayOp { args, .. }
        | Expr::Printout { args }
        | Expr::Sharp { args } => {
            for a in args {
                check_pre_initialization(a, under_arrow_body, diags, ctx);
            }
        }
        Expr::DynIndex { base, index, default } => {
            check_pre_initialization(base, under_arrow_body, diags, ctx);
            check_pre_initialization(index, under_arrow_body, diags, ctx);
            check_pre_initialization(default, under_arrow_body, diags, ctx);
        }
        Expr::Replicate { value, size } => {
            check_pre_initialization(value, under_arrow_body, diags, ctx);
            check_pre_initialization(size, under_arrow_body, diags, ctx);
        }
        Expr::Slice { base, lo, hi } => {
            check_pre_initialization(base, under_arrow_body, diags, ctx);
            check_pre_initialization(lo, under_arrow_body, diags, ctx);
            check_pre_initialization(hi, under_arrow_body, diags, ctx);
        }
        Expr::Transpose { base } => check_pre_initialization(base, under_arrow_body, diags, ctx),
        Expr::Update { base, index, value, .. } => {
            check_pre_initialization(base, under_arrow_body, diags, ctx);
            if let Some(i) = index {
                check_pre_initialization(i, under_arrow_body, diags, ctx);
            }
            check_pre_initialization(value, under_arrow_body, diags, ctx);
        }
        Expr::Case { sel, arms, default } => {
            check_pre_initialization(sel, under_arrow_body, diags, ctx);
            for arm in arms {
                check_pre_initialization(&arm.value, under_arrow_body, diags, ctx);
            }
            if let Some(d) = default {
                check_pre_initialization(d, under_arrow_body, diags, ctx);
            }
        }
        Expr::Const { .. } | Expr::Var { .. } => {}
    }
}

/// If a type hint is an array, the hint for its element (so `replicate(v, n)`
/// under an `int8[]` hint types `v` as `int8`). `None` otherwise.
fn hint_elem(hint: Option<&Type>) -> Option<&Type> {
    match hint {
        Some(Type::Array { elem, .. }) => Some(elem),
        _ => None,
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
        Expr::Last { name } => {
            diags.push(
                Diagnostic::error(
                    "E0180",
                    format!(
                        "`last {name}` is only allowed inside a state machine — \
                         it resolves to the previous cycle's value when the machine lowers"
                    ),
                )
                .with_context(ctx.to_string()),
            );
            None
        }
        Expr::Const { lit } => Some(match lit {
            Literal::Bool { .. } => Type::Bool,
            Literal::Int { value } => integer_literal_type(*value, hint, tctx),
            Literal::Float { .. } => match hint.map(|h| tctx.resolve(h)) {
                Some(t) if t.is_float() => t,
                _ => Type::Float64,
            },
            Literal::Char { .. } => Type::Char,
        }),
        Expr::Cast { to, arg } => {
            let a = infer_expr_type(arg, env, sigs, node, diags, ctx, tctx, None)?;
            let ar = tctx.resolve(&a);
            // Casts bridge numeric and fixed-point types (both directions);
            // a fixed operand/target rescales by 2^frac on the way through.
            if !(ar.is_numeric() || ar.is_fixed()) {
                diags.push(
                    Diagnostic::error(
                        "E0093",
                        format!("numeric_cast requires a numeric or fixed-point operand, got {a:?}"),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            if !(to.is_numeric() || to.is_fixed()) {
                diags.push(
                    Diagnostic::error(
                        "E0094",
                        format!("numeric_cast target must be numeric or fixed-point, got {to:?}"),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            if let Type::Fixed { bits, frac, .. } = to {
                if to.fixed_storage().is_none() || *frac >= *bits {
                    diags.push(
                        Diagnostic::error(
                            "E0095",
                            format!(
                                "invalid fixed-point type {to:?}: `bits` must be 8/16/32/64 and \
                                 `frac` < `bits`"
                            ),
                        )
                        .with_context(ctx.to_string()),
                    );
                    return None;
                }
            }
            Some(to.clone())
        }
        Expr::FloatIntrinsic { op, args, single } => {
            let name = if *single { op.single_name() } else { op.name().to_string() };
            let want = if *single { Type::Float32 } else { Type::Float64 };
            let want_name = if *single { "float32" } else { "float64" };
            if args.len() != op.arity() {
                diags.push(
                    Diagnostic::error(
                        "E0160",
                        format!(
                            "`{name}` takes {} argument{}, got {}",
                            op.arity(),
                            if op.arity() == 1 { "" } else { "s" },
                            args.len()
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            let mut ok = true;
            for a in args {
                let t = infer_expr_type(a, env, sigs, node, diags, ctx, tctx, Some(&want));
                match t {
                    Some(t) if tctx.resolve(&t) == want => {}
                    Some(t) => {
                        diags.push(
                            Diagnostic::error(
                                "E0161",
                                format!(
                                    "`{name}` requires {want_name} operands, got {t:?} — cast \
                                     explicitly, e.g. `{name}({want_name}(x))`"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        ok = false;
                    }
                    None => ok = false,
                }
            }
            if ok {
                Some(want)
            } else {
                None
            }
        }
        // printout: declared scalar variables in, the special bool
        // `terminal_out` value out. E0149 covers every misuse.
        Expr::Printout { args } => {
            if args.is_empty() || args.len() > 12 {
                diags.push(
                    Diagnostic::error(
                        "E0149",
                        format!("printout takes 1 to 12 inputs, got {}", args.len()),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            let mut ok = true;
            for a in args {
                let Expr::Var { name } = a else {
                    diags.push(
                        Diagnostic::error(
                            "E0149",
                            "printout inputs must be declared variables (wire a signal \
                             to each pin), not expressions",
                        )
                        .with_context(ctx.to_string()),
                    );
                    ok = false;
                    continue;
                };
                match env.get(name) {
                    Some(t) if matches!(tctx.resolve(t), Type::Bool)
                        || tctx.resolve(t).is_numeric() => {}
                    Some(t) => {
                        diags.push(
                            Diagnostic::error(
                                "E0149",
                                format!(
                                    "printout input `{name}` has type {t:?} — only \
                                     bool/integer/float signals print in this profile"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        ok = false;
                    }
                    None => {
                        diags.push(
                            Diagnostic::error(
                                "E0149",
                                format!("printout input `{name}` is not a declared variable"),
                            )
                            .with_context(ctx.to_string()),
                        );
                        ok = false;
                    }
                }
            }
            if ok { Some(Type::Bool) } else { None }
        }
        // concat/reverse: array-shape algebra. E0148 covers every misuse.
        Expr::ArrayOp { op, args } => {
            if args.len() != op.arity() {
                diags.push(
                    Diagnostic::error(
                        "E0148",
                        format!("`{}` takes {} argument(s), got {}", op.name(), op.arity(), args.len()),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            let mut shapes: Vec<(Type, u32)> = Vec::new();
            for a in args {
                let t = infer_expr_type(a, env, sigs, node, diags, ctx, tctx, None)?;
                match tctx.resolve(&t) {
                    Type::Array { elem, len } => shapes.push((*elem, len)),
                    other => {
                        diags.push(
                            Diagnostic::error(
                                "E0148",
                                format!("`{}` operands must be arrays, got {other:?}", op.name()),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                }
            }
            match op {
                ol_ir::ArrayOpKind::Concat => {
                    if shapes[0].0 != shapes[1].0 {
                        diags.push(
                            Diagnostic::error(
                                "E0148",
                                format!(
                                    "concat operands must share one element type, got {:?} and {:?}",
                                    shapes[0].0, shapes[1].0
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                    Some(Type::Array {
                        elem: Box::new(shapes[0].0.clone()),
                        len: shapes[0].1 + shapes[1].1,
                    })
                }
                ol_ir::ArrayOpKind::Reverse => Some(Type::Array {
                    elem: Box::new(shapes[0].0.clone()),
                    len: shapes[0].1,
                }),
            }
        }
        // SCADE `#(a, b, …)`: every operand boolean, result boolean.
        Expr::Sharp { args } => {
            for a in args {
                if let Some(t) = infer_expr_type(a, env, sigs, node, diags, ctx, tctx, Some(&Type::Bool)) {
                    if tctx.resolve(&t) != Type::Bool {
                        diags.push(
                            Diagnostic::error(
                                "E0195",
                                format!("`#` operands must be boolean, got {t:?}"),
                            )
                            .with_context(ctx.to_string()),
                        );
                    }
                }
            }
            Some(Type::Bool)
        }
        // SCADE bounds-safe dynamic projection `(base.[i] default d)`: element
        // type of `base`, index integer, default of the element type.
        Expr::DynIndex { base, index, default } => {
            let bt = infer_expr_type(base, env, sigs, node, diags, ctx, tctx, None)?;
            let elem = match tctx.resolve(&bt) {
                Type::Array { elem, .. } => *elem,
                other => {
                    diags.push(
                        Diagnostic::error(
                            "E0196",
                            format!("dynamic projection `.[i]` needs an array, got {other:?}"),
                        )
                        .with_context(ctx.to_string()),
                    );
                    return None;
                }
            };
            let it = infer_expr_type(index, env, sigs, node, diags, ctx, tctx, Some(&Type::Int32))?;
            if !tctx.resolve(&it).is_integer() {
                diags.push(
                    Diagnostic::error("E0196", format!("dynamic index must be an integer, got {it:?}"))
                        .with_context(ctx.to_string()),
                );
            }
            if let Some(dt) = infer_expr_type(default, env, sigs, node, diags, ctx, tctx, Some(&elem)) {
                if !types_compatible(tctx, &elem, &dt) {
                    diags.push(
                        Diagnostic::error(
                            "E0196",
                            format!("dynamic projection default must match the element type {elem:?}, got {dt:?}"),
                        )
                        .with_context(ctx.to_string()),
                    );
                }
            }
            Some(elem)
        }
        // SCADE replication `replicate(v, n)`: an array of `n` copies of `v`.
        Expr::Replicate { value, size } => {
            let vt = infer_expr_type(value, env, sigs, node, diags, ctx, tctx, hint_elem(hint))?;
            let n = match size.const_int() {
                Some(n) if n >= 0 => n as u32,
                _ => {
                    diags.push(
                        Diagnostic::error(
                            "E0197",
                            "replication size must be a non-negative compile-time constant".to_string(),
                        )
                        .with_context(ctx.to_string()),
                    );
                    return None;
                }
            };
            Some(Type::Array { elem: Box::new(vt), len: n })
        }
        // SCADE slice `base[lo .. hi]`: sub-array, inclusive bounds.
        Expr::Slice { base, lo, hi } => {
            let bt = infer_expr_type(base, env, sigs, node, diags, ctx, tctx, None)?;
            let (elem, len) = match tctx.resolve(&bt) {
                Type::Array { elem, len } => (*elem, len),
                other => {
                    diags.push(
                        Diagnostic::error("E0198", format!("slice needs an array, got {other:?}"))
                            .with_context(ctx.to_string()),
                    );
                    return None;
                }
            };
            let (lo_v, hi_v) = match (lo.const_int(), hi.const_int()) {
                (Some(a), Some(b)) => (a, b),
                _ => {
                    diags.push(
                        Diagnostic::error("E0198", "slice bounds must be compile-time constants".to_string())
                            .with_context(ctx.to_string()),
                    );
                    return None;
                }
            };
            if lo_v < 0 || hi_v < lo_v || hi_v >= len as i64 {
                diags.push(
                    Diagnostic::error(
                        "E0198",
                        format!("slice [{lo_v} .. {hi_v}] is out of range for an array of length {len}"),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            Some(Type::Array { elem: Box::new(elem), len: (hi_v - lo_v + 1) as u32 })
        }
        // SCADE transpose: `T[m][n]` (array of m rows of n) → `T[n][m]`.
        Expr::Transpose { base } => {
            let bt = infer_expr_type(base, env, sigs, node, diags, ctx, tctx, None)?;
            match tctx.resolve(&bt) {
                Type::Array { elem, len: m } => match tctx.resolve(&elem) {
                    Type::Array { elem: inner, len: n } => Some(Type::Array {
                        elem: Box::new(Type::Array { elem: inner, len: m }),
                        len: n,
                    }),
                    other => {
                        diags.push(
                            Diagnostic::error(
                                "E0199",
                                format!("transpose needs a 2-D array (array of arrays), got rows of {other:?}"),
                            )
                            .with_context(ctx.to_string()),
                        );
                        None
                    }
                },
                other => {
                    diags.push(
                        Diagnostic::error("E0199", format!("transpose needs an array, got {other:?}"))
                            .with_context(ctx.to_string()),
                    );
                    None
                }
            }
        }
        // SCADE functional update `(base with [i] = v)` / `(base with .f = v)`:
        // same type as `base`, with the element/field position's type checked.
        Expr::Update { base, index, field, value } => {
            let bt = infer_expr_type(base, env, sigs, node, diags, ctx, tctx, hint)?;
            match (index, field) {
                (Some(idx), None) => {
                    let elem = match tctx.resolve(&bt) {
                        Type::Array { elem, .. } => *elem,
                        other => {
                            diags.push(
                                Diagnostic::error(
                                    "E0200",
                                    format!("`with [i] = v` needs an array base, got {other:?}"),
                                )
                                .with_context(ctx.to_string()),
                            );
                            return None;
                        }
                    };
                    let it = infer_expr_type(idx, env, sigs, node, diags, ctx, tctx, Some(&Type::Int32))?;
                    if !tctx.resolve(&it).is_integer() {
                        diags.push(
                            Diagnostic::error("E0200", format!("update index must be an integer, got {it:?}"))
                                .with_context(ctx.to_string()),
                        );
                    }
                    if let Some(vt) = infer_expr_type(value, env, sigs, node, diags, ctx, tctx, Some(&elem)) {
                        if !types_compatible(tctx, &elem, &vt) {
                            diags.push(
                                Diagnostic::error(
                                    "E0200",
                                    format!("update value must match the element type {elem:?}, got {vt:?}"),
                                )
                                .with_context(ctx.to_string()),
                            );
                        }
                    }
                    Some(bt)
                }
                (None, Some(fname)) => {
                    let rec_name = match tctx.resolve(&bt) {
                        Type::Named { name } if tctx.record_fields(&name).is_some() => name,
                        other => {
                            diags.push(
                                Diagnostic::error(
                                    "E0200",
                                    format!("`with .{fname} = v` needs a record base, got {other:?}"),
                                )
                                .with_context(ctx.to_string()),
                            );
                            return None;
                        }
                    };
                    let schema = tctx.record_fields(&rec_name).cloned().unwrap_or_default();
                    match schema.iter().find(|rf| &rf.name == fname) {
                        Some(rf) => {
                            if let Some(vt) = infer_expr_type(value, env, sigs, node, diags, ctx, tctx, Some(&rf.ty)) {
                                if !types_compatible(tctx, &rf.ty, &vt) {
                                    diags.push(
                                        Diagnostic::error(
                                            "E0200",
                                            format!("update of `{rec_name}.{fname}` expects {:?}, got {vt:?}", rf.ty),
                                        )
                                        .with_context(ctx.to_string()),
                                    );
                                }
                            }
                        }
                        None => {
                            diags.push(
                                Diagnostic::error(
                                    "E0200",
                                    format!("record `{rec_name}` has no field `{fname}`"),
                                )
                                .with_context(ctx.to_string()),
                            );
                        }
                    }
                    Some(bt)
                }
                _ => {
                    diags.push(
                        Diagnostic::error("E0200", "update needs exactly one of [index] or .field".to_string())
                            .with_context(ctx.to_string()),
                    );
                    None
                }
            }
        }
        Expr::Case { sel, arms, default } => {
            let sel_t = infer_expr_type(sel, env, sigs, node, diags, ctx, tctx, None)?;
            let enum_name = match tctx.resolve(&sel_t) {
                Type::Named { name } if tctx.enum_variants(&name).is_some() => name,
                other => {
                    diags.push(
                        Diagnostic::error(
                            "E0170",
                            format!("`case` selects on an enum, got {other:?}"),
                        )
                        .with_context(ctx.to_string()),
                    );
                    return None;
                }
            };
            let variants = tctx.enum_variants(&enum_name).cloned().unwrap_or_default();
            let mut seen: Vec<&str> = Vec::new();
            for arm in arms {
                if !variants.iter().any(|v| v == &arm.variant) {
                    diags.push(
                        Diagnostic::error(
                            "E0171",
                            format!(
                                "`case` arm `{}` is not a variant of enum `{enum_name}` \
                                 (variants: {})",
                                arm.variant,
                                variants.join(", ")
                            ),
                        )
                        .with_context(ctx.to_string()),
                    );
                }
                if seen.contains(&arm.variant.as_str()) {
                    diags.push(
                        Diagnostic::error(
                            "E0172",
                            format!("`case` arm `{}` appears twice", arm.variant),
                        )
                        .with_context(ctx.to_string()),
                    );
                }
                seen.push(&arm.variant);
            }
            if default.is_none() {
                let missing: Vec<&str> = variants
                    .iter()
                    .map(|v| v.as_str())
                    .filter(|v| !seen.contains(v))
                    .collect();
                if !missing.is_empty() {
                    diags.push(
                        Diagnostic::error(
                            "E0173",
                            format!(
                                "`case` on `{enum_name}` is not exhaustive: missing {} — \
                                 add the arm(s) or a `_:` default",
                                missing.join(", ")
                            ),
                        )
                        .with_context(ctx.to_string()),
                    );
                }
            }
            // All arms (and the default) must agree on one result type.
            let mut result: Option<Type> = None;
            let values = arms
                .iter()
                .map(|a| &a.value)
                .chain(default.as_deref());
            for v in values {
                let t = infer_expr_type(v, env, sigs, node, diags, ctx, tctx, hint.or(result.as_ref()))?;
                match &result {
                    None => result = Some(t),
                    Some(r) if types_compatible(tctx, r, &t) => {}
                    Some(r) => {
                        diags.push(
                            Diagnostic::error(
                                "E0174",
                                format!("`case` arms disagree in type: {r:?} vs {t:?}"),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                }
            }
            result
        }
        Expr::Var { name } => match env.get(name) {
            Some(t) => Some(t.clone()),
            None => match tctx.const_type(name) {
                Some(t) => Some(t.clone()),
                None => match tctx.enum_for_variant(name) {
                    Some(enum_name) => Some(Type::named(enum_name)),
                    None => {
                        diags.push(
                            Diagnostic::error("E0080", format!("unknown identifier `{name}`"))
                                .with_context(ctx.to_string()),
                        );
                        None
                    }
                },
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
                BinOp::SatAdd | BinOp::SatSub | BinOp::SatMul | BinOp::SatDiv => hint,
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => hint,
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
                    // Fixed-point orders on its stored integer, so it is ordered
                    // alongside the plain numeric types (operands must match).
                    let orderable = (lr.is_numeric() || lr.is_fixed())
                        && (rr.is_numeric() || rr.is_fixed());
                    if !(orderable && types_compatible(tctx, &l, &r)) {
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
                    if lr.is_fixed() || rr.is_fixed() {
                        // Fixed-point arithmetic is integer ops on the stored
                        // value, so both sides must be the SAME fixed type
                        // (cast to align a literal or a differing Q-format).
                        if !(lr.is_fixed() && rr.is_fixed() && types_compatible(tctx, &l, &r)) {
                            diags.push(
                                Diagnostic::error(
                                    "E0086",
                                    format!(
                                        "fixed-point arithmetic requires matching fixed-point \
                                         operands, got {l:?} and {r:?} (cast to align formats)"
                                    ),
                                )
                                .with_context(ctx.to_string()),
                            );
                            return None;
                        }
                        if matches!(op, BinOp::Mod) {
                            diags.push(
                                Diagnostic::error(
                                    "E0088",
                                    "fixed-point modulo is not supported; cast to an integer \
                                     type first"
                                        .to_string(),
                                )
                                .with_context(ctx.to_string()),
                            );
                            return None;
                        }
                        return Some(l);
                    }
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
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                    if !(lr.is_integer() && rr.is_integer() && types_compatible(tctx, &l, &r)) {
                        diags.push(
                            Diagnostic::error(
                                "E0087",
                                format!(
                                    "bitwise operator requires matching integer operands, got {l:?} and {r:?}"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                    Some(l)
                }
                BinOp::SatAdd | BinOp::SatSub | BinOp::SatMul | BinOp::SatDiv => {
                    // Saturating arithmetic clamps to the type's range; defined
                    // only for fixed-point, where both operands share a format.
                    if !(lr.is_fixed() && rr.is_fixed() && types_compatible(tctx, &l, &r)) {
                        diags.push(
                            Diagnostic::error(
                                "E0089",
                                format!(
                                    "saturating operators require matching fixed-point operands, \
                                     got {l:?} and {r:?}"
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
                0 => Some(Type::named("__unit__")),
                1 => Some(outputs[0].ty.clone()),
                _ => Some(Type::named("__tuple__")),
            }
        }
        Expr::Field { base, field } => {
            let bt = infer_expr_type(base, env, sigs, node, diags, ctx, tctx, None)?;
            let resolved = tctx.resolve(&bt);
            match resolved {
                Type::Named { name: ref rec_name } => match tctx.record_fields(rec_name) {
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
        Expr::Array { items } => {
            // If the array is hinted `T[n]`, each element is hinted `T`.
            let elem_hint = match hint.map(|h| tctx.resolve(h)) {
                Some(Type::Array { elem, .. }) => Some((*elem).clone()),
                _ => None,
            };
            if items.is_empty() {
                return match elem_hint {
                    Some(e) => Some(Type::Array { elem: Box::new(e), len: 0 }),
                    None => {
                        diags.push(
                            Diagnostic::error(
                                "E0123",
                                "empty array literal needs a type annotation to fix its element type",
                            )
                            .with_context(ctx.to_string()),
                        );
                        None
                    }
                };
            }
            let mut elem_ty: Option<Type> = None;
            for it in items {
                let t = infer_expr_type(it, env, sigs, node, diags, ctx, tctx, elem_hint.as_ref())?;
                match &elem_ty {
                    None => elem_ty = Some(t),
                    Some(prev) if !types_compatible(tctx, prev, &t) => {
                        diags.push(
                            Diagnostic::error(
                                "E0124",
                                format!("array elements must share one type: {prev:?} vs {t:?}"),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                    Some(_) => {}
                }
            }
            Some(Type::Array {
                elem: Box::new(elem_ty.unwrap()),
                len: items.len() as u32,
            })
        }
        Expr::Struct { ty, fields } => {
            let rec_name = match tctx.resolve(&Type::named(ty.clone())) {
                Type::Named { name } => name,
                _ => ty.clone(),
            };
            match tctx.record_fields(&rec_name).cloned() {
                Some(schema) => {
                    for fi in fields {
                        match schema.iter().find(|rf| rf.name == fi.field) {
                            Some(rf) => {
                                if let Some(vt) = infer_expr_type(
                                    &fi.value, env, sigs, node, diags, ctx, tctx, Some(&rf.ty),
                                ) {
                                    if !types_compatible(tctx, &rf.ty, &vt) {
                                        diags.push(
                                            Diagnostic::error(
                                                "E0127",
                                                format!(
                                                    "record `{rec_name}` field `{}` expects {:?}, got {vt:?}",
                                                    fi.field, rf.ty
                                                ),
                                            )
                                            .with_context(ctx.to_string()),
                                        );
                                    }
                                }
                            }
                            None => {
                                diags.push(
                                    Diagnostic::error(
                                        "E0126",
                                        format!("record `{rec_name}` has no field `{}`", fi.field),
                                    )
                                    .with_context(ctx.to_string()),
                                );
                            }
                        }
                    }
                    // A record literal must initialize every field.
                    for rf in &schema {
                        if !fields.iter().any(|fi| fi.field == rf.name) {
                            diags.push(
                                Diagnostic::error(
                                    "E0128",
                                    format!("record `{rec_name}` literal is missing field `{}`", rf.name),
                                )
                                .with_context(ctx.to_string()),
                            );
                        }
                    }
                    Some(Type::named(rec_name))
                }
                None => {
                    diags.push(
                        Diagnostic::error(
                            "E0125",
                            format!("`{rec_name}` is not a record type, so `{rec_name} {{ … }}` is not a value"),
                        )
                        .with_context(ctx.to_string()),
                    );
                    None
                }
            }
        }
        Expr::Tuple { .. } => Some(Type::named("__tuple__")),
        Expr::When { arg, clock, .. } => {
            check_clock_var_is_bool(clock, env, node, diags, ctx, tctx);
            // Sampling changes the clock, not the data type.
            infer_expr_type(arg, env, sigs, node, diags, ctx, tctx, hint)
        }
        Expr::Merge { clock, on_true, on_false } => {
            check_clock_var_is_bool(clock, env, node, diags, ctx, tctx);
            let t = infer_expr_type(on_true, env, sigs, node, diags, ctx, tctx, hint)?;
            let else_hint = if hint.is_some() { hint } else { Some(&t) };
            let f = infer_expr_type(on_false, env, sigs, node, diags, ctx, tctx, else_hint)?;
            if !types_compatible(tctx, &t, &f) {
                diags.push(
                    Diagnostic::error(
                        "E0131",
                        format!("merge branches must agree in type, got {t:?} and {f:?}"),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            Some(t)
        }
        Expr::Iterate { kind, node: f_name, init, arrays } => {
            infer_iterator_type(*kind, f_name, init.as_deref(), arrays,
                env, sigs, node, diags, ctx, tctx)
        }
    }
}

/// Type an array iterator. The iterated `F` must be a stateless function
/// with one output; `map` needs one input per array and yields an array of
/// `F`'s output, `fold` needs `(accumulator, element)` and yields the
/// accumulator. Lengths of all array operands must agree.
#[allow(clippy::too_many_arguments)]
fn infer_iterator_type(
    kind: ol_ir::IterKind,
    f_name: &str,
    init: Option<&Expr>,
    arrays: &[Expr],
    env: &BTreeMap<String, Type>,
    sigs: &HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)>,
    node: &NodeDef,
    diags: &mut Vec<Diagnostic>,
    ctx: &str,
    tctx: &TypeContext,
) -> Option<Type> {
    use ol_ir::IterKind;
    let iter = match kind {
        IterKind::Map => "map",
        IterKind::Fold => "fold",
        IterKind::MapFold => "mapfold",
        IterKind::Mapi => "mapi",
        IterKind::Foldi => "foldi",
    };

    let Some((f_inputs, f_outputs, f_kind)) = sigs.get(f_name) else {
        diags.push(
            Diagnostic::error("E0140", format!("{iter} calls unknown function `{f_name}`"))
                .with_context(ctx.to_string()),
        );
        return None;
    };
    // Stateless only: a stateful body would need per-element state, which
    // this profile does not generate. Loud, not silent.
    if !matches!(f_kind, NodeKind::Function) {
        diags.push(
            Diagnostic::error(
                "E0141",
                format!(
                    "{iter} requires a stateless `function`, but `{f_name}` is a {f_kind:?} \
                     — iterating stateful operators is not supported yet"
                ),
            )
            .with_context(ctx.to_string()),
        );
        return None;
    }
    let want_outputs = if kind == IterKind::MapFold { 2 } else { 1 };
    // Indexed iterators pass the element index (an integer, 0-based) as F's
    // FIRST input; every rule below then applies to the remaining inputs.
    let indexed = matches!(kind, IterKind::Mapi | IterKind::Foldi);
    if f_outputs.len() != want_outputs {
        diags.push(
            Diagnostic::error(
                "E0142",
                format!(
                    "{iter}'s function `{f_name}` must have exactly {want_outputs} output{}",
                    if want_outputs == 1 { "" } else { "s (accumulator, element)" }
                ),
            )
            .with_context(ctx.to_string()),
        );
        return None;
    }
    if indexed {
        let ok = f_inputs.first().map(|p| {
            matches!(
                tctx.resolve(&p.ty),
                Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
                    | Type::Uint8 | Type::Uint16 | Type::Uint32 | Type::Uint64
            )
        });
        if ok != Some(true) {
            diags.push(
                Diagnostic::error(
                    "E0145",
                    format!(
                        "{iter}: `{f_name}`'s first input receives the element index \
                         and must be an integer type"
                    ),
                )
                .with_context(ctx.to_string()),
            );
            return None;
        }
    }
    let f_out = f_outputs[0].ty.clone();

    // Each array operand must be an array; collect (element type, length).
    let mut elems: Vec<Type> = Vec::new();
    let mut lengths: Vec<u32> = Vec::new();
    for a in arrays {
        let at = infer_expr_type(a, env, sigs, node, diags, ctx, tctx, None)?;
        match tctx.resolve(&at) {
            Type::Array { elem, len } => {
                elems.push(*elem);
                lengths.push(len);
            }
            other => {
                diags.push(
                    Diagnostic::error(
                        "E0143",
                        format!("{iter} operand must be an array, got {other:?}"),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
        }
    }
    if let Some(&first) = lengths.first() {
        if lengths.iter().any(|&l| l != first) {
            diags.push(
                Diagnostic::error(
                    "E0144",
                    format!("{iter}'s array operands must have equal length, got {lengths:?}"),
                )
                .with_context(ctx.to_string()),
            );
            return None;
        }
    }
    let n = lengths.first().copied().unwrap_or(0);

    match kind {
        IterKind::Map | IterKind::Mapi => {
            let idx = if indexed { 1 } else { 0 };
            if f_inputs.len() != arrays.len() + idx {
                diags.push(
                    Diagnostic::error(
                        "E0145",
                        format!(
                            "{iter}: `{f_name}` takes {} input(s) but {} array(s) were given{}",
                            f_inputs.len(),
                            arrays.len(),
                            if indexed { " (plus the index input)" } else { "" }
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            for (i, (p, e)) in f_inputs[idx..].iter().zip(elems.iter()).enumerate() {
                if !types_compatible(tctx, &p.ty, e) {
                    diags.push(
                        Diagnostic::error(
                            "E0145",
                            format!(
                                "map: array #{i} has element type {e:?} but `{f_name}` \
                                 expects {:?}",
                                p.ty
                            ),
                        )
                        .with_context(ctx.to_string()),
                    );
                    return None;
                }
            }
            Some(Type::Array { elem: Box::new(f_out), len: n })
        }
        IterKind::Fold | IterKind::Foldi => {
            // fold(F, init, a): F is (accumulator, element) -> accumulator;
            // foldi prepends the element index: (index, accumulator, element).
            let idx = if indexed { 1 } else { 0 };
            if f_inputs.len() != 2 + idx {
                diags.push(
                    Diagnostic::error(
                        "E0145",
                        format!(
                            "{iter}: `{f_name}` must take exactly {} inputs ({}accumulator, element)",
                            2 + idx,
                            if indexed { "index, " } else { "" }
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            let acc_ty = f_inputs[idx].ty.clone();
            let elem_ty = f_inputs[idx + 1].ty.clone();
            // The accumulator type must thread: in, out, and the seed agree.
            if !types_compatible(tctx, &acc_ty, &f_out) {
                diags.push(
                    Diagnostic::error(
                        "E0145",
                        format!(
                            "{iter}: `{f_name}`'s accumulator input {acc_ty:?} and output {f_out:?} \
                             must be the same type"
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            if let Some(seed) = init {
                if let Some(seed_ty) =
                    infer_expr_type(seed, env, sigs, node, diags, ctx, tctx, Some(&acc_ty))
                {
                    if !types_compatible(tctx, &acc_ty, &seed_ty) {
                        diags.push(
                            Diagnostic::error(
                                "E0145",
                                format!(
                                    "{iter}: seed has type {seed_ty:?} but `{f_name}`'s accumulator \
                                     is {acc_ty:?}"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                }
            }
            if let Some(e) = elems.first() {
                if !types_compatible(tctx, &elem_ty, e) {
                    diags.push(
                        Diagnostic::error(
                            "E0145",
                            format!(
                                "{iter}: array element type {e:?} but `{f_name}` expects {elem_ty:?}"
                            ),
                        )
                        .with_context(ctx.to_string()),
                    );
                    return None;
                }
            }
            Some(f_out)
        }
        IterKind::MapFold => {
            // mapfold(F, init, a): F is (accumulator, element) ->
            // (accumulator, element_out); the result is the tuple
            // (final accumulator, mapped array), bound by a two-name lhs.
            if f_inputs.len() != 2 {
                diags.push(
                    Diagnostic::error(
                        "E0145",
                        format!(
                            "mapfold: `{f_name}` must take exactly two inputs \
                             (accumulator, element)"
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            let acc_ty = f_inputs[0].ty.clone();
            let elem_ty = f_inputs[1].ty.clone();
            if !types_compatible(tctx, &acc_ty, &f_out) {
                diags.push(
                    Diagnostic::error(
                        "E0145",
                        format!(
                            "mapfold: `{f_name}`'s accumulator input {acc_ty:?} and first \
                             output {f_out:?} must be the same type"
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
                return None;
            }
            if let Some(seed) = init {
                if let Some(seed_ty) =
                    infer_expr_type(seed, env, sigs, node, diags, ctx, tctx, Some(&acc_ty))
                {
                    if !types_compatible(tctx, &acc_ty, &seed_ty) {
                        diags.push(
                            Diagnostic::error(
                                "E0145",
                                format!(
                                    "mapfold: seed has type {seed_ty:?} but `{f_name}`'s \
                                     accumulator is {acc_ty:?}"
                                ),
                            )
                            .with_context(ctx.to_string()),
                        );
                        return None;
                    }
                }
            }
            if let Some(e) = elems.first() {
                if !types_compatible(tctx, &elem_ty, e) {
                    diags.push(
                        Diagnostic::error(
                            "E0145",
                            format!(
                                "mapfold: array element type {e:?} but `{f_name}` \
                                 expects {elem_ty:?}"
                            ),
                        )
                        .with_context(ctx.to_string()),
                    );
                    return None;
                }
            }
            // The equation-level check (E0147) verifies the two-name lhs
            // against (accumulator, element_out array); the marker type here
            // is the same tuple convention multi-output calls use.
            Some(Type::named("__tuple__"))
        }
    }
}

/// The lhs shape of a `mapfold` equation: exactly two names, the first the
/// accumulator's type, the second an array of `F`'s second output with the
/// operand array's length.
fn check_mapfold_lhs(
    eq: &ol_ir::Equation,
    env: &BTreeMap<String, Type>,
    sigs: &HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)>,
    diags: &mut Vec<Diagnostic>,
    ctx: &str,
    tctx: &TypeContext,
) {
    let Expr::Iterate { kind: ol_ir::IterKind::MapFold, node: f_name, arrays, .. } = &eq.rhs
    else {
        return;
    };
    let Some((_, f_outputs, _)) = sigs.get(f_name) else { return };
    if f_outputs.len() != 2 {
        return; // already E0142
    }
    if eq.lhs.len() != 2 {
        diags.push(
            Diagnostic::error(
                "E0147",
                format!(
                    "mapfold binds two results — write `(acc, arr) = mapfold({f_name}, …)`, \
                     got {} name(s)",
                    eq.lhs.len()
                ),
            )
            .with_context(ctx.to_string()),
        );
        return;
    }
    let n = arrays
        .first()
        .and_then(|a| match env.get(match a {
            Expr::Var { name } => name.as_str(),
            _ => "",
        }) {
            Some(t) => match tctx.resolve(t) {
                Type::Array { len, .. } => Some(len),
                _ => None,
            },
            None => None,
        });
    let want = [
        f_outputs[0].ty.clone(),
        Type::Array {
            elem: Box::new(f_outputs[1].ty.clone()),
            len: n.unwrap_or(0),
        },
    ];
    for (name, want_ty) in eq.lhs.iter().zip(want.iter()) {
        if let Some(decl) = env.get(name) {
            let ok = match (tctx.resolve(decl), tctx.resolve(want_ty)) {
                // Unknown operand length (0) checks the element type only.
                (Type::Array { elem: d, .. }, Type::Array { elem: w, len: 0 }) => d == w,
                (d, w) => d == w,
            };
            if !ok {
                diags.push(
                    Diagnostic::error(
                        "E0147",
                        format!(
                            "mapfold result `{name}` is declared {decl:?} but the iterator \
                             produces {want_ty:?}"
                        ),
                    )
                    .with_context(ctx.to_string()),
                );
            }
        }
    }
}

/// An array iterator may only be the *whole* right-hand side of an equation,
/// never nested inside another expression — that keeps codegen a single
/// `for` loop (a `map` even produces an array, which has no C value form).
/// The GUI drops each iterator as its own equation, so this never bites real
/// authoring; it only rejects hand-written nesting.
fn check_iterator_placement(rhs: &Expr, diags: &mut Vec<Diagnostic>, ctx: &str) {
    fn forbid_nested(e: &Expr, diags: &mut Vec<Diagnostic>, ctx: &str) {
        e.visit(|x| {
            let name = match x {
                Expr::Iterate { .. } => "map/fold",
                Expr::ArrayOp { .. } => "concat/reverse",
                _ => return,
            };
            diags.push(
                Diagnostic::error(
                    "E0146",
                    format!(
                        "{name} may only be the whole right-hand side of an equation, \
                         not nested inside another expression"
                    ),
                )
                .with_context(ctx.to_string()),
            );
        });
    }
    match rhs {
        Expr::Iterate { init, arrays, .. } => {
            for sub in init.iter().map(|b| b.as_ref()).chain(arrays.iter()) {
                forbid_nested(sub, diags, ctx);
            }
        }
        Expr::ArrayOp { args, .. } => {
            for sub in args {
                forbid_nested(sub, diags, ctx);
            }
        }
        other => forbid_nested(other, diags, ctx),
    }
}

/// The condition of a `when`/`merge` must be a declared boolean variable.
fn check_clock_var_is_bool(
    clock: &str,
    env: &BTreeMap<String, Type>,
    _node: &NodeDef,
    diags: &mut Vec<Diagnostic>,
    ctx: &str,
    tctx: &TypeContext,
) {
    match env.get(clock) {
        Some(t) if tctx.resolve(t).is_bool() => {}
        Some(t) => {
            diags.push(
                Diagnostic::error(
                    "E0130",
                    format!("clock `{clock}` must be bool, got {t:?}"),
                )
                .with_context(ctx.to_string()),
            );
        }
        None => {
            diags.push(
                Diagnostic::error("E0080", format!("unknown identifier `{clock}`"))
                    .with_context(ctx.to_string()),
            );
        }
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
        Expr::Const { .. } | Expr::Last { .. } => {}
        Expr::Var { name } => {
            out.insert(name.clone());
        }
        Expr::Unary { arg, .. } | Expr::Cast { arg, .. } => collect_immediate_deps(arg, out),
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
        Expr::Call { args, .. }
        | Expr::FloatIntrinsic { args, .. }
        | Expr::ArrayOp { args, .. }
        | Expr::Printout { args }
        | Expr::Sharp { args } => {
            for a in args {
                collect_immediate_deps(a, out);
            }
        }
        Expr::Field { base, .. } => collect_immediate_deps(base, out),
        Expr::Index { base, index } => {
            collect_immediate_deps(base, out);
            collect_immediate_deps(index, out);
        }
        Expr::DynIndex { base, index, default } => {
            collect_immediate_deps(base, out);
            collect_immediate_deps(index, out);
            collect_immediate_deps(default, out);
        }
        Expr::Replicate { value, size } => {
            collect_immediate_deps(value, out);
            collect_immediate_deps(size, out);
        }
        Expr::Slice { base, lo, hi } => {
            collect_immediate_deps(base, out);
            collect_immediate_deps(lo, out);
            collect_immediate_deps(hi, out);
        }
        Expr::Transpose { base } => collect_immediate_deps(base, out),
        Expr::Update { base, index, value, .. } => {
            collect_immediate_deps(base, out);
            if let Some(i) = index {
                collect_immediate_deps(i, out);
            }
            collect_immediate_deps(value, out);
        }
        Expr::Tuple { items } | Expr::Array { items } => {
            for i in items {
                collect_immediate_deps(i, out);
            }
        }
        Expr::Struct { fields, .. } => {
            for fi in fields {
                collect_immediate_deps(&fi.value, out);
            }
        }
        Expr::When { arg, clock, .. } => {
            out.insert(clock.clone());
            collect_immediate_deps(arg, out);
        }
        Expr::Merge { clock, on_true, on_false } => {
            out.insert(clock.clone());
            collect_immediate_deps(on_true, out);
            collect_immediate_deps(on_false, out);
        }
        Expr::Case { sel, arms, default } => {
            collect_immediate_deps(sel, out);
            for arm in arms {
                collect_immediate_deps(&arm.value, out);
            }
            if let Some(d) = default {
                collect_immediate_deps(d, out);
            }
        }
        // The iterated function is stateless: its seed and arrays are
        // same-cycle reads (the function name is not a variable).
        Expr::Iterate { init, arrays, .. } => {
            if let Some(i) = init {
                collect_immediate_deps(i, out);
            }
            for a in arrays {
                collect_immediate_deps(a, out);
            }
        }
    }
}
