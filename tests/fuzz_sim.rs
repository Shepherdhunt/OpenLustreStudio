//! Fuzz simulation: the type-aware random-input explorer (`ol_sim::fuzz`).
//!
//! The engine's contract: deterministic for a seed, findings deduplicated
//! with the first occurrence carrying a replayable input trace, automatic
//! crash detection (evaluation errors AND interpreter panics), user error
//! predicates over the full watch view (temporal operators allowed), and
//! type-aware generation (enum inputs only ever produce declared variants).

use std::collections::BTreeMap;

use ol_ir::{
    EnumDef, Equation, Expr, Literal, NodeDef, NodeKind, Package, Port, Project, Type, TypeBody,
    TypeDef,
};
use ol_sim::fuzz::{fuzz_node, FindingKind, FuzzConfig, FuzzPredicate};

fn e(s: &str) -> Expr {
    ol_stdlib::parse_expr(s).expect(s)
}

fn node(name: &str, inputs: Vec<Port>, outputs: Vec<Port>, equations: Vec<Equation>) -> NodeDef {
    NodeDef {
        name: name.into(),
        kind: NodeKind::Function,
        inputs,
        outputs,
        locals: vec![],
        equations,
        contract: None,
        diagram: Default::default(),
        probes: vec![],
        requirements: vec![],
        sysml: None,
        generics: vec![],
    }
}

fn project(nodes: Vec<NodeDef>, types: Vec<TypeDef>) -> Project {
    let main = nodes.first().map(|n| n.name.clone());
    Project {
        name: "fuzzed".into(),
        packages: vec![Package { name: "user".into(), nodes, types, ..Default::default() }],
        main,
        ..Default::default()
    }
}

fn port(name: &str, ty: Type) -> Port {
    Port { name: name.into(), ty }
}

fn eq(lhs: &str, rhs: Expr) -> Equation {
    Equation { lhs: vec![lhs.into()], rhs }
}

// --- Automatic crash detection ------------------------------------------------

#[test]
fn fuzz_finds_a_planted_division_by_zero_and_the_trace_replays() {
    // y = 100 / x crashes exactly when x == 0 — the generator's boundary menu
    // makes that a certainty within a few iterations.
    let p = project(
        vec![node(
            "Div",
            vec![port("x", Type::Int32)],
            vec![port("y", Type::Int32)],
            vec![eq("y", e("100 / x"))],
        )],
        vec![],
    );
    let cfg = FuzzConfig { cycles: 10, iterations: 50, seed: 7, ..Default::default() };
    let report = fuzz_node(&p, "Div", &cfg).expect("fuzz runs");

    let crash = report
        .findings
        .iter()
        .find(|f| f.kind == FindingKind::Crash)
        .expect("the division by zero is found");
    assert!(crash.detail.contains("division by zero"), "got: {}", crash.detail);
    assert_eq!(crash.columns, vec!["x".to_string()]);
    assert_eq!(crash.rows.len(), crash.cycle + 1, "trace runs up to the failing cycle");
    assert_eq!(crash.rows[crash.cycle], vec!["0".to_string()], "the failing input is x = 0");

    // The trace REPRODUCES: replaying the recorded inputs through a fresh
    // simulator hits the same error at the same cycle.
    let mut sim = ol_sim::Sim::new(&p, "Div").unwrap();
    for (cycle, row) in crash.rows.iter().enumerate() {
        let mut inputs = BTreeMap::new();
        inputs.insert("x".to_string(), ol_sim::Value::Int(row[0].parse().unwrap()));
        match sim.step(&inputs) {
            Ok(_) => assert!(cycle < crash.cycle, "replay crashed early"),
            Err(err) => {
                assert_eq!(cycle, crash.cycle, "replay crashes at the recorded cycle");
                assert!(err.to_string().contains("division by zero"));
                return;
            }
        }
    }
    panic!("replay never crashed");
}

#[test]
fn fuzz_catches_interpreter_panics_as_crash_findings() {
    // Debug builds panic on i64 overflow: y = x * x * x with full-range int64
    // inputs overflows quickly. The fuzzer must survive and report it, not die.
    let p = project(
        vec![node(
            "Cube",
            vec![port("x", Type::Int64)],
            vec![port("y", Type::Int64)],
            vec![eq("y", e("x * x * x"))],
        )],
        vec![],
    );
    let cfg = FuzzConfig { cycles: 10, iterations: 60, seed: 3, ..Default::default() };
    let report = fuzz_node(&p, "Cube", &cfg).expect("fuzz survives the panic");
    let crash = report
        .findings
        .iter()
        .find(|f| f.kind == FindingKind::Crash && f.detail.starts_with("panic:"))
        .expect("the overflow panic is reported as a finding");
    assert!(crash.detail.contains("overflow"), "got: {}", crash.detail);
}

// --- User error predicates ------------------------------------------------------

#[test]
fn fuzz_fires_user_error_predicates_and_reports_outputs() {
    let p = project(
        vec![node(
            "Doubler",
            vec![port("x", Type::Int32)],
            vec![port("y", Type::Int32)],
            vec![eq("y", e("x + x"))],
        )],
        vec![],
    );
    let cfg = FuzzConfig {
        cycles: 10,
        iterations: 50,
        seed: 11,
        predicates: vec![FuzzPredicate { name: "y out of budget".into(), expr: e("y > 100 or y < -100") }],
        ..Default::default()
    };
    let report = fuzz_node(&p, "Doubler", &cfg).expect("fuzz runs");
    let hit = report
        .findings
        .iter()
        .find(|f| f.kind == FindingKind::Predicate)
        .expect("the predicate fires");
    assert_eq!(hit.detail, "y out of budget");
    // The recorded failing input actually violates the budget.
    let x: i64 = hit.rows[hit.cycle][0].parse().unwrap();
    assert!(2 * x > 100 || 2 * x < -100, "x = {x}");
    // And the outputs snapshot shows the offending y.
    let y = hit.outputs.iter().find(|(k, _)| k == "y").map(|(_, v)| v.clone()).unwrap();
    assert_eq!(y.parse::<i64>().unwrap(), 2 * x);
}

#[test]
fn fuzz_rejects_a_predicate_that_does_not_typecheck() {
    let p = project(
        vec![node(
            "Id",
            vec![port("x", Type::Int32)],
            vec![port("y", Type::Int32)],
            vec![eq("y", e("x"))],
        )],
        vec![],
    );
    let cfg = FuzzConfig {
        cycles: 5,
        iterations: 5,
        seed: 1,
        predicates: vec![FuzzPredicate { name: "not boolean".into(), expr: e("y + 1") }],
        ..Default::default()
    };
    let err = fuzz_node(&p, "Id", &cfg).unwrap_err().to_string();
    assert!(err.contains("not boolean"), "got: {err}");

    let cfg = FuzzConfig {
        cycles: 5,
        iterations: 5,
        seed: 1,
        predicates: vec![FuzzPredicate { name: "unknown name".into(), expr: e("zz > 0") }],
        ..Default::default()
    };
    let err = fuzz_node(&p, "Id", &cfg).unwrap_err().to_string();
    assert!(err.contains("unknown name"), "got: {err}");
}

// --- Input selection and pinning -------------------------------------------------

#[test]
fn fuzz_scopes_to_selected_inputs_and_pins_the_rest() {
    // Fuzz only `a`; pin `b` to 7. If `b` ever moved, `b <> 7` would fire.
    let p = project(
        vec![node(
            "TwoIn",
            vec![port("a", Type::Int32), port("b", Type::Int32)],
            vec![port("y", Type::Int32)],
            vec![eq("y", e("a + b"))],
        )],
        vec![],
    );
    let mut held = BTreeMap::new();
    held.insert("b".to_string(), "7".to_string());
    let cfg = FuzzConfig {
        fuzz_inputs: vec!["a".into()],
        cycles: 15,
        iterations: 20,
        seed: 5,
        predicates: vec![FuzzPredicate { name: "b moved".into(), expr: e("b <> 7") }],
        held,
        ..Default::default()
    };
    let report = fuzz_node(&p, "TwoIn", &cfg).expect("fuzz runs");
    assert_eq!(report.fuzzed_inputs, vec!["a".to_string()]);
    assert!(
        report.findings.iter().all(|f| f.detail != "b moved"),
        "the pinned input stayed pinned: {:?}",
        report.findings
    );

    // Asking to fuzz an input that doesn't exist is a loud error.
    let cfg = FuzzConfig { fuzz_inputs: vec!["nope".into()], ..Default::default() };
    assert!(fuzz_node(&p, "TwoIn", &cfg).unwrap_err().to_string().contains("no such input"));
}

// --- Type awareness ---------------------------------------------------------------

#[test]
fn fuzz_generates_only_declared_enum_variants() {
    let color = TypeDef {
        body: TypeBody::Enum(EnumDef {
            name: "Color".into(),
            variants: vec!["Red".into(), "Green".into(), "Blue".into()],
        }),
    };
    let p = project(
        vec![node(
            "Pick",
            vec![port("c", Type::Named { name: "Color".into() })],
            vec![port("o", Type::Named { name: "Color".into() })],
            vec![eq("o", e("c"))],
        )],
        vec![color],
    );
    // A temporal predicate over the enum stream: fires the first time the
    // generated input CHANGES variant — so the finding's trace carries the
    // rows generated up to that point, and `pre`/`->` in predicates is
    // exercised against the equation semantics.
    let cfg = FuzzConfig {
        cycles: 8,
        iterations: 6,
        seed: 9,
        predicates: vec![FuzzPredicate {
            name: "probe".into(),
            expr: e("false -> (c <> pre c)"),
        }],
        max_findings: 1,
        ..Default::default()
    };
    let report = fuzz_node(&p, "Pick", &cfg).expect("fuzz runs");
    let probe = report.findings.first().expect("probe fired");
    assert_eq!(probe.kind, FindingKind::Predicate);
    assert!(probe.rows.len() >= 2, "the change needs at least two cycles");
    for row in &probe.rows {
        assert!(
            ["Red", "Green", "Blue"].contains(&row[0].as_str()),
            "generated enum value is a declared variant, got {:?}",
            row[0]
        );
    }
}

// --- Determinism -------------------------------------------------------------------

#[test]
fn fuzz_is_deterministic_for_a_seed() {
    let p = project(
        vec![node(
            "Mix",
            vec![port("x", Type::Int32), port("b", Type::Bool), port("f", Type::Float64)],
            vec![port("y", Type::Int32)],
            vec![eq("y", e("if b then x else 0 - x"))],
        )],
        vec![],
    );
    let cfg = FuzzConfig {
        cycles: 12,
        iterations: 8,
        seed: 42,
        predicates: vec![FuzzPredicate { name: "spike".into(), expr: e("y > 1000") }],
        max_findings: 0,
        ..Default::default()
    };
    let a = fuzz_node(&p, "Mix", &cfg).unwrap();
    let b = fuzz_node(&p, "Mix", &cfg).unwrap();
    assert_eq!(a.total_cycles, b.total_cycles);
    assert_eq!(a.findings.len(), b.findings.len());
    for (fa, fb) in a.findings.iter().zip(b.findings.iter()) {
        assert_eq!(fa.kind, fb.kind);
        assert_eq!(fa.detail, fb.detail);
        assert_eq!(fa.iteration, fb.iteration);
        assert_eq!(fa.cycle, fb.cycle);
        assert_eq!(fa.occurrences, fb.occurrences);
        assert_eq!(fa.rows, fb.rows);
    }
}

// --- Non-finite detection -----------------------------------------------------------

#[test]
fn fuzz_flags_non_finite_outputs() {
    // y = (x*x) * 1e300 overflows f64 to +inf for |x| ≥ 1e6 — values the
    // generator's boundary menu produces routinely.
    let huge = Expr::Const { lit: Literal::Float { value: 1e300 } };
    let xx = Expr::Binary {
        op: ol_ir::BinOp::Mul,
        lhs: Box::new(Expr::var("x")),
        rhs: Box::new(Expr::var("x")),
    };
    let rhs = Expr::Binary { op: ol_ir::BinOp::Mul, lhs: Box::new(xx), rhs: Box::new(huge) };
    let p = project(
        vec![node(
            "Blow",
            vec![port("x", Type::Float64)],
            vec![port("y", Type::Float64)],
            vec![eq("y", rhs)],
        )],
        vec![],
    );
    let cfg = FuzzConfig { cycles: 10, iterations: 60, seed: 2, ..Default::default() };
    let report = fuzz_node(&p, "Blow", &cfg).expect("fuzz runs");
    let hit = report
        .findings
        .iter()
        .find(|f| f.kind == FindingKind::NonFinite)
        .expect("the infinity is flagged");
    assert!(hit.detail.contains("`y`"), "got: {}", hit.detail);
}

// --- Interactive draws: the simulator's Fuzz Operator toggle ---------------------

#[test]
fn random_inputs_draws_type_aware_deterministic_values() {
    let color = TypeDef {
        body: TypeBody::Enum(EnumDef {
            name: "Color".into(),
            variants: vec!["Red".into(), "Green".into(), "Blue".into()],
        }),
    };
    let p = project(
        vec![node(
            "Panel",
            vec![
                port("c", Type::Named { name: "Color".into() }),
                port("x", Type::Int8),
                port("b", Type::Bool),
            ],
            vec![port("o", Type::Int8)],
            vec![eq("o", e("x"))],
        )],
        vec![color],
    );

    let empty = BTreeMap::new();
    let (vals, unfuzzable) =
        ol_sim::fuzz::random_inputs(&p, "Panel", &empty, Some(5)).expect("draw");
    assert!(unfuzzable.is_empty());
    assert!(["Red", "Green", "Blue"].contains(&vals["c"].as_str()), "{vals:?}");
    let x: i64 = vals["x"].parse().unwrap();
    assert!((i8::MIN as i64..=i8::MAX as i64).contains(&x), "int8 range, got {x}");
    assert!(vals["b"] == "true" || vals["b"] == "false", "{vals:?}");

    // Equal seeds draw equal values; the trace is the reproducible record.
    let (again, _) = ol_sim::fuzz::random_inputs(&p, "Panel", &empty, Some(5)).unwrap();
    assert_eq!(vals, again);

    // Stickiness: with a previous value supplied, some seeds hold it and some
    // draw fresh — both behaviors must occur across seeds.
    let mut prev = BTreeMap::new();
    prev.insert("x".to_string(), "7".to_string());
    let (mut held, mut moved) = (false, false);
    for seed in 0..64u64 {
        let (v, _) = ol_sim::fuzz::random_inputs(&p, "Panel", &prev, Some(seed)).unwrap();
        if v["x"] == "7" { held = true } else { moved = true }
    }
    assert!(held && moved, "held={held} moved={moved}");

    assert!(ol_sim::fuzz::random_inputs(&p, "Nope", &empty, Some(1)).is_err());
}
