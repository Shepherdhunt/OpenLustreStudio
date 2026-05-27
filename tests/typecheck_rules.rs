//! Negative tests for the type checker rules called out in Phase 2.

use ol_ir::{Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};

fn bool_port(name: &str) -> Port {
    Port { name: name.into(), ty: Type::Bool }
}

fn project_with(node: NodeDef) -> Project {
    Project {
        name: "negative_test".into(),
        packages: vec![Package {
            name: "p".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: None,
    }
}

#[test]
fn function_cannot_contain_pre() {
    // `function` bodies must not use `pre` (Phase 0 profile rule).
    let bad = NodeDef {
        name: "BadFn".into(),
        kind: NodeKind::Function,
        inputs: vec![bool_port("x")],
        outputs: vec![bool_port("y")],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::arrow(Expr::bool_lit(false), Expr::pre(Expr::var("x"))),
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(bad));
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0030"), "expected E0030, got {codes:?}");
}

#[test]
fn unassigned_output_is_an_error() {
    let bad = NodeDef {
        name: "MissingOutput".into(),
        kind: NodeKind::Operator,
        inputs: vec![bool_port("x")],
        outputs: vec![bool_port("y")],
        locals: vec![],
        equations: vec![],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(bad));
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0050"), "expected E0050, got {codes:?}");
}

#[test]
fn double_assignment_is_an_error() {
    let bad = NodeDef {
        name: "DoubleAssign".into(),
        kind: NodeKind::Operator,
        inputs: vec![bool_port("x")],
        outputs: vec![bool_port("y")],
        locals: vec![],
        equations: vec![
            Equation { lhs: vec!["y".into()], rhs: Expr::var("x") },
            Equation { lhs: vec!["y".into()], rhs: Expr::not(Expr::var("x")) },
        ],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(bad));
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0021"), "expected E0021, got {codes:?}");
}

#[test]
fn combinational_cycle_is_an_error() {
    // y = z; z = y — pure combinational loop with no temporal break.
    let bad = NodeDef {
        name: "Cycle".into(),
        kind: NodeKind::Operator,
        inputs: vec![],
        outputs: vec![bool_port("y"), bool_port("z")],
        locals: vec![],
        equations: vec![
            Equation { lhs: vec!["y".into()], rhs: Expr::var("z") },
            Equation { lhs: vec!["z".into()], rhs: Expr::var("y") },
        ],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(bad));
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0060"), "expected E0060, got {codes:?}");
}

#[test]
fn uninitialized_pre_is_an_error() {
    // `pre x` outside of an `->` is forbidden.
    let bad = NodeDef {
        name: "UninitPre".into(),
        kind: NodeKind::Operator,
        inputs: vec![bool_port("x")],
        outputs: vec![bool_port("y")],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::pre(Expr::var("x")),
        }],
        contract: None,
        diagram: Default::default(),
    };
    let report = ol_typecheck::check_project(&project_with(bad));
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0070"), "expected E0070, got {codes:?}");
}
