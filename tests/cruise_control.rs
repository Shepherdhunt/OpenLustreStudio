//! The cruise-control example: a state-machine model that is the worked example
//! shipped under `examples/cruise_control/`. This test both *generates* the
//! example workspace file and *verifies* it across stages — it typechecks
//! cleanly (the Lustre is correct), simulates with sensible cruise behavior
//! (test as a model), and emits C-Lite (the generated code) — exactly the loop
//! a user drives: design → check → simulate → generate.

use std::path::PathBuf;

use ol_ir::{
    BinOp, Equation, Expr, NodeDef, NodeKind, Package, Port, Project, StateDef, StateMachineDef,
    Transition, Type,
};

fn input(name: &str, ty: Type) -> Port {
    Port { name: name.into(), ty }
}

/// `CruiseControl(speed, set_cruise_on, brake, turn_cruise_off, increase_by_one)
/// -> (cruise_active, target_speed)` driven by an owned Off/On state machine.
fn cruise_project() -> Project {
    let inputs = vec![
        input("speed", Type::Int32),
        input("set_cruise_on", Type::Bool),
        input("brake", Type::Bool),
        input("turn_cruise_off", Type::Bool),
        input("increase_by_one", Type::Bool),
    ];
    let outputs = vec![
        input("cruise_active", Type::Bool),
        input("target_speed", Type::Int32),
    ];

    // The operator is the machine's body — no equations of its own.
    let op = NodeDef {
        name: "CruiseControl".into(),
        kind: NodeKind::Operator,
        inputs: inputs.clone(),
        outputs: outputs.clone(),
        locals: vec![],
        equations: vec![],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
    };

    let eq = |lhs: &str, rhs: Expr| Equation { lhs: vec![lhs.into()], rhs };
    // target_speed while cruising: hold the last value (which is the road speed
    // captured the cycle cruise engaged), +1 per `increase_by_one`.
    let hold_plus_incr = Expr::Binary {
        op: BinOp::Add,
        lhs: Box::new(Expr::pre(Expr::var("target_speed"))),
        rhs: Box::new(Expr::if_then_else(
            Expr::var("increase_by_one"),
            Expr::int_lit(1),
            Expr::int_lit(0),
        )),
    };

    let off = StateDef {
        name: "Off".into(),
        equations: vec![
            eq("cruise_active", Expr::bool_lit(false)),
            // While off, target tracks the current road speed, so it is the
            // set-point the moment cruise engages.
            eq("target_speed", Expr::var("speed")),
        ],
        transitions: vec![Transition {
            guard: Expr::var("set_cruise_on"),
            target: "On".into(),
        }],
        regions: vec![],
        refines: None,
    };
    let on = StateDef {
        name: "On".into(),
        equations: vec![
            eq("cruise_active", Expr::bool_lit(true)),
            // `pre` needs an `->` initial value; cycle 0 is always the Off state,
            // so the init (`speed`) is a placeholder — the held set-point comes
            // from the Off state's value the cycle cruise engaged.
            eq("target_speed", Expr::arrow(Expr::var("speed"), hold_plus_incr)),
        ],
        transitions: vec![
            Transition { guard: Expr::var("brake"), target: "Off".into() },
            Transition { guard: Expr::var("turn_cruise_off"), target: "Off".into() },
        ],
        regions: vec![],
        refines: None,
    };

    let sm = StateMachineDef {
        name: "Cruise".into(),
        inputs,
        outputs,
        locals: vec![],
        initial_state: "Off".into(),
        states: vec![off, on],
        contract: None,
        owner: Some("CruiseControl".into()),
    };

    Project {
        name: "cruise_control".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![op],
            state_machines: vec![sm],
            ..Default::default()
        }],
        main: Some("CruiseControl".into()),
        ..Default::default()
    }
}

const SCENARIO: &str = "speed,set_cruise_on,brake,turn_cruise_off,increase_by_one\n\
50,false,false,false,false\n\
55,true,false,false,false\n\
60,false,false,false,false\n\
62,false,false,false,true\n\
64,false,false,false,true\n\
66,false,true,false,false\n\
70,false,false,false,false\n";

#[test]
fn cruise_control_example_generates_checks_simulates_and_emits() {
    // 1) Generate the shipped example workspace (raw project; the Studio lowers
    //    the state machine on open).
    let raw = cruise_project();
    // CARGO_MANIFEST_DIR is the `tests/` package; the example ships at the repo
    // root under `examples/`.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/cruise_control");
    std::fs::create_dir_all(dir.join("scenarios")).unwrap();
    std::fs::write(
        dir.join("cruise_control.wksc"),
        serde_json::to_string_pretty(&raw).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("scenarios").join("drive.csv"), SCENARIO).unwrap();

    // 2) Lower the state machine into the operator and typecheck — the Lustre is
    //    correct iff this is clean.
    let mut project = raw.clone();
    project.lower_state_machines().expect("cruise machine lowers");
    let report = ol_typecheck::check_project(&project);
    assert!(
        !report.has_errors(),
        "cruise control must typecheck clean:\n{}",
        report
            .diagnostics
            .iter()
            .map(|d| d.render())
            .collect::<Vec<_>>()
            .join("\n")
    );

    // 3) Simulate (test as a model).
    let mut sim = ol_sim::Sim::new(&project, "CruiseControl").expect("sim builds");
    let trace = sim.run_csv(SCENARIO).expect("sim runs").to_csv();
    eprintln!("--- cruise trace ---\n{trace}");
    let rows: Vec<Vec<String>> = trace
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split(',').map(|s| s.trim().to_string()).collect())
        .collect();
    let header: Vec<String> = trace.lines().next().unwrap().split(',').map(|s| s.trim().to_string()).collect();
    let col = |name: &str| header.iter().position(|h| h == name).unwrap_or_else(|| panic!("col {name} in {header:?}"));
    let (ca, ts) = (col("cruise_active"), col("target_speed"));
    let active: Vec<&str> = rows.iter().map(|r| r[ca].as_str()).collect();
    let target: Vec<i64> = rows.iter().map(|r| r[ts].parse().unwrap()).collect();

    // Behavioral checks (robust to the exact transition cycle): cruise engages
    // after set_cruise_on, the set-point rises while increase_by_one holds, and
    // braking disengages.
    assert!(active.iter().any(|a| *a == "true"), "cruise should engage: {active:?}");
    assert_eq!(active[0], "false", "starts disengaged");
    assert_eq!(active[6], "false", "braking (cycle 5) disengages by cycle 6: {active:?}");
    let max_target = *target.iter().max().unwrap();
    assert!(max_target >= 56, "increase_by_one must raise the set-point: {target:?}");

    // 4) Emit C-Lite (the generated code).
    let bundle = ol_clite_emit::emit_project(&project);
    assert!(bundle.header.contains("CruiseControl_step"), "generated C exposes the step API");
    assert!(bundle.source.contains("CruiseControl_step"));
}
