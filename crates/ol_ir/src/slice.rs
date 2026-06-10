//! Selective code generation: slice a project down to a selected root
//! operator and everything it transitively uses — the SCADE KCG behavior of
//! "generate the selected operator and all that are used by that model".
//!
//! The slice keeps:
//! * the root node and the transitive closure of nodes it calls (including
//!   calls made inside contract expressions),
//! * only the type definitions reachable from kept ports/locals/constants
//!   (records recurse into field types; aliases into their targets), plus any
//!   enum whose *variant* is referenced by name in a kept expression,
//! * only the constants referenced by kept expressions (closed over
//!   constants referencing other constants),
//! * only the contracts attached to kept nodes (closed over contract
//!   imports), and the imported-operator manifests of kept imported nodes.
//!
//! Package structure is preserved (each package filtered in place; empty
//! packages dropped) and `main` is set to the root. Slice AFTER state-machine
//! lowering — machines become ordinary nodes there; any unlowered machine
//! matching a kept name is retained defensively.

use std::collections::{BTreeSet, VecDeque};

use crate::expr::Expr;
use crate::project::{Package, Project, TypeBody};
use crate::types::Type;

/// Produce the sub-project rooted at `root`. Errors when `root` does not
/// name a node in the project.
pub fn slice_for_root(project: &Project, root: &str) -> Result<Project, String> {
    if project.find_node(root).is_none() {
        return Err(format!("root node `{root}` not found in project"));
    }

    // --- 1. Node closure over calls (equations + contract expressions). ---
    let mut kept_nodes: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(root.to_string());
    while let Some(name) = queue.pop_front() {
        if !kept_nodes.insert(name.clone()) {
            continue;
        }
        let Some(node) = project.find_node(&name) else {
            continue; // unresolved call — typecheck reports it; nothing to slice
        };
        let mut enqueue_calls = |e: &Expr| {
            e.visit(|sub| {
                if let Expr::Call { node: callee, .. } = sub {
                    queue.push_back(callee.clone());
                }
            });
        };
        for eq in &node.equations {
            enqueue_calls(&eq.rhs);
        }
        if let Some(cname) = &node.contract {
            for expr in contract_exprs(project, cname) {
                enqueue_calls(&expr);
            }
        }
    }

    // --- 2. Contract closure (kept nodes' contracts + their imports). ---
    let mut kept_contracts: BTreeSet<String> = BTreeSet::new();
    let mut cqueue: VecDeque<String> = VecDeque::new();
    for name in &kept_nodes {
        if let Some(node) = project.find_node(name) {
            if let Some(c) = &node.contract {
                cqueue.push_back(c.clone());
            }
        }
    }
    while let Some(cname) = cqueue.pop_front() {
        if !kept_contracts.insert(cname.clone()) {
            continue;
        }
        if let Some(raw) = find_contract_raw(project, &cname) {
            if let Some(imports) = raw.get("imports").and_then(|v| v.as_array()) {
                for imp in imports {
                    if let Some(target) = imp.get("contract").and_then(|v| v.as_str()) {
                        cqueue.push_back(target.to_string());
                    }
                }
            }
        }
    }

    // --- 3. Free variables across all kept expressions: feed constant and
    //        enum-variant retention. ---
    let mut free_vars: BTreeSet<String> = BTreeSet::new();
    let mut used_types: Vec<Type> = Vec::new();
    for name in &kept_nodes {
        let Some(node) = project.find_node(name) else { continue };
        for p in node.inputs.iter().chain(node.outputs.iter()) {
            used_types.push(p.ty.clone());
        }
        for l in &node.locals {
            used_types.push(l.ty.clone());
        }
        for eq in &node.equations {
            for v in eq.rhs.free_vars() {
                free_vars.insert(v);
            }
        }
    }
    for cname in &kept_contracts {
        for expr in contract_exprs(project, cname) {
            for v in expr.free_vars() {
                free_vars.insert(v);
            }
        }
        // Ghost-var declared types participate in the type closure.
        if let Some(raw) = find_contract_raw(project, cname) {
            if let Some(ghosts) = raw.get("ghost_vars").and_then(|v| v.as_array()) {
                for g in ghosts {
                    if let Some(ty) = g.get("ty") {
                        if let Ok(t) = serde_json::from_value::<Type>(ty.clone()) {
                            used_types.push(t);
                        }
                    }
                }
            }
        }
    }

    // --- 4. Constant closure: name-referenced constants, then constants
    //        their values reference, to fixpoint. ---
    let mut kept_consts: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut grew = false;
        for pkg in &project.packages {
            for c in &pkg.constants {
                if kept_consts.contains(&c.name) {
                    continue;
                }
                let referenced = free_vars.contains(&c.name)
                    || kept_consts.iter().any(|k| {
                        project_const_value_refs(project, k, &c.name)
                    });
                if referenced {
                    kept_consts.insert(c.name.clone());
                    used_types.push(c.ty.clone());
                    for v in c.value.free_vars() {
                        free_vars.insert(v);
                    }
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }

    // --- 5. Type closure: named types reachable from used_types, recursing
    //        through record fields and alias targets; plus enums whose
    //        variant names appear as free variables. ---
    let mut kept_types: BTreeSet<String> = BTreeSet::new();
    let mut tqueue: VecDeque<String> = VecDeque::new();
    for t in &used_types {
        push_named(t, &mut tqueue);
    }
    for pkg in &project.packages {
        for t in &pkg.types {
            if let TypeBody::Enum(e) = &t.body {
                if e.variants.iter().any(|v| free_vars.contains(v)) {
                    tqueue.push_back(e.name.clone());
                }
            }
        }
    }
    while let Some(tname) = tqueue.pop_front() {
        if !kept_types.insert(tname.clone()) {
            continue;
        }
        for pkg in &project.packages {
            for t in &pkg.types {
                if t.name() != tname {
                    continue;
                }
                match &t.body {
                    TypeBody::Record { fields, .. } => {
                        for f in fields {
                            push_named(&f.ty, &mut tqueue);
                        }
                    }
                    TypeBody::Alias { target, .. } => push_named(target, &mut tqueue),
                    TypeBody::Enum(_) => {}
                }
            }
        }
    }

    // --- 6. Assemble: same package layout, filtered; drop empty packages. ---
    let mut packages = Vec::new();
    for pkg in &project.packages {
        let nodes: Vec<_> = pkg
            .nodes
            .iter()
            .filter(|n| kept_nodes.contains(&n.name))
            .cloned()
            .collect();
        let types: Vec<_> = pkg
            .types
            .iter()
            .filter(|t| kept_types.contains(t.name()))
            .cloned()
            .collect();
        let constants: Vec<_> = pkg
            .constants
            .iter()
            .filter(|c| kept_consts.contains(&c.name))
            .cloned()
            .collect();
        let contracts: Vec<_> = pkg
            .contracts
            .iter()
            .filter(|raw| {
                raw.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| kept_contracts.contains(n))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let imported_operators: Vec<_> = pkg
            .imported_operators
            .iter()
            .filter(|raw| {
                raw.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| kept_nodes.contains(n))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let state_machines: Vec<_> = pkg
            .state_machines
            .iter()
            .filter(|sm| kept_nodes.contains(&sm.name))
            .cloned()
            .collect();
        if nodes.is_empty()
            && types.is_empty()
            && constants.is_empty()
            && contracts.is_empty()
            && imported_operators.is_empty()
            && state_machines.is_empty()
        {
            continue;
        }
        packages.push(Package {
            name: pkg.name.clone(),
            types,
            constants,
            nodes,
            contracts,
            imported_operators,
            state_machines,
        });
    }

    Ok(Project {
        name: project.name.clone(),
        packages,
        main: Some(root.to_string()),
        includes: vec![],
    })
}

fn push_named(ty: &Type, queue: &mut VecDeque<String>) {
    match ty {
        Type::Named { name } => queue.push_back(name.clone()),
        Type::Array { elem, .. } => push_named(elem, queue),
        _ => {}
    }
}

/// Does the value of constant `const_name` reference `target` by name?
fn project_const_value_refs(project: &Project, const_name: &str, target: &str) -> bool {
    for pkg in &project.packages {
        for c in &pkg.constants {
            if c.name == const_name {
                return c.value.free_vars().iter().any(|v| v == target);
            }
        }
    }
    false
}

fn find_contract_raw<'a>(project: &'a Project, name: &str) -> Option<&'a serde_json::Value> {
    for pkg in &project.packages {
        for raw in &pkg.contracts {
            if raw.get("name").and_then(|n| n.as_str()) == Some(name) {
                return Some(raw);
            }
        }
    }
    None
}

/// Best-effort extraction of every Expr inside a raw contract JSON value:
/// assumptions, guarantees, mode requires/ensures, and ghost definitions.
fn contract_exprs(project: &Project, name: &str) -> Vec<Expr> {
    let mut out = Vec::new();
    let Some(raw) = find_contract_raw(project, name) else {
        return out;
    };
    let mut push = |v: Option<&serde_json::Value>| {
        if let Some(v) = v {
            if let Ok(e) = serde_json::from_value::<Expr>(v.clone()) {
                out.push(e);
            }
        }
    };
    for key in ["assumptions", "guarantees"] {
        if let Some(items) = raw.get(key).and_then(|v| v.as_array()) {
            for item in items {
                push(item.get("expr"));
            }
        }
    }
    if let Some(modes) = raw.get("modes").and_then(|v| v.as_array()) {
        for m in modes {
            for key in ["requires", "ensures"] {
                if let Some(items) = m.get(key).and_then(|v| v.as_array()) {
                    for item in items {
                        push(Some(item));
                    }
                }
            }
        }
    }
    if let Some(ghosts) = raw.get("ghost_vars").and_then(|v| v.as_array()) {
        for g in ghosts {
            push(g.get("definition"));
        }
    }
    out
}
