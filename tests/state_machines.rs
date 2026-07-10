//! State machines end-to-end: build an SM, lower it to dataflow, typecheck
//! the lowered model, and simulate it cycle-by-cycle.

use std::collections::BTreeMap;

use ol_ir::{
    Equation, Expr, NodeKind, Package, Port, Project, Region, StateDef, StateMachineDef,
    Transition, Type,
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
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
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
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
            },
        ],
        contract: None,
        signals: vec![],
        owner: None,
        diagram: Default::default(),
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
        ..Default::default()
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
        regions: vec![],
        refines: None,
        refine_history: false,
        emits: vec![],
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
        signals: vec![],
        owner: None,
        diagram: Default::default(),
    };
    let mut project = Project {
        name: "tl".into(),
        packages: vec![Package {
            name: "user".into(),
            state_machines: vec![sm],
            ..Default::default()
        }],
        main: Some("TrafficLight".into()),
        ..Default::default()
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

/// Hierarchical automaton: `Mode` has top states Idle/Active; while in Active a
/// nested region runs Lo<->Hi (toggled by `tick`) and drives `level`. `level`
/// is the nested region's value in Active, 0 in Idle; the nested region
/// restarts at Lo each time Active is (re-)entered.
fn hierarchical_mode_machine() -> StateMachineDef {
    let lo = StateDef {
        name: "Lo".into(),
        equations: vec![Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(1) }],
        transitions: vec![Transition { guard: Expr::var("tick"), target: "Hi".into() }],
        regions: vec![],
        refines: None,
        refine_history: false,
        emits: vec![],
    };
    let hi = StateDef {
        name: "Hi".into(),
        equations: vec![Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(2) }],
        transitions: vec![Transition { guard: Expr::var("tick"), target: "Lo".into() }],
        regions: vec![],
        refines: None,
        refine_history: false,
        emits: vec![],
    };
    let idle = StateDef {
        name: "Idle".into(),
        equations: vec![
            Equation { lhs: vec!["mode_active".into()], rhs: Expr::bool_lit(false) },
            Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(0) },
        ],
        transitions: vec![Transition { guard: Expr::var("go"), target: "Active".into() }],
        regions: vec![],
        refines: None,
        refine_history: false,
        emits: vec![],
    };
    let active = StateDef {
        name: "Active".into(),
        // mode_active is driven here; level is driven by the nested region.
        equations: vec![Equation { lhs: vec!["mode_active".into()], rhs: Expr::bool_lit(true) }],
        transitions: vec![Transition { guard: Expr::var("stop"), target: "Idle".into() }],
        regions: vec![Region {
            initial_state: "Lo".into(),
            states: vec![lo, hi],
            history: false,
        }],
        refines: None,
        refine_history: false,
        emits: vec![],
    };
    StateMachineDef {
        name: "Mode".into(),
        inputs: vec![
            Port { name: "go".into(), ty: Type::Bool },
            Port { name: "stop".into(), ty: Type::Bool },
            Port { name: "tick".into(), ty: Type::Bool },
        ],
        outputs: vec![
            Port { name: "mode_active".into(), ty: Type::Bool },
            Port { name: "level".into(), ty: Type::Int32 },
        ],
        locals: vec![],
        initial_state: "Idle".into(),
        states: vec![idle, active],
        contract: None,
        signals: vec![],
        owner: None,
        diagram: Default::default(),
    }
}

#[test]
fn hierarchical_machine_lowers_with_a_region_local_and_two_enums() {
    let mut project = Project {
        name: "h".into(),
        packages: vec![Package { name: "user".into(), state_machines: vec![hierarchical_mode_machine()], ..Default::default() }],
        main: Some("Mode".into()),
        ..Default::default()
    };
    project.lower_state_machines().expect("lowers cleanly");
    assert!(!ol_typecheck::check_project(&project).has_errors());
    let pkg = &project.packages[0];
    // Top enum + one nested-region enum.
    assert!(pkg.types.iter().any(|t| t.name() == "Mode_StateEnum"));
    assert!(pkg.types.iter().any(|t| t.name() == "Mode_r1_StateEnum"));
    // Top state local + the nested region's own state local.
    let node = pkg.find_node("Mode").unwrap();
    assert!(node.locals.iter().any(|l| l.name == "__sm_state"));
    assert!(node.locals.iter().any(|l| l.name == "__sm_r1_state"));
}

/// `Spin` is a flat machine (Lo<->Hi driving `level`); `RefMode` is Idle/Active
/// where Active *refines* Spin. After resolution the behaviour is identical to
/// the inline hierarchical `Mode`, proving refine-by-reference.
fn spin_and_refmode() -> Vec<StateMachineDef> {
    let spin = StateMachineDef {
        name: "Spin".into(),
        inputs: vec![Port { name: "tick".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "level".into(), ty: Type::Int32 }],
        locals: vec![],
        initial_state: "Lo".into(),
        states: vec![
            StateDef {
                name: "Lo".into(),
                equations: vec![Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(1) }],
                transitions: vec![Transition { guard: Expr::var("tick"), target: "Hi".into() }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
            },
            StateDef {
                name: "Hi".into(),
                equations: vec![Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(2) }],
                transitions: vec![Transition { guard: Expr::var("tick"), target: "Lo".into() }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
            },
        ],
        contract: None,
        signals: vec![],
        owner: None,
        diagram: Default::default(),
    };
    let refmode = StateMachineDef {
        name: "RefMode".into(),
        inputs: vec![
            Port { name: "go".into(), ty: Type::Bool },
            Port { name: "stop".into(), ty: Type::Bool },
            Port { name: "tick".into(), ty: Type::Bool },
        ],
        outputs: vec![
            Port { name: "mode_active".into(), ty: Type::Bool },
            Port { name: "level".into(), ty: Type::Int32 },
        ],
        locals: vec![],
        initial_state: "Idle".into(),
        states: vec![
            StateDef {
                name: "Idle".into(),
                equations: vec![
                    Equation { lhs: vec!["mode_active".into()], rhs: Expr::bool_lit(false) },
                    Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(0) },
                ],
                transitions: vec![Transition { guard: Expr::var("go"), target: "Active".into() }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
            },
            StateDef {
                name: "Active".into(),
                equations: vec![Equation { lhs: vec!["mode_active".into()], rhs: Expr::bool_lit(true) }],
                transitions: vec![Transition { guard: Expr::var("stop"), target: "Idle".into() }],
                regions: vec![],
                refines: Some("Spin".into()), // delegate to the Spin machine
                refine_history: false,
                emits: vec![],
            },
        ],
        contract: None,
        signals: vec![],
        owner: None,
        diagram: Default::default(),
    };
    vec![spin, refmode]
}

#[test]
fn refine_resolves_a_sub_machine_and_simulates() {
    let mut project = Project {
        name: "ref".into(),
        packages: vec![Package { name: "user".into(), state_machines: spin_and_refmode(), ..Default::default() }],
        main: Some("RefMode".into()),
        ..Default::default()
    };
    project.lower_state_machines().expect("refine resolves and lowers");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "RefMode").unwrap();
    let seq = [
        (false, false, false),
        (true, false, false),
        (false, false, true),
        (false, false, true),
        (false, true, false),
        (true, false, false),
        (false, false, false),
    ];
    let mut trace = Vec::new();
    for (go, stop, tick) in seq {
        let mut inputs = BTreeMap::new();
        inputs.insert("go".into(), Value::Bool(go));
        inputs.insert("stop".into(), Value::Bool(stop));
        inputs.insert("tick".into(), Value::Bool(tick));
        let out = sim.step(&inputs).unwrap();
        trace.push((out["mode_active"].as_bool().unwrap(), out["level"].as_int().unwrap()));
    }
    // Identical to the inline hierarchical Mode.
    assert_eq!(
        trace,
        vec![(false, 0), (false, 0), (true, 1), (true, 2), (true, 1), (false, 0), (true, 1)]
    );
}

#[test]
fn operator_owned_machine_merges_into_the_operator_and_simulates() {
    // Operator `Lamp` with an empty body; its owned machine drives `on`.
    let lamp = ol_ir::NodeDef {
        name: "Lamp".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "press".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "on".into(), ty: Type::Bool }],
        locals: vec![],
        equations: vec![],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
        requirements: vec![],
        sysml: None,
        generics: vec![],
    };
    let sm = StateMachineDef {
        name: "LampSM".into(),
        inputs: vec![Port { name: "press".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "on".into(), ty: Type::Bool }],
        locals: vec![],
        initial_state: "Off".into(),
        states: vec![
            StateDef {
                name: "Off".into(),
                equations: vec![Equation { lhs: vec!["on".into()], rhs: Expr::bool_lit(false) }],
                transitions: vec![Transition { guard: Expr::var("press"), target: "On".into() }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
            },
            StateDef {
                name: "On".into(),
                equations: vec![Equation { lhs: vec!["on".into()], rhs: Expr::bool_lit(true) }],
                transitions: vec![Transition { guard: Expr::var("press"), target: "Off".into() }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
            },
        ],
        contract: None,
        signals: vec![],
        owner: Some("Lamp".into()), // operator-owned: merge into Lamp's body
        diagram: Default::default(),
    };
    let mut project = Project {
        name: "owned".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![lamp],
            state_machines: vec![sm],
            ..Default::default()
        }],
        main: Some("Lamp".into()),
        ..Default::default()
    };
    project.lower_state_machines().expect("owned machine merges");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    // The machine merged into Lamp — it is NOT a standalone node.
    assert!(project.find_node("LampSM").is_none(), "owned machine must not be a separate node");
    let lamp_node = project.find_node("Lamp").unwrap();
    assert!(lamp_node.locals.iter().any(|l| l.name == "__sm_state"), "state local merged into Lamp");
    assert!(
        lamp_node.equations.iter().any(|e| e.lhs == vec!["on".to_string()]),
        "`on` is driven by the merged automaton"
    );

    // Lamp simulates as the toggle (one-cycle lag).
    let mut sim = Sim::new(&project, "Lamp").unwrap();
    let mut ons = Vec::new();
    for p in [false, true, true, false, false, true, false] {
        let mut inputs = BTreeMap::new();
        inputs.insert("press".into(), Value::Bool(p));
        ons.push(sim.step(&inputs).unwrap()["on"].as_bool().unwrap());
    }
    assert_eq!(ons, vec![false, false, true, false, false, false, true]);
}

#[test]
fn refine_to_unknown_machine_is_rejected() {
    let mut machines = spin_and_refmode();
    machines[1].states[1].refines = Some("Ghost".into()); // Active refines a missing machine
    let mut project = Project {
        name: "ref".into(),
        packages: vec![Package { name: "user".into(), state_machines: machines, ..Default::default() }],
        main: Some("RefMode".into()),
        ..Default::default()
    };
    let errs = project.lower_state_machines().unwrap_err();
    assert!(matches!(errs[0], ol_ir::state_machine::LowerError::UnknownRefine(_, _, _)), "{errs:?}");
}

#[test]
fn hierarchical_machine_simulates_with_restart_on_entry() {
    let mut project = Project {
        name: "h".into(),
        packages: vec![Package { name: "user".into(), state_machines: vec![hierarchical_mode_machine()], ..Default::default() }],
        main: Some("Mode".into()),
        ..Default::default()
    };
    project.lower_state_machines().unwrap();
    let mut sim = Sim::new(&project, "Mode").unwrap();

    // (go, stop, tick) per cycle.
    let seq = [
        (false, false, false), // c0: Idle
        (true, false, false),  // c1: Idle, then -> Active
        (false, false, true),  // c2: Active/Lo, tick -> Hi next
        (false, false, true),  // c3: Active/Hi, tick -> Lo next
        (false, true, false),  // c4: Active/Lo, stop -> Idle next
        (true, false, false),  // c5: Idle, go -> Active next
        (false, false, false), // c6: Active, restarts at Lo
    ];
    let mut trace = Vec::new();
    for (go, stop, tick) in seq {
        let mut inputs = BTreeMap::new();
        inputs.insert("go".into(), Value::Bool(go));
        inputs.insert("stop".into(), Value::Bool(stop));
        inputs.insert("tick".into(), Value::Bool(tick));
        let out = sim.step(&inputs).unwrap();
        trace.push((out["mode_active"].as_bool().unwrap(), out["level"].as_int().unwrap()));
    }
    let expected = vec![
        (false, 0), // c0 Idle
        (false, 0), // c1 Idle
        (true, 1),  // c2 Active, nested Lo
        (true, 2),  // c3 Active, nested Hi
        (true, 1),  // c4 Active, nested Lo
        (false, 0), // c5 Idle (level held at 0, nested frozen)
        (true, 1),  // c6 Active re-entered -> nested restarts at Lo
    ];
    assert_eq!(trace, expected);
}

// --- Signals (SCADE-style boolean events) -------------------------------------

/// Toggle with a `blink` signal: ON emits it, both states mirror it into the
/// `flash` output (same-cycle broadcast), and ON's exit guard reads it too.
fn beacon_machine() -> StateMachineDef {
    StateMachineDef {
        name: "Beacon".into(),
        inputs: vec![Port { name: "pulse".into(), ty: Type::Bool }],
        outputs: vec![
            Port { name: "light".into(), ty: Type::Bool },
            Port { name: "flash".into(), ty: Type::Bool },
        ],
        locals: vec![],
        initial_state: "OFF".into(),
        states: vec![
            StateDef {
                name: "OFF".into(),
                equations: vec![
                    Equation { lhs: vec!["light".into()], rhs: Expr::bool_lit(false) },
                    Equation { lhs: vec!["flash".into()], rhs: Expr::var("blink") },
                ],
                transitions: vec![Transition { guard: Expr::var("pulse"), target: "ON".into() }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
            },
            StateDef {
                name: "ON".into(),
                equations: vec![
                    Equation { lhs: vec!["light".into()], rhs: Expr::bool_lit(true) },
                    Equation { lhs: vec!["flash".into()], rhs: Expr::var("blink") },
                ],
                // The guard reads the signal the state itself emits.
                transitions: vec![Transition {
                    guard: Expr::and(Expr::var("pulse"), Expr::var("blink")),
                    target: "OFF".into(),
                }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec!["blink".into()],
            },
        ],
        contract: None,
        signals: vec!["blink".into()],
        owner: None,
        diagram: Default::default(),
    }
}

#[test]
fn signals_are_true_exactly_while_an_emitting_state_is_active() {
    let mut project = Project {
        name: "sig".into(),
        packages: vec![Package {
            name: "user".into(),
            state_machines: vec![beacon_machine()],
            ..Default::default()
        }],
        main: Some("Beacon".into()),
        ..Default::default()
    };
    project.lower_state_machines().expect("lowers cleanly");
    assert!(
        !ol_typecheck::check_project(&project).has_errors(),
        "{:?}",
        ol_typecheck::check_project(&project).errors().map(|d| d.render()).collect::<Vec<_>>()
    );

    // The signal is an ordinary bool local of the lowered node, with its own
    // equation — so every downstream tool (C, Kind 2, monitors) sees it.
    let node = project.find_node("Beacon").unwrap();
    assert!(node.locals.iter().any(|l| l.name == "blink" && l.ty == Type::Bool));
    assert!(node.equations.iter().any(|e| e.lhs == vec!["blink".to_string()]));
    let c = ol_clite_emit::emit_project(&project);
    assert!(c.source.contains("blink"), "signal local reaches the generated C");

    // flash mirrors the signal, which mirrors "the ON state is active".
    let mut sim = Sim::new(&project, "Beacon").unwrap();
    let mut trace = Vec::new();
    for p in [false, true, true, false, true, false] {
        let mut inputs = BTreeMap::new();
        inputs.insert("pulse".into(), Value::Bool(p));
        let out = sim.step(&inputs).unwrap();
        trace.push((out["light"].as_bool().unwrap(), out["flash"].as_bool().unwrap()));
    }
    // The signal always equals the light (same activation), including the
    // cycle where the guard `pulse and blink` fires the exit.
    assert_eq!(
        trace,
        vec![
            (false, false),
            (false, false),
            (true, true), // ON: emits blink; pulse and blink -> OFF
            (false, false),
            (false, false), // pulse=T -> ON next cycle
            (true, true),
        ]
    );
}

#[test]
fn emitting_an_undeclared_signal_is_rejected() {
    let mut bad = beacon_machine();
    bad.signals.clear();
    let mut project = Project {
        name: "sig".into(),
        packages: vec![Package { name: "user".into(), state_machines: vec![bad], ..Default::default() }],
        ..Default::default()
    };
    let errs = project.lower_state_machines().unwrap_err();
    assert!(matches!(errs[0], ol_ir::state_machine::LowerError::UnknownSignal(_, _, _)), "{errs:?}");
}

#[test]
fn duplicate_and_clashing_signal_names_are_rejected() {
    let mut dup = beacon_machine();
    dup.signals = vec!["blink".into(), "blink".into()];
    let mut project = Project {
        name: "sig".into(),
        packages: vec![Package { name: "user".into(), state_machines: vec![dup], ..Default::default() }],
        ..Default::default()
    };
    let errs = project.lower_state_machines().unwrap_err();
    assert!(matches!(errs[0], ol_ir::state_machine::LowerError::DuplicateSignal(_, _)), "{errs:?}");

    // A signal may not shadow an output…
    let mut clash = beacon_machine();
    clash.signals = vec!["light".into()];
    clash.states[1].emits = vec!["light".into()];
    let mut project = Project {
        name: "sig".into(),
        packages: vec![Package { name: "user".into(), state_machines: vec![clash], ..Default::default() }],
        ..Default::default()
    };
    let errs = project.lower_state_machines().unwrap_err();
    assert!(matches!(errs[0], ol_ir::state_machine::LowerError::SignalClash(_, _)), "{errs:?}");

    // …nor a state name (states are enum variants referenced by bare name).
    let mut clash = beacon_machine();
    clash.signals = vec!["ON".into()];
    clash.states[1].emits = vec!["ON".into()];
    let mut project = Project {
        name: "sig".into(),
        packages: vec![Package { name: "user".into(), state_machines: vec![clash], ..Default::default() }],
        ..Default::default()
    };
    let errs = project.lower_state_machines().unwrap_err();
    assert!(matches!(errs[0], ol_ir::state_machine::LowerError::SignalClash(_, _)), "{errs:?}");
}

/// A refined sub-machine's signal merges into the parent on resolution, so a
/// parent state equation can read what a nested state emits.
#[test]
fn a_sub_machine_signal_is_readable_in_the_parent_after_refinement() {
    let blinker = StateMachineDef {
        name: "Blinker".into(),
        inputs: vec![Port { name: "tick".into(), ty: Type::Bool }],
        outputs: vec![],
        locals: vec![],
        initial_state: "A".into(),
        states: vec![
            StateDef {
                name: "A".into(),
                equations: vec![],
                transitions: vec![Transition { guard: Expr::var("tick"), target: "B".into() }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec![],
            },
            StateDef {
                name: "B".into(),
                equations: vec![],
                transitions: vec![Transition { guard: Expr::var("tick"), target: "A".into() }],
                regions: vec![],
                refines: None,
                refine_history: false,
                emits: vec!["hot".into()],
            },
        ],
        contract: None,
        signals: vec!["hot".into()],
        owner: None,
        diagram: Default::default(),
    };
    let watch = StateMachineDef {
        name: "Watch".into(),
        inputs: vec![Port { name: "tick".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "hot_seen".into(), ty: Type::Bool }],
        locals: vec![],
        initial_state: "Watching".into(),
        states: vec![StateDef {
            name: "Watching".into(),
            equations: vec![Equation { lhs: vec!["hot_seen".into()], rhs: Expr::var("hot") }],
            transitions: vec![],
            regions: vec![],
            refines: Some("Blinker".into()),
            refine_history: false,
            emits: vec![],
        }],
        contract: None,
        signals: vec![],
        owner: None,
        diagram: Default::default(),
    };
    let mut project = Project {
        name: "sig".into(),
        packages: vec![Package {
            name: "user".into(),
            state_machines: vec![blinker, watch],
            ..Default::default()
        }],
        main: Some("Watch".into()),
        ..Default::default()
    };
    project.lower_state_machines().expect("resolves and lowers");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "Watch").unwrap();
    let mut seen = Vec::new();
    for _ in 0..4 {
        let mut inputs = BTreeMap::new();
        inputs.insert("tick".into(), Value::Bool(true));
        seen.push(sim.step(&inputs).unwrap()["hot_seen"].as_bool().unwrap());
    }
    // The nested machine alternates A/B each tick; `hot` (and so `hot_seen`)
    // is true exactly on the B cycles.
    assert_eq!(seen, vec![false, true, false, true]);
}

// --- Refine with history -------------------------------------------------------

/// `refine … with history`: on re-entry the sub-machine resumes at the
/// sub-state it held on exit, instead of restarting at its initial state.
#[test]
fn refine_with_history_resumes_the_sub_state_held_on_exit() {
    let run = |history: bool| {
        let mut machines = spin_and_refmode();
        machines[1].states[1].refine_history = history;
        let mut project = Project {
            name: "ref".into(),
            packages: vec![Package { name: "user".into(), state_machines: machines, ..Default::default() }],
            main: Some("RefMode".into()),
            ..Default::default()
        };
        project.lower_state_machines().expect("lowers");
        let mut sim = Sim::new(&project, "RefMode").unwrap();
        let seq = [
            (true, false, false),  // Idle, go -> Active
            (false, false, true),  // Active/Lo (level 1), tick -> Hi
            (false, true, false),  // Active/Hi (level 2), stop -> Idle
            (true, false, false),  // Idle, go -> Active
            (false, false, false), // Active again: restart at Lo, or resume Hi?
        ];
        let mut levels = Vec::new();
        for (go, stop, tick) in seq {
            let mut inputs = BTreeMap::new();
            inputs.insert("go".into(), Value::Bool(go));
            inputs.insert("stop".into(), Value::Bool(stop));
            inputs.insert("tick".into(), Value::Bool(tick));
            levels.push(sim.step(&inputs).unwrap()["level"].as_int().unwrap());
        }
        levels
    };
    assert_eq!(run(false), vec![0, 1, 2, 0, 1], "no history: restart at Lo");
    assert_eq!(run(true), vec![0, 1, 2, 0, 2], "history: resume at Hi");
}

// --- `last x` (SCADE's previous-cycle value) -----------------------------------

/// A counter whose states both read `last n` — the whole point of `last`:
/// the previous cycle's value crosses state boundaries without manual
/// `pre` plumbing (each state's equation would otherwise need its own
/// arrow-init).
fn last_counter() -> StateMachineDef {
    let st = |name: &str, body: &str, target: &str| StateDef {
        name: name.into(),
        equations: vec![Equation {
            lhs: vec!["n".into()],
            rhs: ol_stdlib::parse_expr(body).unwrap(),
        }],
        transitions: vec![Transition {
            guard: Expr::var("flip"),
            target: target.into(),
        }],
        regions: vec![],
        refines: None,
        refine_history: false,
        emits: vec![],
    };
    StateMachineDef {
        name: "Count".into(),
        inputs: vec![Port { name: "flip".into(), ty: Type::Bool }],
        outputs: vec![Port { name: "n".into(), ty: Type::Int32 }],
        locals: vec![],
        initial_state: "Up".into(),
        states: vec![st("Up", "last n + 1", "Down"), st("Down", "last n - 1", "Up")],
        contract: None,
        signals: vec![],
        owner: None,
        diagram: Default::default(),
    }
}

#[test]
fn last_reads_the_previous_cycle_across_states() {
    // Parse + format round-trip first.
    let e = ol_stdlib::parse_expr("last n + 1").unwrap();
    assert_eq!(ol_lustre_emit::format_expr(&e), "last n + 1");

    let mut project = Project {
        name: "last".into(),
        packages: vec![Package {
            name: "user".into(),
            state_machines: vec![last_counter()],
            ..Default::default()
        }],
        main: Some("Count".into()),
        ..Default::default()
    };
    project.lower_state_machines().expect("last resolves and lowers");
    assert!(
        !ol_typecheck::check_project(&project).has_errors(),
        "{:?}",
        ol_typecheck::check_project(&project).errors().map(|d| d.render()).collect::<Vec<_>>()
    );

    let mut sim = Sim::new(&project, "Count").unwrap();
    let mut ns = Vec::new();
    for flip in [false, false, true, false, false] {
        let mut inputs = BTreeMap::new();
        inputs.insert("flip".into(), Value::Bool(flip));
        ns.push(sim.step(&inputs).unwrap()["n"].as_int().unwrap());
    }
    // Up counts from the int32 default (0): 1, 2, 3; the flip lands us in
    // Down NEXT cycle, which counts back down off the held value.
    assert_eq!(ns, vec![1, 2, 3, 2, 1]);
}

#[test]
fn last_of_an_unknown_variable_is_rejected_at_lowering() {
    let mut bad = last_counter();
    bad.states[0].equations[0].rhs = ol_stdlib::parse_expr("last zz + 1").unwrap();
    let mut project = Project {
        name: "last".into(),
        packages: vec![Package { name: "user".into(), state_machines: vec![bad], ..Default::default() }],
        ..Default::default()
    };
    let errs = project.lower_state_machines().unwrap_err();
    assert!(
        matches!(errs[0], ol_ir::state_machine::LowerError::UnknownLastVar(_, _)),
        "{errs:?}"
    );
}

#[test]
fn last_outside_a_state_machine_is_a_type_error() {
    // In a plain dataflow equation `last` has no meaning — E0180, not a
    // silent misread.
    let node = ol_ir::NodeDef {
        name: "Plain".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "x".into(), ty: Type::Int32 }],
        outputs: vec![Port { name: "y".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: ol_stdlib::parse_expr("last x").unwrap(),
        }],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
        requirements: vec![],
        sysml: None,
        generics: vec![],
    };
    let project = Project {
        name: "last".into(),
        packages: vec![Package { name: "user".into(), nodes: vec![node], ..Default::default() }],
        ..Default::default()
    };
    let r = ol_typecheck::check_project(&project);
    assert!(
        r.diagnostics.iter().any(|d| d.code == "E0180"),
        "expected E0180, got {:?}",
        r.diagnostics
    );
}
