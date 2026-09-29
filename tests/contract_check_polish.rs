//! Contract-checker polish (Phase 3): unreachable / vacuous / contradictory
//! clauses, overlapping modes, and contract-import signature validation.

use ol_ir::{Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};

fn project_with(
    nodes: Vec<NodeDef>,
    contracts: Vec<serde_json::Value>,
) -> Project {
    Project {
        name: "ccpolish".into(),
        packages: vec![Package {
            name: "p".into(),
            nodes,
            contracts,
            ..Default::default()
        }],
        main: None,
        ..Default::default()
    }
}

/// A bare `node N(a:bool, b:bool) returns (y:bool); y = a and b;` with a
/// contract whose JSON we customize per test.
fn dummy_node(name: &str, contract: Option<&str>) -> NodeDef {
    NodeDef {
        name: name.into(),
        kind: NodeKind::Operator,
        inputs: vec![
            Port { name: "a".into(), ty: Type::Bool },
            Port { name: "b".into(), ty: Type::Bool },
        ],
        outputs: vec![Port { name: "y".into(), ty: Type::Bool }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::and(Expr::var("a"), Expr::var("b")),
        }],
        contract: contract.map(|s| s.into()),
        diagram: Default::default(),
            probes: vec![],
    }
}

fn codes(diags: &[ol_ir::Diagnostic]) -> Vec<&str> {
    diags.iter().map(|d| d.code.as_str()).collect()
}

#[test]
fn vacuous_guarantee_warns() {
    // guarantee: true   --- statically vacuous
    let contract = serde_json::json!({
        "name": "C", "inputs": [], "outputs": [],
        "ghost_vars": [], "assumptions": [],
        "guarantees": [{ "name": "vac", "expr": Expr::bool_lit(true) }],
        "modes": [], "imports": []
    });
    let node = dummy_node("N", Some("C"));
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    assert!(codes(&report.diagnostics).contains(&"C0062"), "got {:?}", codes(&report.diagnostics));
}

#[test]
fn tautology_guarantee_warns() {
    // guarantee: a or not a
    let contract = serde_json::json!({
        "name": "C",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [],
        "guarantees": [{
            "name": "always",
            "expr": Expr::or(Expr::var("a"), Expr::not(Expr::var("a")))
        }],
        "modes": [], "imports": []
    });
    let node = NodeDef {
        inputs: vec![Port { name: "a".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "y".into(), ty: Type::Bool }],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::var("a"),
        }],
        ..dummy_node("N", Some("C"))
    };
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    assert!(codes(&report.diagnostics).contains(&"C0062"));
}

#[test]
fn statically_false_assumption_warns() {
    let contract = serde_json::json!({
        "name": "C",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [],
        "assumptions": [{ "name": "impossible", "expr": Expr::bool_lit(false) }],
        "guarantees": [], "modes": [], "imports": []
    });
    let node = dummy_node("N", Some("C"));
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    assert!(codes(&report.diagnostics).contains(&"C0063"), "got {:?}", codes(&report.diagnostics));
}

#[test]
fn unreachable_mode_with_contradictory_requires_warns() {
    // mode Bad: require a; require not a;
    let contract = serde_json::json!({
        "name": "C",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [], "guarantees": [],
        "modes": [{
            "name": "Bad",
            "requires": [Expr::var("a"), Expr::not(Expr::var("a"))],
            "ensures": [Expr::var("y")]
        }],
        "imports": []
    });
    let node = dummy_node("N", Some("C"));
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    assert!(codes(&report.diagnostics).contains(&"C0060"));
}

#[test]
fn unreachable_mode_with_static_false_require_warns() {
    let contract = serde_json::json!({
        "name": "C",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [], "guarantees": [],
        "modes": [{
            "name": "Never",
            "requires": [Expr::bool_lit(false)],
            "ensures": [Expr::var("y")]
        }],
        "imports": []
    });
    let node = dummy_node("N", Some("C"));
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    assert!(codes(&report.diagnostics).contains(&"C0060"));
}

#[test]
fn vacuous_mode_ensure_warns() {
    let contract = serde_json::json!({
        "name": "C",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [], "guarantees": [],
        "modes": [{
            "name": "M",
            "requires": [Expr::var("a")],
            "ensures": [Expr::bool_lit(true)]
        }],
        "imports": []
    });
    let node = dummy_node("N", Some("C"));
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    assert!(codes(&report.diagnostics).contains(&"C0061"));
}

#[test]
fn overlapping_modes_with_identical_requires_warn() {
    let req = vec![Expr::var("a")];
    let contract = serde_json::json!({
        "name": "C",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [], "guarantees": [],
        "modes": [
            { "name": "First",  "requires": req,           "ensures": [Expr::var("y")] },
            { "name": "Second", "requires": vec![Expr::var("a")], "ensures": [Expr::not(Expr::var("y"))] },
        ],
        "imports": []
    });
    let node = dummy_node("N", Some("C"));
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    assert!(codes(&report.diagnostics).contains(&"C0064"), "got {:?}", codes(&report.diagnostics));
}

#[test]
fn contract_import_to_unknown_contract_errors() {
    let contract = serde_json::json!({
        "name": "C",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [], "guarantees": [], "modes": [],
        "imports": [{
            "contract": "Nonexistent",
            "input_map": [],
            "output_map": []
        }]
    });
    let node = dummy_node("N", Some("C"));
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    assert!(codes(&report.diagnostics).contains(&"C0070"));
}

#[test]
fn contract_import_with_missing_input_mapping_errors() {
    let imported = serde_json::json!({
        "name": "Inner",
        "inputs": [
            {"name":"x","ty":{"kind":"Bool"}},
            {"name":"z","ty":{"kind":"Bool"}}
        ],
        "outputs": [{"name":"r","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [], "guarantees": [], "modes": [], "imports": []
    });
    let outer = serde_json::json!({
        "name": "Outer",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [], "guarantees": [], "modes": [],
        // Missing mapping for `z`, plus mapping an unknown input `nope`.
        "imports": [{
            "contract": "Inner",
            "input_map": [["x", Expr::var("a")], ["nope", Expr::var("b")]],
            "output_map": [["r", "y"]]
        }]
    });
    let node = dummy_node("N", Some("Outer"));
    let report =
        ol_contract_check::check_project(&project_with(vec![node], vec![imported, outer]));
    let cs = codes(&report.diagnostics);
    // One for missing-input `z`, one for unknown-input `nope`.
    assert!(cs.iter().filter(|c| **c == "C0071").count() >= 2, "got {cs:?}");
}

#[test]
fn well_formed_contract_does_not_get_extra_warnings() {
    // a real, well-formed contract — no false positives.
    let contract = serde_json::json!({
        "name": "C",
        "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
        "ghost_vars": [], "assumptions": [],
        "guarantees": [
            { "name": "g1", "expr": Expr::implies(Expr::var("y"), Expr::var("a")) }
        ],
        "modes": [
            { "name": "On",  "requires": [Expr::var("a")],             "ensures": [Expr::var("y")] },
            { "name": "Off", "requires": [Expr::not(Expr::var("a"))], "ensures": [Expr::not(Expr::var("y"))] },
        ],
        "imports": []
    });
    let node = dummy_node("N", Some("C"));
    let report = ol_contract_check::check_project(&project_with(vec![node], vec![contract]));
    let cs = codes(&report.diagnostics);
    for forbidden in ["C0060", "C0061", "C0062", "C0063", "C0064", "C0070", "C0071", "C0072"] {
        assert!(!cs.contains(&forbidden), "spurious {forbidden} in {cs:?}");
    }
}

/// C0080: contract expressions are type-checked for real. An undeclared name
/// or a non-Boolean clause is an error pinned to the clause; ghost variables
/// are in scope for every clause (and may use `->` / `pre`).
#[test]
fn contract_expressions_are_type_checked() {
    let iface = |extra: serde_json::Value| {
        let mut c = serde_json::json!({
            "name": "C",
            "inputs": [{"name":"a","ty":{"kind":"Bool"}}, {"name":"b","ty":{"kind":"Bool"}}],
            "outputs": [{"name":"y","ty":{"kind":"Bool"}}],
            "ghost_vars": [], "assumptions": [], "guarantees": [], "modes": [], "imports": []
        });
        for (k, v) in extra.as_object().unwrap() {
            c[k] = v.clone();
        }
        c
    };
    let c0080 = |report: &ol_contract_check::ContractReport| -> Vec<String> {
        report.diagnostics.iter().filter(|d| d.code == "C0080").map(|d| d.message.clone()).collect()
    };

    // A typo'd signal in a guarantee.
    let typo = iface(serde_json::json!({
        "guarantees": [{ "name": "typo", "expr": Expr::implies(Expr::var("y"), Expr::var("aa")) }]
    }));
    let report = ol_contract_check::check_project(&project_with(vec![dummy_node("N", Some("C"))], vec![typo]));
    let errs = c0080(&report);
    assert!(
        errs.iter().any(|m| m.contains("guarantee `typo`") && m.contains("`aa`")),
        "undeclared name reported on its clause: {errs:?}"
    );
    assert!(report.has_errors());

    // A non-Boolean mode ensure.
    let non_bool = iface(serde_json::json!({
        "modes": [{ "name": "M", "requires": [Expr::var("a")], "ensures": [Expr::int_lit(3)] }]
    }));
    let report = ol_contract_check::check_project(&project_with(vec![dummy_node("N", Some("C"))], vec![non_bool]));
    assert!(
        c0080(&report).iter().any(|m| m.contains("mode `M` ensure #0")),
        "non-Boolean ensure reported: {:?}",
        c0080(&report)
    );

    // A ghost variable (with `->` / `pre`) is in scope and type-checks clean.
    let ghost = iface(serde_json::json!({
        "ghost_vars": [{ "name": "first", "ty": {"kind":"Bool"},
                         "definition": Expr::arrow(Expr::bool_lit(true), Expr::bool_lit(false)) }],
        "modes": [{ "name": "Init", "requires": [Expr::var("first")], "ensures": [Expr::not(Expr::var("y"))] }]
    }));
    let report = ol_contract_check::check_project(&project_with(vec![dummy_node("N", Some("C"))], vec![ghost]));
    assert!(c0080(&report).is_empty(), "ghost in scope: {:?}", c0080(&report));
}
