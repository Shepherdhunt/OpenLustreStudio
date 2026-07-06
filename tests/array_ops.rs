//! `concat(a, b)` / `reverse(a)` — array structure operators: parse/format,
//! typecheck (E0146 whole-rhs, E0148 shapes), simulation, generated C loops,
//! and the IR-vs-compiled-C equivalence run.

use std::path::PathBuf;
use std::process::Command;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn arr_ty(len: u32) -> serde_json::Value {
    serde_json::json!({"kind": "Array", "elem": {"kind": "Int32"}, "len": len})
}

fn joiner_model() -> serde_json::Value {
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    serde_json::json!({
        "name": "ao",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Joiner",
                "kind": "Function",
                "inputs": [
                    {"name": "a", "ty": arr_ty(2)},
                    {"name": "b", "ty": arr_ty(3)}
                ],
                "outputs": [
                    {"name": "joined", "ty": arr_ty(5)},
                    {"name": "flipped", "ty": arr_ty(3)}
                ],
                "equations": [
                    eq("joined", "concat(a, b)"),
                    eq("flipped", "reverse(b)")
                ]
            }]
        }],
        "main": "Joiner"
    })
}

#[test]
fn array_ops_parse_typecheck_and_reject_misuse() {
    let e = ol_stdlib::parse_expr("concat(a, b)").expect("parse concat");
    assert!(matches!(&e, ol_ir::Expr::ArrayOp { op: ol_ir::ArrayOpKind::Concat, .. }), "{e:?}");
    assert_eq!(ol_lustre_emit::format_expr(&e), "concat(a, b)");
    assert_eq!(e, ol_stdlib::parse_expr("concat(a, b)").unwrap());
    assert!(ol_stdlib::parse_expr("concat(a)").is_err());
    assert!(ol_stdlib::parse_expr("reverse(a, b)").is_err());

    // Well-shaped model is clean.
    let p: ol_ir::Project = serde_json::from_value(joiner_model()).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);

    // Mismatched element types are E0148; nesting is E0146; a non-array
    // operand is E0148; a wrong declared length is the usual E0040.
    let has = |mutate: fn(&mut serde_json::Value), code: &str| {
        let mut m = joiner_model();
        mutate(&mut m);
        let p: ol_ir::Project = serde_json::from_value(m).unwrap();
        let r = ol_typecheck::check_project(&p);
        assert!(
            r.diagnostics.iter().any(|d| d.code == code),
            "expected {code}, got {:?}",
            r.diagnostics
        );
    };
    has(|m| {
        m["packages"][0]["nodes"][0]["inputs"][1]["ty"] =
            serde_json::json!({"kind": "Array", "elem": {"kind": "Bool"}, "len": 3});
    }, "E0148");
    has(|m| {
        m["packages"][0]["nodes"][0]["inputs"][1]["ty"] = serde_json::json!({"kind": "Int32"});
    }, "E0148");
    has(|m| {
        m["packages"][0]["nodes"][0]["equations"][0]["rhs"] =
            serde_json::to_value(ol_stdlib::parse_expr("reverse(concat(a, b))").unwrap()).unwrap();
    }, "E0146");
    has(|m| {
        m["packages"][0]["nodes"][0]["outputs"][0]["ty"] =
            serde_json::json!({"kind": "Array", "elem": {"kind": "Int32"}, "len": 4});
    }, "E0040");
}

#[test]
fn array_ops_simulate_and_match_compiled_c() {
    let tmp = make_tempdir("array_ops");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&joiner_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();

    let mut sim = ol_sim::Sim::new(&project, "Joiner").unwrap();
    let trace = sim.run_csv("a,b\n[1;2],[3;4;5]\n").unwrap();
    let lines: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert_eq!(lines[0], "cycle,joined,flipped");
    assert_eq!(lines[1], "0,[1;2;3;4;5],[5;4;3]");

    // The generated C is element loops over fixed bounds.
    let emitted = ol_clite_emit::emit_project(&project);
    assert!(emitted.source.contains("[2 + "), "concat offset loop missing:\n{}", emitted.source);
    assert!(emitted.source.contains("3 - 1 - "), "reverse loop missing:\n{}", emitted.source);

    // Dual-backend equivalence.
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("ao.csv"), "a,b\n[1;2],[3;4;5]\n[-7;0],[9;9;1]\n").unwrap();
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
    assert!(out.contains("[PASS] ao (ir)"), "{out}");
    assert!(out.contains("[PASS] ao (c )"), "array ops C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
