//! End-to-end test that a user model can call stateful standard-library
//! operators (RisingEdge, Counter, Latch) and the simulator threads per-call
//! state correctly across cycles.

use std::path::PathBuf;

use ol_ir::{Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};
use ol_sim::Sim;

fn libraries_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries")
}

/// Build a user project whose entry point counts the rising edges of `button`,
/// then runs that count through `Latch` reset/set logic. This exercises three
/// distinct stateful subnode call sites in one model.
fn build_edge_counter_project() -> Project {
    let inputs = vec![
        Port { name: "button".into(), ty: Type::Bool },
        Port { name: "clear".into(), ty: Type::Bool },
    ];
    let outputs = vec![
        Port { name: "edge".into(), ty: Type::Bool },
        Port { name: "count".into(), ty: Type::Int32 },
        Port { name: "armed".into(), ty: Type::Bool },
    ];

    // edge  = RisingEdge(button)
    // count = Counter(clear, edge)
    // armed = Latch(edge, clear)
    let equations = vec![
        Equation {
            lhs: vec!["edge".into()],
            rhs: Expr::call("RisingEdge", vec![Expr::var("button")]),
        },
        Equation {
            lhs: vec!["count".into()],
            rhs: Expr::call("Counter", vec![Expr::var("clear"), Expr::var("edge")]),
        },
        Equation {
            lhs: vec!["armed".into()],
            rhs: Expr::call("Latch", vec![Expr::var("edge"), Expr::var("clear")]),
        },
    ];

    let node = NodeDef {
        name: "EdgeCounter".into(),
        kind: NodeKind::Operator,
        inputs,
        outputs,
        locals: vec![],
        equations,
        contract: None,
        diagram: Default::default(),
    };

    Project {
        name: "edge_counter".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some("EdgeCounter".into()),
    }
}

fn run(csv: &str) -> Vec<(bool, i64, bool)> {
    let mut project = build_edge_counter_project();
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("stdlib loads");
    lib.merge_into(&mut project, "stdlib");

    // The folded project should pass the regular check passes.
    let report = ol_typecheck::check_project(&project);
    assert!(
        !report.has_errors(),
        "typecheck errors after stdlib merge: {:?}",
        report
            .errors()
            .map(|d| d.render())
            .collect::<Vec<_>>()
    );

    let mut sim = Sim::new(&project, "EdgeCounter").expect("sim builds");
    let trace = sim.run_csv(csv).expect("sim runs");
    trace
        .rows
        .iter()
        .map(|row| {
            let edge = row[1].as_bool().expect("edge bool");
            let count = row[2].as_int().expect("count int");
            let armed = row[3].as_bool().expect("armed bool");
            (edge, count, armed)
        })
        .collect()
}

#[test]
fn rising_edge_fires_only_on_low_to_high_transitions() {
    // Cycle: 0 1 2 3 4 5 6
    // button F T T F T T F
    // clear  F F F F F F F
    // Expected edges at cycles 1 and 4 only.
    let csv = "\
button,clear
false,false
true,false
true,false
false,false
true,false
true,false
false,false
";
    let out = run(csv);
    let edges: Vec<bool> = out.iter().map(|(e, _, _)| *e).collect();
    assert_eq!(edges, vec![false, true, false, false, true, false, false]);
}

#[test]
fn counter_accumulates_edges_and_clear_resets() {
    // Two rising edges followed by a clear at cycle 5.
    let csv = "\
button,clear
false,false
true,false
false,false
true,false
true,false
false,true
true,false
true,false
";
    let out = run(csv);
    let counts: Vec<i64> = out.iter().map(|(_, c, _)| *c).collect();
    // Edges fire at cycles 1 and 3 (count becomes 1 then 2). Cycle 5 clears
    // (count = 0). Cycle 6 has a rising edge again -> count = 1. Cycle 7 holds.
    assert_eq!(counts, vec![0, 1, 1, 2, 2, 0, 1, 1]);
}

#[test]
fn latch_arms_on_edge_and_clears_on_reset() {
    let csv = "\
button,clear
false,false
true,false
false,false
false,false
false,true
false,false
true,false
";
    let out = run(csv);
    let armed: Vec<bool> = out.iter().map(|(_, _, a)| *a).collect();
    // Latch: edge sets, clear resets. Cycle 1 edge -> armed. Stays armed
    // through cycle 3. Cycle 4 clear -> not armed. Cycle 6 edge -> armed.
    assert_eq!(armed, vec![false, true, true, true, false, false, true]);
}

#[test]
fn merge_into_does_not_clobber_existing_node() {
    // If the user defines their own `Counter`, the stdlib's must be skipped.
    let mut project = Project {
        name: "p".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![NodeDef {
                name: "Counter".into(),
                kind: NodeKind::Function,
                inputs: vec![Port { name: "x".into(), ty: Type::Bool }],
                outputs: vec![Port { name: "y".into(), ty: Type::Bool }],
                locals: vec![],
                equations: vec![Equation {
                    lhs: vec!["y".into()],
                    rhs: Expr::var("x"),
                }],
                contract: None,
                diagram: Default::default(),
            }],
            ..Default::default()
        }],
        main: None,
    };
    let lib = ol_stdlib::load_dir(&libraries_dir()).unwrap();
    lib.merge_into(&mut project, "stdlib");
    let counters: Vec<_> = project
        .all_nodes()
        .filter(|n| n.name == "Counter")
        .collect();
    assert_eq!(counters.len(), 1, "user's Counter should not be shadowed");
    assert!(counters[0].is_function(), "user's Counter should remain");
}
