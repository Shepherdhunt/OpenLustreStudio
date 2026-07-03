//! `openlustre diff` — a *semantic* comparison of two model files, the
//! configuration-management story: what changed in the design, not in the
//! JSON. Diagram layout (positions, grid, box sizes) is deliberately
//! invisible here; equations compare by their formatted surface text, so a
//! re-serialization or a box shuffle produces an empty diff.
//!
//! Output is one line per change, `+`/`-`/`~` prefixed, grouped per element:
//!
//! ```text
//! ~ node Interlock: + input reset : bool
//! ~ node Interlock: ~ equation ok = a and b  ->  ok = a and b and not reset
//! - node Legacy
//! + type Mode (enum)
//! ```

use std::collections::BTreeMap;

use ol_ir::{NodeDef, Project, TypeBody};

pub fn diff_projects(old: &Project, new: &Project) -> Vec<String> {
    let mut out = Vec::new();

    if old.main != new.main {
        out.push(format!(
            "~ main: {} -> {}",
            old.main.as_deref().unwrap_or("(none)"),
            new.main.as_deref().unwrap_or("(none)")
        ));
    }

    // --- Nodes -----------------------------------------------------------
    let old_nodes: BTreeMap<&str, &NodeDef> =
        old.all_nodes().map(|n| (n.name.as_str(), n)).collect();
    let new_nodes: BTreeMap<&str, &NodeDef> =
        new.all_nodes().map(|n| (n.name.as_str(), n)).collect();
    for (name, n) in &new_nodes {
        if !old_nodes.contains_key(name) {
            out.push(format!(
                "+ node {name} ({:?}, {} in / {} out, {} equation{})",
                n.kind,
                n.inputs.len(),
                n.outputs.len(),
                n.equations.len(),
                if n.equations.len() == 1 { "" } else { "s" }
            ));
        }
    }
    for (name, o) in &old_nodes {
        match new_nodes.get(name) {
            None => out.push(format!("- node {name}")),
            Some(n) => diff_node(o, n, &mut out),
        }
    }

    // --- Types -----------------------------------------------------------
    let type_desc = |b: &TypeBody| match b {
        TypeBody::Enum(e) => format!("enum [{}]", e.variants.join(", ")),
        TypeBody::Record { fields, .. } => format!(
            "record {{ {} }}",
            fields
                .iter()
                .map(|f| format!("{}: {}", f.name, f.ty.lustre_name()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeBody::Alias { target, .. } => format!("alias = {}", target.lustre_name()),
    };
    let old_types: BTreeMap<&str, String> = old
        .packages
        .iter()
        .flat_map(|p| &p.types)
        .map(|t| (t.name(), type_desc(&t.body)))
        .collect();
    let new_types: BTreeMap<&str, String> = new
        .packages
        .iter()
        .flat_map(|p| &p.types)
        .map(|t| (t.name(), type_desc(&t.body)))
        .collect();
    diff_named_map("type", &old_types, &new_types, &mut out);

    // --- Constants ---------------------------------------------------------
    let const_desc = |c: &ol_ir::ConstDef| {
        format!("{} = {}", c.ty.lustre_name(), ol_lustre_emit::format_expr(&c.value))
    };
    let old_consts: BTreeMap<&str, String> = old
        .packages
        .iter()
        .flat_map(|p| &p.constants)
        .map(|c| (c.name.as_str(), const_desc(c)))
        .collect();
    let new_consts: BTreeMap<&str, String> = new
        .packages
        .iter()
        .flat_map(|p| &p.constants)
        .map(|c| (c.name.as_str(), const_desc(c)))
        .collect();
    diff_named_map("constant", &old_consts, &new_consts, &mut out);

    // --- State machines (raw, pre-lowering) -------------------------------
    let sm_desc = |m: &ol_ir::StateMachineDef| {
        format!(
            "owner {}, initial {}, states [{}]",
            m.owner.as_deref().unwrap_or("(standalone)"),
            m.initial_state,
            m.states.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
        )
    };
    let old_sms: BTreeMap<&str, String> = old
        .packages
        .iter()
        .flat_map(|p| &p.state_machines)
        .map(|m| (m.name.as_str(), sm_desc(m)))
        .collect();
    let new_sms: BTreeMap<&str, String> = new
        .packages
        .iter()
        .flat_map(|p| &p.state_machines)
        .map(|m| (m.name.as_str(), sm_desc(m)))
        .collect();
    diff_named_map("state machine", &old_sms, &new_sms, &mut out);

    out
}

/// Generic +/-/~ diff over name → description maps.
fn diff_named_map(
    kind: &str,
    old: &BTreeMap<&str, String>,
    new: &BTreeMap<&str, String>,
    out: &mut Vec<String>,
) {
    for (name, desc) in new {
        if !old.contains_key(name) {
            out.push(format!("+ {kind} {name} ({desc})"));
        }
    }
    for (name, odesc) in old {
        match new.get(name) {
            None => out.push(format!("- {kind} {name}")),
            Some(ndesc) if ndesc != odesc => {
                out.push(format!("~ {kind} {name}: {odesc}  ->  {ndesc}"));
            }
            _ => {}
        }
    }
}

fn diff_node(old: &NodeDef, new: &NodeDef, out: &mut Vec<String>) {
    let name = &new.name;
    let mut push = |msg: String| out.push(format!("~ node {name}: {msg}"));

    if old.kind != new.kind {
        push(format!("kind {:?} -> {:?}", old.kind, new.kind));
    }
    for (role, o, n) in [
        ("input", ports(&old.inputs), ports(&new.inputs)),
        ("output", ports(&old.outputs), ports(&new.outputs)),
        (
            "local",
            old.locals.iter().map(|l| (l.name.as_str(), l.ty.lustre_name())).collect(),
            new.locals.iter().map(|l| (l.name.as_str(), l.ty.lustre_name())).collect(),
        ),
    ] {
        for (pn, ty) in &n {
            if !o.contains_key(pn) {
                push(format!("+ {role} {pn} : {ty}"));
            }
        }
        for (pn, oty) in &o {
            match n.get(pn) {
                None => push(format!("- {role} {pn} : {oty}")),
                Some(nty) if nty != oty => push(format!("~ {role} {pn} : {oty} -> {nty}")),
                _ => {}
            }
        }
    }

    // Equations compare by lhs: the defining expression of each result. A
    // model can't define one lhs twice (single assignment), so lhs text is a
    // stable key; multi-output equations key on the joined tuple.
    let eqs = |node: &NodeDef| -> BTreeMap<String, String> {
        node.equations
            .iter()
            .map(|e| (e.lhs.join(", "), ol_lustre_emit::format_expr(&e.rhs)))
            .collect()
    };
    let o_eqs = eqs(old);
    let n_eqs = eqs(new);
    for (lhs, rhs) in &n_eqs {
        if !o_eqs.contains_key(lhs) {
            push(format!("+ equation {lhs} = {rhs}"));
        }
    }
    for (lhs, orhs) in &o_eqs {
        match n_eqs.get(lhs) {
            None => push(format!("- equation {lhs} = {orhs}")),
            Some(nrhs) if nrhs != orhs => {
                push(format!("~ equation {lhs} = {orhs}  ->  {lhs} = {nrhs}"));
            }
            _ => {}
        }
    }

    if old.contract != new.contract {
        push(format!(
            "contract {} -> {}",
            old.contract.as_deref().unwrap_or("(none)"),
            new.contract.as_deref().unwrap_or("(none)")
        ));
    }
    if old.requirements != new.requirements {
        push(format!(
            "requirements [{}] -> [{}]",
            old.requirements.join(", "),
            new.requirements.join(", ")
        ));
    }
}

fn ports(ps: &[ol_ir::Port]) -> BTreeMap<&str, String> {
    ps.iter().map(|p| (p.name.as_str(), p.ty.lustre_name())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ol_ir::{Equation, Expr, NodeKind, Package, Port, Type};

    fn node(name: &str, eq_body: Expr) -> NodeDef {
        NodeDef {
            name: name.into(),
            kind: NodeKind::Operator,
            inputs: vec![Port { name: "a".into(), ty: Type::Bool }],
            outputs: vec![Port { name: "y".into(), ty: Type::Bool }],
            locals: vec![],
            equations: vec![Equation { lhs: vec!["y".into()], rhs: eq_body }],
            contract: None,
            diagram: Default::default(),
            probes: vec![],
            requirements: vec![],
        }
    }

    fn project(nodes: Vec<NodeDef>) -> Project {
        Project {
            name: "p".into(),
            main: None,
            includes: vec![],
            packages: vec![Package {
                name: "user".into(),
                types: vec![],
                constants: vec![],
                nodes,
                contracts: vec![],
                imported_operators: vec![],
                state_machines: vec![],
            }],
        }
    }

    #[test]
    fn identical_models_diff_empty_even_with_layout_changes() {
        let a = project(vec![node("N", Expr::var("a"))]);
        let mut b = a.clone();
        // Layout is not semantics: moving a box changes nothing.
        b.packages[0].nodes[0]
            .diagram
            .positions
            .insert("y".into(), ol_ir::NodePos { x: 500.0, y: 500.0, ..Default::default() });
        assert!(diff_projects(&a, &b).is_empty());
    }

    #[test]
    fn added_removed_and_changed_elements_are_reported() {
        let a = project(vec![node("Keep", Expr::var("a")), node("Drop", Expr::var("a"))]);
        let mut changed = node("Keep", Expr::not(Expr::var("a")));
        changed.requirements = vec!["SRS-1".into()];
        let b = project(vec![changed, node("Add", Expr::var("a"))]);
        let d = diff_projects(&a, &b);
        assert!(d.iter().any(|l| l.starts_with("+ node Add")), "{d:?}");
        assert!(d.iter().any(|l| l == "- node Drop"), "{d:?}");
        assert!(
            d.iter().any(|l| l.contains("~ node Keep: ~ equation y = a  ->  y = not a")),
            "{d:?}"
        );
        assert!(d.iter().any(|l| l.contains("requirements [] -> [SRS-1]")), "{d:?}");
    }
}
