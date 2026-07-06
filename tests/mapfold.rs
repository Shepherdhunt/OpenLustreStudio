//! `mapfold(F, init, a)` — SCADE's combined iterator: F is
//! (accumulator, element) -> (accumulator, element_out); the equation binds
//! `(acc, arr) = mapfold(...)`. Parse/format, typecheck (E0142/E0145/E0147),
//! simulation, generated C, and the IR-vs-compiled-C equivalence run.

use std::path::PathBuf;
use std::process::Command;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn mapfold_parses_and_round_trips() {
    let e = ol_stdlib::parse_expr("mapfold(Step, 0, xs)").expect("parse mapfold");
    assert!(
        matches!(&e, ol_ir::Expr::Iterate { kind: ol_ir::IterKind::MapFold, init: Some(_), .. }),
        "{e:?}"
    );
    let text = ol_lustre_emit::format_expr(&e);
    assert_eq!(text, "mapfold(Step, 0, xs)");
    assert_eq!(e, ol_stdlib::parse_expr(&text).unwrap(), "must round-trip");
    assert!(ol_stdlib::parse_expr("mapfold(Step, xs)").is_err(), "arity is loud");
}

/// Running total: F(acc, x) = (acc + x, acc + x) — the mapped array is the
/// prefix sums and the final accumulator the total.
fn prefix_model() -> serde_json::Value {
    let eq = |lhs: Vec<&str>, body: &str| {
        serde_json::json!({"lhs": lhs, "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    serde_json::json!({
        "name": "mf",
        "packages": [{
            "name": "user",
            "nodes": [
                {
                    "name": "Step",
                    "kind": "Function",
                    "inputs": [
                        {"name": "acc", "ty": {"kind": "Int32"}},
                        {"name": "x", "ty": {"kind": "Int32"}}
                    ],
                    "outputs": [
                        {"name": "acc_out", "ty": {"kind": "Int32"}},
                        {"name": "y", "ty": {"kind": "Int32"}}
                    ],
                    "equations": [
                        eq(vec!["acc_out"], "acc + x"),
                        eq(vec!["y"], "acc + x")
                    ]
                },
                {
                    "name": "Prefix",
                    "kind": "Function",
                    "inputs": [{"name": "xs", "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}}],
                    "outputs": [
                        {"name": "total", "ty": {"kind": "Int32"}},
                        {"name": "sums", "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}}
                    ],
                    "equations": [
                        eq(vec!["total", "sums"], "mapfold(Step, 0, xs)")
                    ]
                }
            ]
        }],
        "main": "Prefix"
    })
}

#[test]
fn mapfold_typechecks_and_rejects_bad_shapes() {
    let p: ol_ir::Project = serde_json::from_value(prefix_model()).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);

    // A one-output F is E0142; a one-name lhs is E0147.
    let mut bad = prefix_model();
    bad["packages"][0]["nodes"][1]["equations"][0]["lhs"] = serde_json::json!(["total"]);
    let p: ol_ir::Project = serde_json::from_value(bad).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.iter().any(|d| d.code == "E0147"), "{:?}", r.diagnostics);

    let mut bad = prefix_model();
    // Swap the iterated function for the single-output shape.
    bad["packages"][0]["nodes"][0]["outputs"] = serde_json::json!([
        {"name": "acc_out", "ty": {"kind": "Int32"}}
    ]);
    bad["packages"][0]["nodes"][0]["equations"] = serde_json::json!([
        {"lhs": ["acc_out"], "rhs": ol_stdlib::parse_expr("acc + x").unwrap()}
    ]);
    let p: ol_ir::Project = serde_json::from_value(bad).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.iter().any(|d| d.code == "E0142"), "{:?}", r.diagnostics);

    // A wrongly-typed second lhs is E0147.
    let mut bad = prefix_model();
    bad["packages"][0]["nodes"][1]["outputs"][1] = serde_json::json!(
        {"name": "sums", "ty": {"kind": "Bool"}}
    );
    let p: ol_ir::Project = serde_json::from_value(bad).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.iter().any(|d| d.code == "E0147"), "{:?}", r.diagnostics);
}

#[test]
fn mapfold_simulates_and_matches_compiled_c() {
    let tmp = make_tempdir("mapfold");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&prefix_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();

    // IR: prefix sums of [1;2;3;4] are [1;3;6;10], total 10.
    let mut sim = ol_sim::Sim::new(&project, "Prefix").unwrap();
    let trace = sim.run_csv("xs\n[1;2;3;4]\n").unwrap();
    let lines: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert_eq!(lines[0], "cycle,total,sums");
    assert_eq!(lines[1], "0,10,[1;3;6;10]");

    // Generated C threads the accumulator and fills the array in one loop.
    let emitted = ol_clite_emit::emit_project(&project);
    assert!(emitted.source.contains("Step_step"), "{}", emitted.source);

    // Dual-backend equivalence.
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("mf.csv"), "xs\n[1;2;3;4]\n[5;0;-2;7]\n").unwrap();
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
    assert!(out.contains("[PASS] mf (ir)"), "{out}");
    assert!(out.contains("[PASS] mf (c )"), "mapfold C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
