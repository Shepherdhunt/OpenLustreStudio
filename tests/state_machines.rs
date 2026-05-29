//! State machines end-to-end: build an SM, lower it to dataflow, typecheck
//! the lowered model, and simulate it cycle-by-cycle.

use std::collections::BTreeMap;

use ol_ir::{
    Equation, Expr, NodeKind, Package, Port, Project, StateDef, StateMachineDef, Transition,
    Type,
};
use ol_sim::{Sim, Value};

/// Two-state Toggle: `pulse` flips light off->on->off. The light comes on
/// the cycle AFTER `pulse` is high, which is the dataflow shape `OFF -> pre
/// next_state` produces.
fn toggle_machine() -> StateMachineDef {
    StateMachineDef {
        name: "Toggle".into(),
        inputs: vec![Port { name: "pulse".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "light".into(), ty: Type::Bool }],
        locals: vec![],
        initial_state: "OFF".into(),
        states: vec![
            StateDef {
                name: "OFF".into(),
                equations: vec![Equation {
                    lhs: vec!["light".into()],
                    rhs: Expr::bool_lit(false),
                }],
                transitions: vec![Transition {
                    guard: Expr::var("pulse"),
                    target: "ON".into(),
                }],
            },
            StateDef {
                name: "ON".into(),
                equations: vec![Equation {
                    lhs: vec!["light".into()],
                    rhs: Expr::bool_lit(true),
                }],
                transitions: vec![Transition {
                    guard: Expr::var("pulse"),
                    target: "OFF".into(),
                }],
            },
        ],
        contract: None,
    }
}

fn project_with(sm: StateMachineDef) -> Project {
    Project {
        name: "sm_test".into(),
        packages: vec![Package {
            name: "user".into(),
            state_machines: vec![sm],
            ..Default::default()
        }],
        main: Some("Toggle".into()),
    }
}

#[test]
fn lowering_emits_a_state_enum_and_a_node() {
    let mut project = project_with(toggle_machine());
    project.lower_state_machines().expect("lowers cleanly");

    let pkg = &project.packages[0];
    assert_eq!(pkg.state_machines.len(), 0, "state machines should be consumed");
    let ty = pkg
        .types
        .iter()
        .find(|t| t.name() == "Toggle_StateEnum")
        .expect("state enum present");
    match &ty.body {
        ol_ir::TypeBody::Enum(e) => {
            assert_eq!(e.variants, vec!["OFF".to_string(), "ON".to_string()]);
        }
        _ => panic!("Toggle_StateEnum should be an enum"),
    }

    let node = pkg.find_node("Toggle").expect("Toggle node lowered");
    assert_eq!(node.kind, NodeKind::Operator);
    assert!(node.locals.iter().any(|l| l.name == "__sm_state"));
    assert!(node.locals.iter().any(|l| l.name == "__sm_next_state"));
    // state, next_state, light = 3 equations.
    assert_eq!(node.equations.len(), 3);
}

#[test]
fn lowered_machine_typechecks() {
    let mut project = project_with(toggle_machine());
    project.lower_state_machines().unwrap();
    let report = ol_typecheck::check_project(&project);
    assert!(
        !report.has_errors(),
        "typecheck errors: {:?}",
        report.errors().map(|d| d.render()).collect::<Vec<_>>()
    );
}

#[test]
fn simulator_drives_state_transitions_one_cycle_at_a_time() {
    let mut project = project_with(toggle_machine());
    project.lower_state_machines().unwrap();

    let mut sim = Sim::new(&project, "Toggle").unwrap();

    // Pulse sequence: F T T F F T F
    //
    // Each cycle the machine reads `__sm_state` (driven from prev next_state),
    // computes outputs and next_state, then snapshots. So `light` lags
    // transitions by one cycle.
    let pulses = [false, true, true, false, false, true, false];
    let mut lights = Vec::new();
    for p in pulses {
        let mut inputs = BTreeMap::new();
        inputs.insert("pulse".into(), Value::Bool(p));
        let out = sim.step(&inputs).unwrap();
        lights.push(out["light"].as_bool().unwrap());
    }
    // Cycle 0: state=OFF, light=false. pulse=F -> next_state=OFF.
    // Cycle 1: state=OFF, light=false. pulse=T -> next_state=ON.
    // Cycle 2: state=ON,  light=true.  pulse=T -> next_state=OFF.
    // Cycle 3: state=OFF, light=false. pulse=F -> next_state=OFF.
    // Cycle 4: state=OFF, light=false.
    // Cycle 5: state=OFF, light=false. pulse=T -> next_state=ON.
    // Cycle 6: state=ON,  light=true.  pulse=F -> next_state=ON.
    assert_eq!(lights, vec![false, false, true, false, false, false, true]);
}

#[test]
fn lowering_rejects_unknown_initial_state() {
    let mut bad = toggle_machine();
    bad.initial_state = "NOWHERE".into();
    let mut project = project_with(bad);
    let errs = project.lower_state_machines().unwrap_err();
    assert!(matches!(
        errs[0],
        ol_ir::state_machine::LowerError::UnknownInitialState(_, _)
    ));
}

#[test]
fn lowering_rejects_output_not_assigned_in_every_state() {
    let mut bad = toggle_machine();
    bad.states[1].equations.clear(); // ON no longer assigns light
    let mut project = project_with(bad);
    let errs = project.lower_state_machines().unwrap_err();
    assert!(matches!(
        errs[0],
        ol_ir::state_machine::LowerError::OutputUnassigned(_, _, _)
    ));
}

#[test]
fn lowering_rejects_unknown_transition_target() {
    let mut bad = toggle_machine();
    bad.states[0].transitions[0].target = "GHOST".into();
    let mut project = project_with(bad);
    let errs = project.lower_state_machines().unwrap_err();
    assert!(matches!(
        errs[0],
        ol_ir::state_machine::LowerError::UnknownTarget(_, _, _)
    ));
}

/// Mealy-style traffic light: 3 states (Red, Green, Yellow), inputs
/// tick/emergency, outputs go/warn. Exercises more states and a guard that
/// references inputs other than the prior cycle's outputs.
#[test]
fn three_state_traffic_light_simulates_correctly() {
    let inputs = vec![
        Port { name: "tick".into(), ty: Type::Bool },
        Port { name: "emergency".into(), ty: Type::Bool },
    ];
    let outputs = vec![
        Port { name: "go".into(), ty: Type::Bool },
        Port { name: "warn".into(), ty: Type::Bool },
    ];
    // States: Red (go=F, warn=F), Green (go=T, warn=F), Yellow (go=F, warn=T).
    // Transitions: emergency from anywhere -> Red; otherwise tick advances
    // Red -> Green -> Yellow -> Red.
    let make_state = |name: &str, go: bool, warn: bool, advance_to: &str| StateDef {
        name: name.into(),
        equations: vec![
            Equation {
                lhs: vec!["go".into()],
                rhs: Expr::bool_lit(go),
            },
            Equation {
                lhs: vec!["warn".into()],
                rhs: Expr::bool_lit(warn),
            },
        ],
        transitions: vec![
            // Higher-priority transition listed first (linear search in the
            // chain).
            Transition {
                guard: Expr::var("emergency"),
                target: "Red".into(),
            },
            Transition {
                guard: Expr::var("tick"),
                target: advance_to.into(),
            },
        ],
    };
    let sm = StateMachineDef {
        name: "TrafficLight".into(),
        inputs,
        outputs,
        locals: vec![],
        initial_state: "Red".into(),
        states: vec![
            make_state("Red", false, false, "Green"),
            make_state("Green", true, false, "Yellow"),
            make_state("Yellow", false, true, "Red"),
        ],
        contract: None,
    };
    let mut project = Project {
        name: "tl".into(),
        packages: vec![Package {
            name: "user".into(),
            state_machines: vec![sm],
            ..Default::default()
        }],
        main: Some("TrafficLight".into()),
    };
    project.lower_state_machines().unwrap();
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "TrafficLight").unwrap();

    // Run: tick=T four times (Red -> Green -> Yellow -> Red -> Green),
    //      then emergency=T (stays Green this cycle, transitions next).
    let inputs_seq: Vec<(bool, bool)> = vec![
        (false, false), // c0: Red, go=F warn=F
        (true, false),  // c1: still Red, then -> Green
        (true, false),  // c2: Green, go=T warn=F, then -> Yellow
        (true, false),  // c3: Yellow, go=F warn=T, then -> Red
        (true, false),  // c4: Red, then -> Green
        (false, true),  // c5: Green, but emergency next -> Red
        (false, false), // c6: Red
    ];
    let mut trace = Vec::new();
    for (tick, em) in inputs_seq {
        let mut inputs = BTreeMap::new();
        inputs.insert("tick".into(), Value::Bool(tick));
        inputs.insert("emergency".into(), Value::Bool(em));
        let out = sim.step(&inputs).unwrap();
        trace.push((
            out["go"].as_bool().unwrap(),
            out["warn"].as_bool().unwrap(),
        ));
    }
    let expected = vec![
        (false, false), // Red
        (false, false), // Red
        (true, false),  // Green
        (false, true),  // Yellow
        (false, false), // Red
        (true, false),  // Green (emergency only takes effect on next cycle)
        (false, false), // Red
    ];
    assert_eq!(trace, expected);
}
