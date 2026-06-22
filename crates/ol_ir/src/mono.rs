//! Monomorphization: expand generic node templates into concrete nodes.
//!
//! A *generic template* is an ordinary [`NodeDef`] whose port / local / cast
//! types may name a type parameter (e.g. `Type::Named { name: "T" }`); the
//! package's [`GenericNode`](crate::GenericNode) list records which nodes are
//! templates and over which parameters. A [`GenericInst`](crate::GenericInst)
//! is an explicit, Ada-style instantiation — "build node `PickInt` from generic
//! `Pick` with `T = int32`".
//!
//! [`Project::monomorphize`] replaces every instantiation with a concrete copy
//! of its template (type parameters substituted throughout) and drops the
//! templates. It runs after [`Project::lower_state_machines`] and before any
//! downstream tool, so typecheck, the simulator and the emitters only ever see
//! concrete nodes and need no awareness of genericity — the same strategy state
//! machines use.

use std::collections::{HashMap, HashSet};

use crate::expr::Expr;
use crate::node::NodeDef;
use crate::project::Project;
use crate::types::Type;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MonoError {
    #[error("instantiation `{0}` refers to unknown generic `{1}`")]
    UnknownGeneric(String, String),
    #[error("generic `{0}` is declared but no node named `{0}` was found to use as its template")]
    MissingTemplate(String),
    #[error("instantiation `{inst}` of `{generic}`: `{param}` is not a type parameter of the generic")]
    UnknownParam { inst: String, generic: String, param: String },
    #[error("instantiation `{inst}` of `{generic}`: type parameter `{param}` is left unbound")]
    UnboundParam { inst: String, generic: String, param: String },
    #[error("instantiation `{0}` reuses the name of an existing node")]
    NameClash(String),
}

impl Project {
    /// Expand every generic instantiation into a concrete node and drop the
    /// generic templates. Idempotent on a project with no generics.
    pub fn monomorphize(&mut self) -> Result<(), Vec<MonoError>> {
        let mut errors = Vec::new();
        for pkg in &mut self.packages {
            if pkg.generics.is_empty() && pkg.instantiations.is_empty() {
                continue;
            }
            let params_of: HashMap<String, Vec<String>> =
                pkg.generics.iter().map(|g| (g.node.clone(), g.params.clone())).collect();
            let generic_names: HashSet<String> = params_of.keys().cloned().collect();
            // Snapshot the template bodies before we mutate `pkg.nodes`.
            let templates: HashMap<String, NodeDef> = pkg
                .nodes
                .iter()
                .filter(|n| generic_names.contains(&n.name))
                .map(|n| (n.name.clone(), n.clone()))
                .collect();
            for g in &generic_names {
                if !templates.contains_key(g) {
                    errors.push(MonoError::MissingTemplate(g.clone()));
                }
            }

            let insts = std::mem::take(&mut pkg.instantiations);
            let mut concrete: Vec<NodeDef> = Vec::new();
            for inst in &insts {
                let (params, template) = match (params_of.get(&inst.generic), templates.get(&inst.generic)) {
                    (Some(p), Some(t)) => (p, t),
                    _ => {
                        errors.push(MonoError::UnknownGeneric(inst.name.clone(), inst.generic.clone()));
                        continue;
                    }
                };
                let mut map: HashMap<String, Type> = HashMap::new();
                let mut bad = false;
                for a in &inst.args {
                    if !params.contains(&a.param) {
                        errors.push(MonoError::UnknownParam {
                            inst: inst.name.clone(),
                            generic: inst.generic.clone(),
                            param: a.param.clone(),
                        });
                        bad = true;
                    }
                    map.insert(a.param.clone(), a.ty.clone());
                }
                for p in params {
                    if !map.contains_key(p) {
                        errors.push(MonoError::UnboundParam {
                            inst: inst.name.clone(),
                            generic: inst.generic.clone(),
                            param: p.clone(),
                        });
                        bad = true;
                    }
                }
                if bad {
                    continue;
                }
                let mut node = template.clone();
                node.name = inst.name.clone();
                subst_node(&mut node, &map);
                concrete.push(node);
            }

            // Drop the templates (and the now-consumed generic declarations),
            // then add the concrete monomorphs.
            pkg.nodes.retain(|n| !generic_names.contains(&n.name));
            pkg.generics.clear();
            let existing: HashSet<String> = pkg.nodes.iter().map(|n| n.name.clone()).collect();
            for c in concrete {
                if existing.contains(&c.name) || pkg.nodes.iter().any(|n| n.name == c.name) {
                    errors.push(MonoError::NameClash(c.name.clone()));
                } else {
                    pkg.nodes.push(c);
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn subst_node(node: &mut NodeDef, map: &HashMap<String, Type>) {
    for p in node.inputs.iter_mut().chain(node.outputs.iter_mut()) {
        p.ty = subst_type(&p.ty, map);
    }
    for l in &mut node.locals {
        l.ty = subst_type(&l.ty, map);
    }
    for eq in &mut node.equations {
        subst_expr(&mut eq.rhs, map);
    }
}

/// Substitute type parameters in a type. A `Named { name }` whose name is a
/// bound parameter becomes the argument; arrays recurse into their element.
fn subst_type(ty: &Type, map: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Named { name } => map.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Array { elem, len } => Type::Array { elem: Box::new(subst_type(elem, map)), len: *len },
        other => other.clone(),
    }
}

/// Walk an expression substituting any embedded type — the only one a profile
/// expression carries is a `Cast` target.
fn subst_expr(e: &mut Expr, map: &HashMap<String, Type>) {
    match e {
        Expr::Cast { to, arg } => {
            *to = subst_type(to, map);
            subst_expr(arg, map);
        }
        Expr::Unary { arg, .. } | Expr::Pre { arg } => subst_expr(arg, map),
        Expr::Binary { lhs, rhs, .. } => {
            subst_expr(lhs, map);
            subst_expr(rhs, map);
        }
        Expr::IfThenElse { cond, then_branch, else_branch } => {
            subst_expr(cond, map);
            subst_expr(then_branch, map);
            subst_expr(else_branch, map);
        }
        Expr::Arrow { init, body } => {
            subst_expr(init, map);
            subst_expr(body, map);
        }
        Expr::Call { args, .. } | Expr::Tuple { items: args } | Expr::Array { items: args }
        | Expr::Intrinsic { args, .. } => {
            for a in args {
                subst_expr(a, map);
            }
        }
        Expr::Field { base, .. } => subst_expr(base, map),
        Expr::Index { base, index } => {
            subst_expr(base, map);
            subst_expr(index, map);
        }
        Expr::Struct { fields, .. } => {
            for f in fields {
                subst_expr(&mut f.value, map);
            }
        }
        Expr::When { arg, .. } => subst_expr(arg, map),
        Expr::Merge { on_true, on_false, .. } => {
            subst_expr(on_true, map);
            subst_expr(on_false, map);
        }
        Expr::Iterate { init, arrays, .. } => {
            if let Some(i) = init {
                subst_expr(i, map);
            }
            for a in arrays {
                subst_expr(a, map);
            }
        }
        Expr::Const { .. } | Expr::Var { .. } => {}
    }
}
