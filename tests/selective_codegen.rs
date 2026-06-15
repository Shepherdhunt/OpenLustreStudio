//! Selective code generation (SCADE KCG behavior): generate the selected
//! root operator and the transitive closure of everything it uses — nodes,
//! types, constants, contracts — and nothing else. The sliced project must
//! still typecheck, and its simulated behavior must be identical to the
//! same node simulated inside the full project.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ol_ir::{
    ConstDef, EnumDef, Equation, Expr, NodeDef, NodeKind, Package, Port, Project, RecordField,
    Type, TypeBody, TypeDef,
};
use ol_sim::{Sim, Value};

fn libraries_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries")
}

/// A project with deliberate clutter around the root:
/// - `Main(button) -> (edge, level)` calls `RisingEdge` (stdlib) and
///   references constant `GAIN` (which references `BASE`) plus enum variant
///   `Armed` of `ArmState`.
/// - `Unrelated` node, `UnusedRecord` type, `UNUSED_K` constant, and an
///   unrelated contract must all be sliced away.
fn cluttered_project() -> Project {
    let main = NodeDef {
        name: "Main".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "button".into(), ty: Type::Bool }],
        outputs: vec![
            Port { name: "edge".into(), ty: Type::Bool },
            Port { name: "level".into(), ty: Type::Int32 },
            Port { name: "st".into(), ty: Type::named("ArmState") },
        ],
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["edge".into()],
                rhs: Expr::call("RisingEdge", vec![Expr::var("button")]),
            },
            Equation {
                lhs: vec!["level".into()],
                rhs: Expr::if_then_else(Expr::var("edge"), Expr::var("GAIN"), Expr::int_lit(0)),
            },
            Equation {
                lhs: vec!["st".into()],
                rhs: Expr::if_then_else(Expr::var("edge"), Expr::var("Armed"), Expr::var("Safe")),
            },
        ],
        contract: Some("Main_contract".into()),
        diagram: Default::default(),
            probes: vec![],
    };
    let unrelated = NodeDef {
        name: "Unrelated".into(),
        kind: NodeKind::Function,
        inputs: vec![Port { name: "x".into(), ty: Type::named("UnusedRecord") }],
        outputs: vec![Port { name: "y".into(), ty: Type::Bool }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::Field {
                base: Box::new(Expr::var("x")),
                field: "flag".into(),
            },
        }],
        contract: Some("Unrelated_contract".into()),
        diagram: Default::default(),
            probes: vec![],
    };
    let main_contract = serde_json::json!({
        "name": "Main_contract",
        "inputs": main.inputs,
        "outputs": main.outputs,
        "ghost_vars": [], "assumptions": [],
        "guarantees": [
            { "name": "edge_implies_button", "expr": Expr::implies(Expr::var("edge"), Expr::var("button")) }
        ],
        "modes": [], "imports": []
    });
    let unrelated_contract = serde_json::json!({
        "name": "Unrelated_contract",
        "inputs": unrelated.inputs,
        "outputs": unrelated.outputs,
        "ghost_vars": [], "assumptions": [], "guarantees": [], "modes": [], "imports": []
    });
    let mut project = Project {
        name: "cluttered".into(),
        packages: vec![Package {
            name: "user".into(),
            types: vec![
                TypeDef {
                    body: TypeBody::Enum(EnumDef {
                        name: "ArmState".into(),
                        variants: vec!["Safe".into(), "Armed".into()],
                    }),
                },
                TypeDef {
                    body: TypeBody::Record {
                        name: "UnusedRecord".into(),
                        fields: vec![RecordField { name: "flag".into(), ty: Type::Bool }],
                    },
                },
            ],
            constants: vec![
                ConstDef { name: "BASE".into(), ty: Type::Int32, value: Expr::int_lit(2) },
                ConstDef {
                    name: "GAIN".into(),
                    ty: Type::Int32,
                    value: Expr::bin(ol_ir::BinOp::Mul, Expr::var("BASE"), Expr::int_lit(21)),
                },
                ConstDef { name: "UNUSED_K".into(), ty: Type::Int32, value: Expr::int_lit(99) },
            ],
            nodes: vec![main, unrelated],
            contracts: vec![main_contract, unrelated_contract],
            ..Default::default()
        }],
        main: Some("Main".into()),
        ..Default::default()
    };
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("stdlib loads");
    lib.merge_into(&mut project, "stdlib");
    project
}

#[test]
fn slice_keeps_exactly_the_used_closure() {
    let project = cluttered_project();
    let sliced = project.slice_for_root("Main").expect("slices");

    let node_names: Vec<&str> = sliced.all_nodes().map(|n| n.name.as_str()).collect();
    assert!(node_names.contains(&"Main"));
    assert!(node_names.contains(&"RisingEdge"), "called stdlib block kept");
    assert!(!node_names.contains(&"Unrelated"), "unrelated node dropped");
    assert!(!node_names.contains(&"Watchdog"), "uncalled stdlib dropped");
    assert!(!node_names.contains(&"BitAnd"), "uncalled stdlib dropped");
    assert_eq!(sliced.main.as_deref(), Some("Main"));

    let type_names: Vec<&str> = sliced
        .packages
        .iter()
        .flat_map(|p| p.types.iter().map(|t| t.name()))
        .collect();
    assert!(
        type_names.contains(&"ArmState"),
        "enum kept via variant reference, got {type_names:?}"
    );
    assert!(!type_names.contains(&"UnusedRecord"), "unused type dropped");

    let const_names: Vec<&str> = sliced
        .packages
        .iter()
        .flat_map(|p| p.constants.iter().map(|c| c.name.as_str()))
        .collect();
    assert!(const_names.contains(&"GAIN"), "referenced constant kept");
    assert!(
        const_names.contains(&"BASE"),
        "constant referenced by a kept constant kept"
    );
    assert!(!const_names.contains(&"UNUSED_K"), "unused constant dropped");

    let contract_names: Vec<String> = sliced
        .packages
        .iter()
        .flat_map(|p| p.contracts.iter())
        .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    assert!(contract_names.iter().any(|c| c == "Main_contract"));
    assert!(
        !contract_names.iter().any(|c| c == "Unrelated_contract"),
        "unrelated contract dropped"
    );
}

#[test]
fn sliced_project_typechecks_clean() {
    let project = cluttered_project();
    let sliced = project.slice_for_root("Main").expect("slices");
    let report = ol_typecheck::check_project(&sliced);
    assert!(
        !report.has_errors(),
        "sliced project has errors: {:?}",
        report.errors().map(|d| d.render()).collect::<Vec<_>>()
    );
}

#[test]
fn sliced_simulation_is_identical_to_full_project_simulation() {
    let project = cluttered_project();
    let sliced = project.slice_for_root("Main").expect("slices");

    let mut full_sim = Sim::new(&project, "Main").unwrap();
    let mut sliced_sim = Sim::new(&sliced, "Main").unwrap();

    for button in [false, true, true, false, true] {
        let mut inputs = BTreeMap::new();
        inputs.insert("button".to_string(), Value::Bool(button));
        let full = full_sim.step(&inputs).unwrap();
        let thin = sliced_sim.step(&inputs).unwrap();
        assert_eq!(full, thin, "button={button}");
    }
}

#[test]
fn sliced_c_contains_only_used_step_functions() {
    let project = cluttered_project();
    let sliced = project.slice_for_root("Main").expect("slices");
    let bundle = ol_clite_emit::emit_project(&sliced);
    assert!(bundle.source.contains("void Main_step"));
    assert!(bundle.source.contains("void RisingEdge_step"));
    assert!(!bundle.source.contains("Watchdog_step"), "uncalled block leaked into C");
    assert!(!bundle.source.contains("Unrelated_step"), "unrelated node leaked into C");
    assert!(bundle.header.contains("} ArmState;"), "used enum typedef present");
    assert!(!bundle.header.contains("UnusedRecord"), "unused type leaked into C");
    assert!(bundle.header.contains("#define GAIN"));
    assert!(!bundle.header.contains("#define UNUSED_K"));
}

#[test]
fn unknown_root_is_an_error() {
    let project = cluttered_project();
    let err = project.slice_for_root("Nope").unwrap_err();
    assert!(err.contains("Nope"), "got: {err}");
}
