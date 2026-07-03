//! End-to-end coverage of the Phase 9 safety / observer additions: Timer,
//! Watchdog, RangeCheck, RateMonitor, Assert, Assume. Confirms each block
//! loads from the YAML library and behaves as expected when called from
//! a user model.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ol_ir::{Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};
use ol_sim::{Sim, Value};

fn libraries_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries")
}

fn build_with(node: NodeDef, main: &str) -> Project {
    let mut p = Project {
        name: "safety_test".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some(main.into()),
        ..Default::default()
    };
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("stdlib loads");
    lib.merge_into(&mut p, "stdlib");
    p
}

#[test]
fn watchdog_trips_after_limit_cycles_without_a_feed() {
    // node Sentinel(feed: bool) returns (fault: bool)
    //   (fault, _count) = Watchdog(feed, 3);
    let node = NodeDef {
        name: "Sentinel".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "feed".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "fault".into(), ty: Type::Bool }],
        locals: vec![ol_ir::Local { name: "ignored_count".into(), ty: Type::Int32 }],
        equations: vec![Equation {
            lhs: vec!["fault".into(), "ignored_count".into()],
            rhs: Expr::call("Watchdog", vec![Expr::var("feed"), Expr::int_lit(3)]),
        }],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };
    let project = build_with(node, "Sentinel");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "Sentinel").unwrap();
    // feeds:  T  F  F  F  F  T  F  F  F  F
    // count:  0  1  2  3  4  0  1  2  3  4
    // fault:  F  F  F  T  T  F  F  F  T  T
    let feeds = [true, false, false, false, false, true, false, false, false, false];
    let expected_fault = [false, false, false, true, true, false, false, false, true, true];
    for (i, f) in feeds.into_iter().enumerate() {
        let mut inputs = BTreeMap::new();
        inputs.insert("feed".into(), Value::Bool(f));
        let out = sim.step(&inputs).unwrap();
        assert_eq!(
            out["fault"].as_bool(),
            Some(expected_fault[i]),
            "cycle {i}: feed={f}"
        );
    }
}

#[test]
fn timer_elapses_at_the_limit_and_resets_when_asked() {
    let node = NodeDef {
        name: "Driver".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "reset".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "elapsed".into(), ty: Type::Bool }],
        locals: vec![ol_ir::Local { name: "ignored".into(), ty: Type::Int32 }],
        equations: vec![Equation {
            lhs: vec!["elapsed".into(), "ignored".into()],
            rhs: Expr::call("Timer", vec![Expr::var("reset"), Expr::int_lit(2)]),
        }],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };
    let project = build_with(node, "Driver");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "Driver").unwrap();
    // The Timer body is `count = if reset then 0 else (0 -> pre count) + 1`,
    // so the first cycle (no reset) already increments to 1.
    // resets: F F F F T F F F
    // count:  1 2 3 4 0 1 2 3
    // elap:   F T T T F F T T
    let resets = [false, false, false, false, true, false, false, false];
    let expected = [false, true, true, true, false, false, true, true];
    for (i, r) in resets.into_iter().enumerate() {
        let mut inputs = BTreeMap::new();
        inputs.insert("reset".into(), Value::Bool(r));
        let out = sim.step(&inputs).unwrap();
        assert_eq!(out["elapsed"].as_bool(), Some(expected[i]), "cycle {i}");
    }
}

#[test]
fn range_check_is_stateless_and_correct() {
    let node = NodeDef {
        name: "InBand".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port { name: "x".into(), ty: Type::Int32 },
            Port { name: "lo".into(), ty: Type::Int32 },
            Port { name: "hi".into(), ty: Type::Int32 },
        ],
        outputs: vec![Port { name: "ok".into(), ty: Type::Bool }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["ok".into()],
            rhs: Expr::call(
                "RangeCheck",
                vec![Expr::var("x"), Expr::var("lo"), Expr::var("hi")],
            ),
        }],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };
    let project = build_with(node, "InBand");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "InBand").unwrap();
    let cases = [
        (5, 0, 10, true),
        (-1, 0, 10, false),
        (10, 0, 10, true),
        (11, 0, 10, false),
        (0, 0, 0, true),
    ];
    for (x, lo, hi, expected) in cases {
        let mut inputs = BTreeMap::new();
        inputs.insert("x".into(), Value::Int(x));
        inputs.insert("lo".into(), Value::Int(lo));
        inputs.insert("hi".into(), Value::Int(hi));
        let out = sim.step(&inputs).unwrap();
        assert_eq!(
            out["ok"].as_bool(),
            Some(expected),
            "RangeCheck({x}, {lo}, {hi})"
        );
    }
}

#[test]
fn rate_monitor_returns_x_on_first_cycle_then_first_difference() {
    let node = NodeDef {
        name: "Rate".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "x".into(), ty: Type::Int32 }],
        outputs: vec![Port { name: "r".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["r".into()],
            rhs: Expr::call("RateMonitor", vec![Expr::var("x")]),
        }],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };
    let project = build_with(node, "Rate");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "Rate").unwrap();
    // x:  5  7  10  10  3
    // r:  5  2  3   0  -7   (first cycle: x - 0 = x; thereafter x - prev x)
    let xs = [5, 7, 10, 10, 3];
    let expected = [5, 2, 3, 0, -7];
    for (i, x) in xs.into_iter().enumerate() {
        let mut inputs = BTreeMap::new();
        inputs.insert("x".into(), Value::Int(x));
        let out = sim.step(&inputs).unwrap();
        assert_eq!(out["r"].as_int(), Some(expected[i]), "cycle {i}");
    }
}

#[test]
fn assert_and_assume_pass_their_input_through() {
    // Assert(true) and Assume(true) both yield ok=true. Both blocks exist as
    // contract carriers; their value as a runtime pass-through is incidental
    // but it's worth confirming the dataflow side wires up.
    let node = NodeDef {
        name: "Witness".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "p".into(), ty: Type::Bool }],
        outputs: vec![
            Port { name: "asserted".into(), ty: Type::Bool },
            Port { name: "assumed".into(), ty: Type::Bool },
        ],
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["asserted".into()],
                rhs: Expr::call("Assert", vec![Expr::var("p")]),
            },
            Equation {
                lhs: vec!["assumed".into()],
                rhs: Expr::call("Assume", vec![Expr::var("p")]),
            },
        ],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };
    let project = build_with(node, "Witness");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "Witness").unwrap();
    for p in [true, false, true] {
        let mut inputs = BTreeMap::new();
        inputs.insert("p".into(), Value::Bool(p));
        let out = sim.step(&inputs).unwrap();
        assert_eq!(out["asserted"].as_bool(), Some(p));
        assert_eq!(out["assumed"].as_bool(), Some(p));
    }
}

#[test]
fn library_now_has_31_blocks() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("library loads");
    let names: Vec<&str> = lib.nodes().map(|n| n.name.as_str()).collect();
    for added in [
        "Timer",
        "Watchdog",
        "RangeCheck",
        "RateMonitor",
        "Assert",
        "Assume",
    ] {
        assert!(names.contains(&added), "missing block `{added}`");
    }
    assert!(
        lib.entries.len() >= 31,
        "expected >= 31 blocks, got {}",
        lib.entries.len()
    );
}
