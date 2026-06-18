//! Generic operator templates — SCADE's `'T` type polymorphism and `N`
//! array-size parameters — implemented by **monomorphization**: a template
//! (a node whose ports/locals mention `Type::Var` / `Type::ArrayVar`) is
//! specialized into a concrete node per distinct instantiation, and every
//! call site is rewritten to the specialized name. Templates stay in the
//! project (visible and editable in the Studio) but become unreachable, so
//! root slicing keeps them out of every backend; the emitters additionally
//! skip them outright.
//!
//! Bindings are INFERRED from the call's argument types — `Saturate(x, lo,
//! hi)` with `x: float64` instantiates `Saturate_float64`; `Sum(a)` with
//! `a: int32[4]` binds `N = 4`. A parameter that no argument determines is
//! a loud error (annotate an argument, or wrap the call), as is a binding
//! that violates the parameter's constraint (`'T: numeric` etc.).

use std::collections::{BTreeMap, HashMap};

use ol_ir::{
    Diagnostic, Expr, GenericParam, IterKind, NodeDef, NodeKind, Port, Project, Type,
    TypeConstraint,
};

use crate::{infer_expr_type, types_compatible, TypeContext};

/// The node's generic parameters: the declared list plus any parameter its
/// port/local types mention implicitly (a Studio user typing a `'T` port
/// never has to declare it separately — the constraint defaults to `any`).
pub fn effective_generics(node: &NodeDef) -> Vec<GenericParam> {
    let mut out = node.generics.clone();
    let mut have: std::collections::HashSet<String> =
        out.iter().map(|g| g.name().to_string()).collect();
    let mut walk = |ty: &Type, out: &mut Vec<GenericParam>,
                    have: &mut std::collections::HashSet<String>| {
        let mut stack = vec![ty.clone()];
        while let Some(t) = stack.pop() {
            match t {
                Type::Var { name } => {
                    if have.insert(name.clone()) {
                        out.push(GenericParam::Type { name, constraint: TypeConstraint::Any });
                    }
                }
                Type::ArrayVar { elem, len_param } => {
                    if have.insert(len_param.clone()) {
                        out.push(GenericParam::Size { name: len_param });
                    }
                    stack.push(*elem);
                }
                Type::Array { elem, .. } => stack.push(*elem),
                _ => {}
            }
        }
    };
    for p in node.inputs.iter().chain(node.outputs.iter()) {
        walk(&p.ty, &mut out, &mut have);
    }
    for l in &node.locals {
        walk(&l.ty, &mut out, &mut have);
    }
    out
}

/// Is this node a generic template?
pub fn is_template(node: &NodeDef) -> bool {
    !node.generics.is_empty()
        || node
            .inputs
            .iter()
            .chain(node.outputs.iter())
            .any(|p| p.ty.is_generic())
        || node.locals.iter().any(|l| l.ty.is_generic())
}

/// Unify a template's declared input types against concrete argument types,
/// producing the type- and size-parameter bindings.
fn unify(
    declared: &Type,
    actual: &Type,
    tctx: &TypeContext,
    tmap: &mut BTreeMap<String, Type>,
    smap: &mut BTreeMap<String, u32>,
) -> Result<(), String> {
    match (declared, tctx.resolve(actual)) {
        (Type::Var { name }, concrete) => match tmap.get(name) {
            Some(bound) if !types_compatible(tctx, bound, &concrete) => Err(format!(
                "`'{name}` would bind both {} and {}",
                bound.lustre_name(),
                concrete.lustre_name()
            )),
            Some(_) => Ok(()),
            None => {
                tmap.insert(name.clone(), concrete);
                Ok(())
            }
        },
        (Type::ArrayVar { elem, len_param }, Type::Array { elem: ae, len }) => {
            match smap.get(len_param) {
                Some(&bound) if bound != len => {
                    return Err(format!("`{len_param}` would bind both {bound} and {len}"));
                }
                Some(_) => {}
                None => {
                    smap.insert(len_param.clone(), len);
                }
            }
            unify(elem, &ae, tctx, tmap, smap)
        }
        (Type::Array { elem, len }, Type::Array { elem: ae, len: al }) => {
            if *len != al {
                return Err(format!("array length mismatch: {len} vs {al}"));
            }
            unify(elem, &ae, tctx, tmap, smap)
        }
        (d, a) => {
            if types_compatible(tctx, d, &a) {
                Ok(())
            } else {
                Err(format!("{} vs {}", d.lustre_name(), a.lustre_name()))
            }
        }
    }
}

/// A short, stable suffix for one binding set: `Saturate` + {T→float64}
/// becomes `Saturate_float64`; {N→4} appends `_4`. Parameter order follows
/// the template's declaration order so the name is deterministic.
fn mangle(base: &str, generics: &[GenericParam], tmap: &BTreeMap<String, Type>,
          smap: &BTreeMap<String, u32>) -> String {
    let mut name = base.to_string();
    for g in generics {
        match g {
            GenericParam::Type { name: n, .. } => {
                if let Some(t) = tmap.get(n) {
                    name.push('_');
                    name.push_str(&type_tag(t));
                }
            }
            GenericParam::Size { name: n } => {
                if let Some(s) = smap.get(n) {
                    name.push('_');
                    name.push_str(&s.to_string());
                }
            }
        }
    }
    name
}

/// The width-precise tag for a bound type — `lustre_name` collapses every
/// integer to `int`, which would collide `int8` with `int64` instances.
fn type_tag(t: &Type) -> String {
    match t {
        Type::Bool => "bool".into(),
        Type::Int8 => "int8".into(),
        Type::Int16 => "int16".into(),
        Type::Int32 => "int32".into(),
        Type::Int64 => "int64".into(),
        Type::Uint8 => "uint8".into(),
        Type::Uint16 => "uint16".into(),
        Type::Uint32 => "uint32".into(),
        Type::Uint64 => "uint64".into(),
        Type::Float32 => "float32".into(),
        Type::Float64 => "float64".into(),
        Type::Char => "char".into(),
        Type::Fixed { signed, bits, frac } => {
            format!("{}fix{bits}_{frac}", if *signed { "s" } else { "u" })
        }
        Type::Array { elem, len } => format!("{}x{len}", type_tag(elem)),
        Type::Named { name } => name.clone(),
        Type::Var { name } => name.clone(),
        Type::ArrayVar { elem, len_param } => format!("{}x{len_param}", type_tag(elem)),
    }
}

/// Substitute the bindings through a whole node: ports, locals, and the
/// `Cast` targets inside its equations.
fn substitute_node(
    node: &mut NodeDef,
    tmap: &BTreeMap<String, Type>,
    smap: &BTreeMap<String, u32>,
) {
    for p in node.inputs.iter_mut().chain(node.outputs.iter_mut()) {
        p.ty = p.ty.substitute(tmap, smap);
    }
    for l in &mut node.locals {
        l.ty = l.ty.substitute(tmap, smap);
    }
    for eq in &mut node.equations {
        eq.rhs.visit_mut(&mut |e: &mut Expr| {
            if let Expr::Cast { to, .. } = e {
                *to = to.substitute(tmap, smap);
            }
        });
    }
}

/// Specialize every call to a generic template, per distinct binding set,
/// and rewrite the call sites. Returns diagnostics; on success the project
/// contains the concrete instances and no reachable template calls.
pub fn monomorphize(project: &mut Project) -> Vec<Diagnostic> {
    let mut diags: Vec<Diagnostic> = Vec::new();
    let tctx = TypeContext::from_project(project);

    // Normalize: every template carries its full parameter list.
    for pkg in &mut project.packages {
        for node in &mut pkg.nodes {
            if is_template(node) {
                node.generics = effective_generics(node);
            }
        }
    }
    let templates: HashMap<String, NodeDef> = project
        .all_nodes()
        .filter(|n| is_template(n))
        .map(|n| (n.name.clone(), n.clone()))
        .collect();
    if templates.is_empty() {
        return diags;
    }
    if let Some(main) = &project.main {
        if templates.contains_key(main) {
            diags.push(Diagnostic::error(
                "E0192",
                format!(
                    "the main operator `{main}` is a generic template — instantiate it \
                     from a concrete operator instead"
                ),
            ));
        }
    }

    // Fixpoint: instances are concrete nodes that may themselves call other
    // templates, so keep sweeping until a pass creates nothing new.
    let mut created: std::collections::HashSet<String> = Default::default();
    for _round in 0..16 {
        let sigs: HashMap<String, (Vec<Port>, Vec<Port>, NodeKind)> = project
            .all_nodes()
            .map(|n| (n.name.clone(), (n.inputs.clone(), n.outputs.clone(), n.kind)))
            .collect();
        let mut new_instances: Vec<(usize, NodeDef)> = Vec::new();
        for (pi, pkg) in project.packages.iter_mut().enumerate() {
            for node in &mut pkg.nodes {
                if is_template(node) {
                    continue;
                }
                let env: BTreeMap<String, Type> = node
                    .inputs
                    .iter()
                    .chain(node.outputs.iter())
                    .map(|p| (p.name.clone(), p.ty.clone()))
                    .chain(node.locals.iter().map(|l| (l.name.clone(), l.ty.clone())))
                    .collect();
                let snapshot = node.clone();
                for (ei, eq) in node.equations.iter_mut().enumerate() {
                    let ctx = format!("node {} · equation {}", snapshot.name, ei);
                    eq.rhs.visit_mut(&mut |e: &mut Expr| {
                        // A direct call to a template, or an iterator over one.
                        let (callee, arg_tys): (&mut String, Option<Vec<Type>>) = match e {
                            Expr::Call { node: f, args } if templates.contains_key(f.as_str()) => {
                                let mut sink = Vec::new();
                                let tys: Option<Vec<Type>> = args
                                    .iter()
                                    .map(|a| {
                                        infer_expr_type(
                                            a, &env, &sigs, &snapshot, &mut sink, &ctx, &tctx,
                                            None,
                                        )
                                    })
                                    .collect();
                                (f, tys)
                            }
                            Expr::Iterate { node: f, init, arrays, kind }
                                if templates.contains_key(f.as_str()) =>
                            {
                                // Synthesize the per-element argument types the
                                // iterated template is called with.
                                let mut sink = Vec::new();
                                let elem_of = |a: &Expr, sink: &mut Vec<Diagnostic>| {
                                    match infer_expr_type(
                                        a, &env, &sigs, &snapshot, sink, &ctx, &tctx, None,
                                    )
                                    .map(|t| tctx.resolve(&t))
                                    {
                                        Some(Type::Array { elem, .. }) => Some(*elem),
                                        _ => None,
                                    }
                                };
                                let mut tys: Option<Vec<Type>> = Some(Vec::new());
                                let push = |tys: &mut Option<Vec<Type>>, t: Option<Type>| {
                                    match (tys.as_mut(), t) {
                                        (Some(v), Some(t)) => v.push(t),
                                        _ => *tys = None,
                                    }
                                };
                                if matches!(kind, IterKind::Mapi | IterKind::Foldi) {
                                    push(&mut tys, Some(Type::Int32));
                                }
                                if matches!(
                                    kind,
                                    IterKind::Fold | IterKind::Foldi | IterKind::MapFold
                                ) {
                                    let acc = init.as_ref().and_then(|i| {
                                        infer_expr_type(
                                            i, &env, &sigs, &snapshot, &mut sink, &ctx, &tctx,
                                            None,
                                        )
                                    });
                                    push(&mut tys, acc);
                                }
                                for a in arrays.iter() {
                                    let t = elem_of(a, &mut sink);
                                    push(&mut tys, t);
                                }
                                (f, tys)
                            }
                            _ => return,
                        };
                        let template = &templates[callee.as_str()];
                        let Some(arg_tys) = arg_tys else {
                            diags.push(
                                Diagnostic::error(
                                    "E0190",
                                    format!(
                                        "cannot infer `{}`'s generic parameters — an \
                                         argument's type could not be determined",
                                        template.name
                                    ),
                                )
                                .with_context(ctx.clone()),
                            );
                            return;
                        };
                        if arg_tys.len() != template.inputs.len() {
                            diags.push(
                                Diagnostic::error(
                                    "E0190",
                                    format!(
                                        "`{}` takes {} input(s) but {} were supplied",
                                        template.name,
                                        template.inputs.len(),
                                        arg_tys.len()
                                    ),
                                )
                                .with_context(ctx.clone()),
                            );
                            return;
                        }
                        let mut tmap = BTreeMap::new();
                        let mut smap = BTreeMap::new();
                        for (p, at) in template.inputs.iter().zip(arg_tys.iter()) {
                            if let Err(why) = unify(&p.ty, at, &tctx, &mut tmap, &mut smap) {
                                diags.push(
                                    Diagnostic::error(
                                        "E0190",
                                        format!(
                                            "instantiating `{}`: input `{}`: {why}",
                                            template.name, p.name
                                        ),
                                    )
                                    .with_context(ctx.clone()),
                                );
                                return;
                            }
                        }
                        // Every parameter must be determined by the arguments.
                        for g in &template.generics {
                            let (bound, what) = match g {
                                GenericParam::Type { name, .. } => (tmap.contains_key(name), "'"),
                                GenericParam::Size { name } => (smap.contains_key(name), ""),
                            };
                            if !bound {
                                diags.push(
                                    Diagnostic::error(
                                        "E0190",
                                        format!(
                                            "`{}`'s parameter `{what}{}` is not determined \
                                             by any argument",
                                            template.name,
                                            g.name()
                                        ),
                                    )
                                    .with_context(ctx.clone()),
                                );
                                return;
                            }
                            // …and satisfy its constraint.
                            if let GenericParam::Type { name, constraint } = g {
                                let t = &tmap[name];
                                if !constraint.admits(&tctx.resolve(t)) {
                                    diags.push(
                                        Diagnostic::error(
                                            "E0191",
                                            format!(
                                                "`{}` requires `'{name}: {}` but the call \
                                                 binds {}",
                                                template.name,
                                                constraint.describe(),
                                                t.lustre_name()
                                            ),
                                        )
                                        .with_context(ctx.clone()),
                                    );
                                    return;
                                }
                            }
                        }
                        let inst_name = mangle(&template.name, &template.generics, &tmap, &smap);
                        if !created.contains(&inst_name) && !sigs.contains_key(&inst_name) {
                            let mut inst = template.clone();
                            inst.name = inst_name.clone();
                            inst.generics = Vec::new();
                            inst.contract = None; // v1: contracts stay on the template
                            substitute_node(&mut inst, &tmap, &smap);
                            created.insert(inst_name.clone());
                            new_instances.push((pi, inst));
                        }
                        *callee = inst_name;
                    });
                }
            }
        }
        if new_instances.is_empty() {
            break;
        }
        for (pi, inst) in new_instances {
            project.packages[pi].nodes.push(inst);
        }
    }
    diags
}
