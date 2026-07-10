//! `activate(F, cond, default, args…)` — SCADE's operator conditioning:
//! `F` executes only on the cycles where `cond` holds (its internal state
//! FREEZES on the others) and the block yields `default` off-cycles. Sugar
//! over the boolean-clock core (`merge(cond, F(args when cond), default
//! when not cond)`), so every backend inherits the semantics from the
//! clock machinery — which this test pins on both.

use std::path::PathBuf;
use std::process::Command;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn i32_ty() -> serde_json::Value {
    serde_json::json!({"kind": "Int32"})
}

/// `Acc` keeps a running sum — STATE — so the freeze semantics are
/// observable: an input arriving while the activation is off must never
/// enter the sum.
fn gated_model() -> serde_json::Value {
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    serde_json::json!({
        "name": "act",
        "packages": [{
            "name": "user",
            "nodes": [
                {
                    "name": "Acc", "kind": "Operator",
                    "inputs": [{"name": "x", "ty": i32_ty()}],
                    "outputs": [{"name": "s", "ty": i32_ty()}],
                    "equations": [eq("s", "x + (0 -> pre s)")]
                },
                {
                    "name": "Gate", "kind": "Operator",
                    "inputs": [
                        {"name": "c", "ty": {"kind": "Bool"}},
                        {"name": "x", "ty": i32_ty()}
                    ],
                    "outputs": [{"name": "y", "ty": i32_ty()}],
                    "equations": [eq("y", "activate(Acc, c, -1, x)")]
                }
            ]
        }],
        "main": "Gate"
    })
}

#[test]
fn activate_parses_as_clocked_merge_and_rejects_misuse() {
    // The sugar expands to the clocked core, exactly like `fby`.
    let e = ol_stdlib::parse_expr("activate(F, c, 0, a, b)").expect("parse");
    match &e {
        ol_ir::Expr::Merge { clock, on_true, on_false } => {
            assert_eq!(clock, "c");
            match on_true.as_ref() {
                ol_ir::Expr::Call { node, args } => {
                    assert_eq!(node, "F");
                    assert_eq!(args.len(), 2);
                    assert!(args.iter().all(|a| matches!(
                        a,
                        ol_ir::Expr::When { on: true, .. }
                    )));
                }
                other => panic!("on_true: {other:?}"),
            }
            assert!(matches!(on_false.as_ref(), ol_ir::Expr::When { on: false, .. }));
        }
        other => panic!("expected merge, got {other:?}"),
    }
    assert_eq!(
        ol_lustre_emit::format_expr(&e),
        "merge(c, F(a when c, b when c), 0 when not c)"
    );

    // Too few arguments; a non-variable condition (clocks are variables).
    assert!(ol_stdlib::parse_expr("activate(F, c)").is_err());
    assert!(ol_stdlib::parse_expr("activate(F, c and d, 0, a)").is_err());

    // The gated model typechecks (clock calculus included). Lowering runs
    // first — as every pipeline does — which splits the stateful activation
    // into its own-equation form so the activation clock is explicit.
    let mut p: ol_ir::Project = serde_json::from_value(gated_model()).unwrap();
    p.lower_state_machines().unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let gate = p.find_node("Gate").unwrap();
    assert_eq!(gate.equations.len(), 2, "the activation call was hoisted");
    assert!(gate.locals.iter().any(|l| l.name.starts_with("__act")), "{:?}", gate.locals);
}

#[test]
fn activate_freezes_state_off_cycles_in_both_backends() {
    let tmp = make_tempdir("activate");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&gated_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();

    // (c,x): (T,3) (F,9) (T,4) (F,1) (T,5)
    //   on-cycles sum 3, 4, 5 -> 3, 7, 12; the 9 and 1 arriving while the
    //   activation is off must NOT enter the accumulator; off-cycles yield -1.
    let mut sim = ol_sim::Sim::new(&project, "Gate").unwrap();
    let trace = sim
        .run_csv("c,x\ntrue,3\nfalse,9\ntrue,4\nfalse,1\ntrue,5\n")
        .unwrap();
    let lines: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert_eq!(lines[1], "0,3");
    assert_eq!(lines[2], "1,-1");
    assert_eq!(lines[3], "2,7", "x=9 during the off cycle must not enter the sum");
    assert_eq!(lines[4], "3,-1");
    assert_eq!(lines[5], "4,12");

    // Dual-backend equivalence.
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(
        scen.join("act.csv"),
        "c,x\ntrue,3\nfalse,9\ntrue,4\nfalse,1\ntrue,5\nfalse,100\ntrue,-2\n",
    )
    .unwrap();
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
    let (ok, out) = run(&["test", "run", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "both"]);
    assert!(ok, "run: {out}");
    assert!(out.contains("[PASS] act (ir)"), "{out}");
    assert!(out.contains("[PASS] act (c )"), "activate C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
