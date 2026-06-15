//! MC/DC (Modified Condition/Decision Coverage), the DO-178C Level A metric,
//! end to end: the unique-cause independence analysis, the simulator's
//! per-condition trial capture, and the suite-level report through the CLI.
//!
//! The claim MC/DC makes: every condition in a decision has been shown to
//! independently flip the decision's outcome. `a and b` is fully covered by
//! the three vectors {FT, TT, TF} and not by {FF, TT}.

use std::path::PathBuf;
use std::process::Command;

use ol_sim::{mcdc_independence, McdcTrial};

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

fn trial(values: &[bool], outcome: bool) -> McdcTrial {
    McdcTrial { values: values.to_vec(), outcome }
}

// --- The pure analysis --------------------------------------------------------

#[test]
fn and_is_covered_by_three_vectors_not_two() {
    // a and b: {FT->F, TT->T, TF->F} demonstrates both conditions.
    let full = [trial(&[false, true], false), trial(&[true, true], true), trial(&[true, false], false)];
    let indep = mcdc_independence(2, &full);
    assert!(indep[0].is_some(), "a independence: {indep:?}");
    assert!(indep[1].is_some(), "b independence: {indep:?}");

    // {FF->F, TT->T} flips both conditions at once: neither is isolated.
    let weak = [trial(&[false, false], false), trial(&[true, true], true)];
    let indep = mcdc_independence(2, &weak);
    assert!(indep[0].is_none() && indep[1].is_none(), "neither isolated: {indep:?}");
}

#[test]
fn or_and_three_input_decisions() {
    // a or b: {FF->F, TF->T, FT->T} covers both.
    let or = [trial(&[false, false], false), trial(&[true, false], true), trial(&[false, true], true)];
    let indep = mcdc_independence(2, &or);
    assert!(indep.iter().all(|p| p.is_some()), "or fully covered: {indep:?}");

    // a and b and c needs n+1 = 4 well-chosen vectors. TTT plus each single
    // flip isolates each condition.
    let abc = [
        trial(&[true, true, true], true),
        trial(&[false, true, true], false),
        trial(&[true, false, true], false),
        trial(&[true, true, false], false),
    ];
    let indep = mcdc_independence(3, &abc);
    assert!(indep.iter().all(|p| p.is_some()), "3-input and covered: {indep:?}");

    // Drop the c-flip: c is no longer independent, a and b still are.
    let indep = mcdc_independence(3, &abc[..3]);
    assert!(indep[0].is_some() && indep[1].is_some() && indep[2].is_none(),
        "c should be uncovered: {indep:?}");
}

// --- Simulator capture --------------------------------------------------------

fn load(model: serde_json::Value) -> ol_ir::Project {
    serde_json::from_value(model).expect("model deserializes")
}

fn guard_model() -> serde_json::Value {
    // node Guard(a, b) returns (open)  open = a and b;  (an equation decision)
    serde_json::json!({
        "name": "mcdc", "packages": [{"name": "user", "nodes": [{
            "name": "Guard", "kind": "Operator",
            "inputs": [{"name": "a", "ty": {"kind": "Bool"}}, {"name": "b", "ty": {"kind": "Bool"}}],
            "outputs": [{"name": "open", "ty": {"kind": "Bool"}}],
            "equations": [{"lhs": ["open"], "rhs": {"expr": "Binary", "op": "And",
                "lhs": {"expr": "Var", "name": "a"}, "rhs": {"expr": "Var", "name": "b"}}}]
        }]}],
        "main": "Guard"
    })
}

#[test]
fn simulator_captures_conditions_and_outcome_per_cycle() {
    let project = load(guard_model());
    let mut sim = ol_sim::Sim::new(&project, "Guard").unwrap();
    sim.enable_coverage();
    // Three cycles: FT, TT, TF — the MC/DC-complete suite for `a and b`.
    sim.run_csv_full("a,b\nfalse,true\ntrue,true\ntrue,false\n").unwrap();

    let decisions = sim.mcdc_decisions().expect("coverage enabled");
    let d = decisions.iter().find(|d| d.decision == "a and b").expect("the and decision");
    assert_eq!(d.conditions, vec!["a", "b"]);
    assert_eq!(d.context, "open");
    // Distinct trials captured (order independent).
    assert_eq!(d.trials.len(), 3);
    let indep = mcdc_independence(d.conditions.len(), &d.trials);
    assert!(indep.iter().all(|p| p.is_some()), "fully covered: {indep:?}");

    // A weaker suite leaves both conditions un-isolated.
    let mut sim = ol_sim::Sim::new(&project, "Guard").unwrap();
    sim.enable_coverage();
    sim.run_csv_full("a,b\nfalse,false\ntrue,true\n").unwrap();
    let decisions = sim.mcdc_decisions().unwrap();
    let d = decisions.iter().find(|d| d.decision == "a and b").unwrap();
    let indep = mcdc_independence(d.conditions.len(), &d.trials);
    assert!(indep.iter().all(|p| p.is_none()), "neither isolated: {indep:?}");
}

#[test]
fn if_conditions_are_decisions_too() {
    // z = if a and b then 1 else 0 — the canonical control decision.
    let project = load(serde_json::json!({
        "name": "mcdc_if", "packages": [{"name": "user", "nodes": [{
            "name": "Sel", "kind": "Operator",
            "inputs": [{"name": "a", "ty": {"kind": "Bool"}}, {"name": "b", "ty": {"kind": "Bool"}}],
            "outputs": [{"name": "z", "ty": {"kind": "Int32"}}],
            "equations": [{"lhs": ["z"], "rhs": {"expr": "IfThenElse",
                "cond": {"expr": "Binary", "op": "And",
                    "lhs": {"expr": "Var", "name": "a"}, "rhs": {"expr": "Var", "name": "b"}},
                "then_branch": {"expr": "Const", "lit": {"lit": "Int", "value": 1}},
                "else_branch": {"expr": "Const", "lit": {"lit": "Int", "value": 0}}}}]
        }]}],
        "main": "Sel"
    }));
    let mut sim = ol_sim::Sim::new(&project, "Sel").unwrap();
    sim.enable_coverage();
    sim.run_csv_full("a,b\nfalse,true\ntrue,true\ntrue,false\n").unwrap();
    let decisions = sim.mcdc_decisions().unwrap();
    let d = decisions.iter().find(|d| d.decision == "a and b").expect("if-cond decision");
    let indep = mcdc_independence(d.conditions.len(), &d.trials);
    assert!(indep.iter().all(|p| p.is_some()), "if-cond MC/DC covered: {indep:?}");
}

// --- End to end through the CLI report ----------------------------------------

fn openlustre(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--"])
        .args(args)
        .output()
        .expect("cargo run");
    (
        out.status.success(),
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
    )
}

#[test]
fn cli_reports_mcdc_coverage_for_the_suite() {
    let tmp = make_tempdir("mcdc_cli");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&guard_model()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();

    // A suite that achieves MC/DC on `a and b`.
    std::fs::write(scen.join("full.csv"), "a,b\nfalse,true\ntrue,true\ntrue,false\n").unwrap();
    let (ok, _) = openlustre(&["test", "record", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap()]);
    assert!(ok);
    let (ok, out) = openlustre(&["test", "run", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "ir"]);
    assert!(ok, "{out}");
    assert!(out.contains("MC/DC: 2/2 conditions independent"), "{out}");

    // Replace with a suite that cannot isolate either condition.
    std::fs::remove_file(scen.join("full.csv")).unwrap();
    std::fs::remove_file(scen.join("full.csv.golden")).ok();
    for f in std::fs::read_dir(&scen).unwrap() {
        std::fs::remove_file(f.unwrap().path()).ok();
    }
    std::fs::write(scen.join("weak.csv"), "a,b\nfalse,false\ntrue,true\n").unwrap();
    let (ok, _) = openlustre(&["test", "record", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap()]);
    assert!(ok);
    let (ok, out) = openlustre(&["test", "run", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "ir"]);
    assert!(ok, "{out}");
    assert!(out.contains("MC/DC: 0/2 conditions independent"), "{out}");
    assert!(out.contains("uncovered:") && out.contains("`a`"), "names the uncovered condition: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
