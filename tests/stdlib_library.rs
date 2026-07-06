//! Loads the real `libraries/` tree shipped in the repository and asserts that
//! every standard-library block lowers to IR and passes type + contract checks.
//! This is the regression guard for the standard library: adding a malformed
//! block, or breaking the textual expression parser, fails this test.

use std::path::PathBuf;

use ol_ir::Severity;

fn libraries_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries")
}

#[test]
fn standard_library_loads_and_checks_clean() {
    let dir = libraries_dir();
    let lib = ol_stdlib::load_dir(&dir).expect("library loads");

    assert!(
        lib.entries.len() >= 20,
        "expected the expanded block library, found only {} blocks",
        lib.entries.len()
    );

    let errors: Vec<String> = lib
        .check()
        .into_iter()
        .filter(|d| matches!(d.severity, Severity::Error))
        .map(|d| d.render())
        .collect();
    assert!(
        errors.is_empty(),
        "standard library has check errors:\n{}",
        errors.join("\n")
    );
}

#[test]
fn expected_blocks_are_present() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("library loads");
    let names: Vec<&str> = lib.nodes().map(|n| n.name.as_str()).collect();
    for expected in [
        "And", "Or", "Not", "Xor", "Mux", "Switch", "Add", "Subtract", "Multiply", "Divide",
        "Min", "Max", "Clamp", "Saturate", "Equal", "Less", "RisingEdge", "FallingEdge", "Latch",
        "Delay", "Counter",
        // The control-law family.
        "RateLimiter", "FirstOrderLag", "Hysteresis", "Debounce", "PIDController",
        // Integer helpers.
        "Abs", "Sign",
    ] {
        assert!(names.contains(&expected), "missing block `{expected}`");
    }
}

/// The control-law blocks behave: simulate each through a thin wrapper
/// project, checking the regulation semantics cycle by cycle.
#[test]
fn control_blocks_simulate_correctly() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("library loads");

    // A project whose main simply instantiates one library block.
    let wrap = |callee: &str, ins: &[(&str, &str)], outs: &[(&str, &str)]| -> ol_ir::Project {
        let mut project: ol_ir::Project = serde_json::from_value(serde_json::json!({
            "name": "wrap",
            "main": "Top",
            "packages": [{
                "name": "user",
                "nodes": [{
                    "name": "Top",
                    "kind": "Operator",
                    "inputs": ins.iter().map(|(n, t)| serde_json::json!({
                        "name": n, "ty": ol_stdlib::parse_type(t).unwrap()
                    })).collect::<Vec<_>>(),
                    "outputs": outs.iter().map(|(n, t)| serde_json::json!({
                        "name": n, "ty": ol_stdlib::parse_type(t).unwrap()
                    })).collect::<Vec<_>>(),
                    "equations": [{
                        "lhs": outs.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                        "rhs": {"expr": "Call", "node": callee,
                                "args": ins.iter().map(|(n, _)| serde_json::json!({
                                    "expr": "Var", "name": n
                                })).collect::<Vec<_>>()}
                    }]
                }]
            }]
        }))
        .unwrap();
        lib.merge_into(&mut project, "stdlib");
        project
    };
    let run = |p: &ol_ir::Project, csv: &str| -> Vec<String> {
        let mut sim = ol_sim::Sim::new(p, "Top").unwrap();
        sim.run_csv(csv).unwrap().to_csv().trim().lines().skip(1).map(str::to_owned).collect()
    };

    // RateLimiter: first cycle passes x, then slews by at most `rate`.
    let p = wrap("RateLimiter", &[("x", "float64"), ("rate", "float64")], &[("y", "float64")]);
    let t = run(&p, "x,rate\n0,1\n10,1\n10,1\n-10,2\n");
    assert_eq!(t, ["0,0", "1,1", "2,2", "3,0"], "{t:?}");

    // FirstOrderLag with alpha=0.5: y = 8, then halves toward 0.
    let p = wrap("FirstOrderLag", &[("x", "float64"), ("alpha", "float64")], &[("y", "float64")]);
    let t = run(&p, "x,alpha\n8,0.5\n0,0.5\n0,0.5\n");
    assert_eq!(t, ["0,8", "1,4", "2,2"], "{t:?}");

    // Hysteresis lo=2 hi=8: off until >= 8, held until <= 2.
    let p = wrap(
        "Hysteresis",
        &[("x", "float64"), ("lo", "float64"), ("hi", "float64")],
        &[("on", "bool")],
    );
    let t = run(&p, "x,lo,hi\n5,2,8\n9,2,8\n5,2,8\n1,2,8\n5,2,8\n");
    assert_eq!(t, ["0,false", "1,true", "2,true", "3,false", "4,false"], "{t:?}");

    // Debounce n=2: a one-cycle glitch never propagates; a held value does.
    let p = wrap("Debounce", &[("x", "bool"), ("n", "int32")], &[("y", "bool")]);
    let t = run(&p, "x,n\ntrue,2\nfalse,2\ntrue,2\ntrue,2\ntrue,2\n");
    assert_eq!(t, ["0,false", "1,false", "2,false", "3,false", "4,true"], "{t:?}");

    // Abs saturates INT_MIN (C's abs(INT_MIN) is UB; ours is loud and defined);
    // Sign is -1/0/1.
    let p = wrap("Abs", &[("x", "int32")], &[("y", "int32")]);
    let t = run(&p, "x\n-7\n7\n0\n-2147483648\n");
    assert_eq!(t, ["0,7", "1,7", "2,0", "3,2147483647"], "{t:?}");
    let p = wrap("Sign", &[("x", "int32")], &[("s", "int32")]);
    let t = run(&p, "x\n-9\n0\n3\n");
    assert_eq!(t, ["0,-1", "1,0", "2,1"], "{t:?}");

    // PID with ki=kd=0 is pure proportional control.
    let p = wrap(
        "PIDController",
        &[("setpoint", "float64"), ("meas", "float64"), ("kp", "float64"),
          ("ki", "float64"), ("kd", "float64")],
        &[("u", "float64")],
    );
    let t = run(&p, "setpoint,meas,kp,ki,kd\n10,4,2,0,0\n10,12,2,0,0\n");
    assert_eq!(t, ["0,12", "1,-4"], "{t:?}");
    // And the integral term accumulates: err=1 each cycle, ki=1 → u = 1, 2, 3.
    let t = run(&p, "setpoint,meas,kp,ki,kd\n1,0,0,1,0\n1,0,0,1,0\n1,0,0,1,0\n");
    assert_eq!(t, ["0,1", "1,2", "2,3"], "{t:?}");
}

#[test]
fn every_node_with_a_contract_links_to_a_real_contract() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("library loads");
    let contract_names: Vec<&str> = lib.contracts().map(|c| c.name.as_str()).collect();
    for node in lib.nodes() {
        if let Some(c) = &node.contract {
            assert!(
                contract_names.contains(&c.as_str()),
                "node `{}` references contract `{}` which was not loaded",
                node.name,
                c
            );
        }
    }
}
