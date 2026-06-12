//! Equations are declarative: their declaration order must not matter.
//! These tests pin the dependency-ordered evaluation fix — previously both
//! the IR simulator and the generated C walked equations in declaration
//! order, silently reading stale defaults for forward references (the exact
//! shape the canvas produces when a constant block is dropped after the
//! equation that uses it).

use std::path::PathBuf;
use std::process::Command;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// The drawn-canvas counter: the constant feeding the increment is declared
/// AFTER the equation that reads it.
fn counter_model() -> serde_json::Value {
    serde_json::json!({
        "name": "counter",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Counter",
                "kind": "Operator",
                "inputs": [{"name": "tick", "ty": {"kind": "Bool"}}],
                "outputs": [{"name": "n", "ty": {"kind": "Int32"}}],
                "locals": [{"name": "constant1", "ty": {"kind": "Int32"}}],
                "equations": [
                    {"lhs": ["n"],
                     "rhs": {"expr": "Binary", "op": "Add",
                        "lhs": {"expr": "Arrow",
                            "init": {"expr": "Const", "lit": {"lit": "Int", "value": 0}},
                            "body": {"expr": "Pre", "arg": {"expr": "Var", "name": "n"}}},
                        "rhs": {"expr": "IfThenElse",
                            "cond": {"expr": "Var", "name": "tick"},
                            "then_branch": {"expr": "Var", "name": "constant1"},
                            "else_branch": {"expr": "Const", "lit": {"lit": "Int", "value": 0}}}}},
                    {"lhs": ["constant1"],
                     "rhs": {"expr": "Const", "lit": {"lit": "Int", "value": 1}}}
                ]
            }]
        }],
        "main": "Counter"
    })
}

#[test]
fn forward_referenced_local_sees_this_cycles_value() {
    let tmp = make_tempdir("fwd_sim");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&counter_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();
    let mut sim = ol_sim::Sim::new(&project, "Counter").unwrap();
    let trace = sim.run_csv("tick\ntrue\ntrue\nfalse\ntrue\n").unwrap();
    let csv = trace.to_csv();
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines[1], "0,1", "cycle 0 must already see constant1 = 1");
    assert_eq!(lines[2], "1,2");
    assert_eq!(lines[3], "2,2", "tick=false holds the count");
    assert_eq!(lines[4], "3,3");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn forward_reference_matches_compiled_c() {
    let tmp = make_tempdir("fwd_c");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&counter_model()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("ticks.csv"), "tick\ntrue\ntrue\nfalse\ntrue\ntrue\n").unwrap();

    let run = |args: &[&str]| -> (bool, String) {
        let out = Command::new(env!("CARGO"))
            .args(["run", "-q", "-p", "ol_cli", "--"])
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    let (ok, out) = run(&["test", "record", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap()]);
    assert!(ok, "record: {out}");
    // The recorded golden itself must show the counter counting (the old
    // declaration-order bug recorded all-zero traces that C then "matched").
    // Full-trace columns: cycle,tick,constant1,n.
    let golden = std::fs::read_to_string(scen.join("ticks.golden.csv")).unwrap();
    assert!(golden.contains("0,true,1,1"), "golden must count from 1: {golden}");
    assert!(golden.contains("4,true,1,4"), "golden must keep counting: {golden}");
    let (ok, out) = run(&["test", "run", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "both"]);
    assert!(ok, "run: {out}");
    assert!(out.contains("[PASS] ticks (ir)"), "{out}");
    assert!(out.contains("[PASS] ticks (c )"), "{out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn arrow_body_forward_reference_is_ordered_too() {
    // `x = 0 -> y` reads y's CURRENT value from cycle 1 on, so y's equation
    // must run first even though it is declared second.
    let project: ol_ir::Project = serde_json::from_value(serde_json::json!({
        "name": "arrow",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "A",
                "kind": "Operator",
                "inputs": [{"name": "i", "ty": {"kind": "Int32"}}],
                "outputs": [{"name": "x", "ty": {"kind": "Int32"}}],
                "locals": [{"name": "y", "ty": {"kind": "Int32"}}],
                "equations": [
                    {"lhs": ["x"], "rhs": {"expr": "Arrow",
                        "init": {"expr": "Const", "lit": {"lit": "Int", "value": 0}},
                        "body": {"expr": "Var", "name": "y"}}},
                    {"lhs": ["y"], "rhs": {"expr": "Binary", "op": "Mul",
                        "lhs": {"expr": "Var", "name": "i"},
                        "rhs": {"expr": "Const", "lit": {"lit": "Int", "value": 2}}}}
                ]
            }]
        }],
        "main": "A"
    })).unwrap();
    let mut sim = ol_sim::Sim::new(&project, "A").unwrap();
    let trace = sim.run_csv("i\n5\n7\n").unwrap();
    let lines: Vec<String> = trace.to_csv().trim().lines().map(String::from).collect();
    assert_eq!(lines[1], "0,0");
    assert_eq!(lines[2], "1,14", "cycle 1 must see y = i * 2 of THIS cycle");
}

#[test]
fn combinational_cycle_is_a_loud_error_not_a_wrong_answer() {
    let project: ol_ir::Project = serde_json::from_value(serde_json::json!({
        "name": "cyc",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Cyc",
                "kind": "Operator",
                "inputs": [],
                "outputs": [{"name": "a", "ty": {"kind": "Int32"}}],
                "locals": [{"name": "b", "ty": {"kind": "Int32"}}],
                "equations": [
                    {"lhs": ["a"], "rhs": {"expr": "Var", "name": "b"}},
                    {"lhs": ["b"], "rhs": {"expr": "Var", "name": "a"}}
                ]
            }]
        }]
    })).unwrap();
    let err = match ol_sim::Sim::new(&project, "Cyc") {
        Err(e) => format!("{e}"),
        Ok(_) => panic!("combinational cycle must refuse to simulate"),
    };
    assert!(err.contains("combinational cycle"), "got: {err}");
}
