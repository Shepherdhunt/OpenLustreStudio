//! Float intrinsics (`sqrt`, `sin`, `atan2`, `abs`, `min`, `max`, …) as
//! first-class IR operators: parse/format round-trip, float64-only
//! typechecking, f64 simulation matching C's `<math.h>` double family, the
//! Kind 2 function-call view, and the IR-vs-compiled-C equivalence run.

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

// --- Parse + formatter round-trip ---------------------------------------------

#[test]
fn intrinsics_parse_and_round_trip_through_the_surface_formatter() {
    let e = ol_stdlib::parse_expr("sqrt(x + 1.0)").expect("parse sqrt");
    assert!(
        matches!(&e, ol_ir::Expr::FloatIntrinsic { op: ol_ir::FloatOp::Sqrt, .. }),
        "{e:?}"
    );
    let text = ol_lustre_emit::format_expr(&e);
    assert_eq!(text, "sqrt(x + 1.0)");
    assert_eq!(e, ol_stdlib::parse_expr(&text).unwrap(), "must round-trip");

    // Two-argument intrinsics keep their argument order.
    let a2 = ol_stdlib::parse_expr("atan2(y, x)").unwrap();
    assert!(
        matches!(&a2, ol_ir::Expr::FloatIntrinsic { op: ol_ir::FloatOp::Atan2, args, .. } if args.len() == 2),
        "{a2:?}"
    );
    assert_eq!(ol_lustre_emit::format_expr(&a2), "atan2(y, x)");

    // The Kind 2 view prints the same function-call text (the bit_and
    // convention: the user supplies matching Lustre functions when proving).
    assert_eq!(ol_lustre_emit::format_expr_lustre(&a2), "atan2(y, x)");
    let m = ol_stdlib::parse_expr("min(a, max(b, c))").unwrap();
    assert_eq!(ol_lustre_emit::format_expr_lustre(&m), "min(a, max(b, c))");

    // Arity is enforced at parse time, loudly.
    assert!(ol_stdlib::parse_expr("sqrt(a, b)").is_err());
    assert!(ol_stdlib::parse_expr("pow(a)").is_err());

    // The names are only reserved in call position: as variables they parse.
    let v = ol_stdlib::parse_expr("abs + sin").unwrap();
    assert!(matches!(&v, ol_ir::Expr::Binary { .. }), "{v:?}");
}

// --- Typecheck: float64-only, explicit casts ----------------------------------

fn one_eq_project(out_ty: serde_json::Value, rhs: serde_json::Value) -> ol_ir::Project {
    serde_json::from_value(serde_json::json!({
        "name": "fi",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "N",
                "kind": "Function",
                "inputs": [
                    {"name": "xi", "ty": {"kind": "Int32"}},
                    {"name": "xf", "ty": {"kind": "Float32"}},
                    {"name": "xd", "ty": {"kind": "Float64"}}
                ],
                "outputs": [{"name": "y", "ty": out_ty}],
                "equations": [{"lhs": ["y"], "rhs": rhs}]
            }]
        }]
    }))
    .unwrap()
}

fn intrinsic(op: &str, args: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({"expr": "FloatIntrinsic", "op": op, "args": args})
}

fn var(n: &str) -> serde_json::Value {
    serde_json::json!({"expr": "Var", "name": n})
}

#[test]
fn intrinsics_typecheck_as_float64_and_reject_other_operands() {
    // sqrt(xd) : float64 — clean.
    let p = one_eq_project(
        serde_json::json!({"kind": "Float64"}),
        intrinsic("Sqrt", vec![var("xd")]),
    );
    let report = ol_typecheck::check_project(&p);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    // An int operand is E0161 with the explicit-cast hint.
    let p = one_eq_project(
        serde_json::json!({"kind": "Float64"}),
        intrinsic("Sqrt", vec![var("xi")]),
    );
    let report = ol_typecheck::check_project(&p);
    let d = report
        .diagnostics
        .iter()
        .find(|d| d.code == "E0161")
        .expect("int operand must be E0161");
    assert!(d.message.contains("float64("), "hint missing: {}", d.message);

    // float32 needs the explicit cast too — no implicit widening.
    let p = one_eq_project(
        serde_json::json!({"kind": "Float64"}),
        intrinsic("Sin", vec![var("xf")]),
    );
    let report = ol_typecheck::check_project(&p);
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0161"),
        "{:?}",
        report.diagnostics
    );

    // Casting in fixes it: sin(float64(xf)) is clean.
    let p = one_eq_project(
        serde_json::json!({"kind": "Float64"}),
        intrinsic(
            "Sin",
            vec![serde_json::json!({"expr": "Cast", "to": {"kind": "Float64"}, "arg": var("xf")})],
        ),
    );
    let report = ol_typecheck::check_project(&p);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    // Wrong arity (IR built programmatically) is E0160.
    let p = one_eq_project(
        serde_json::json!({"kind": "Float64"}),
        intrinsic("Pow", vec![var("xd")]),
    );
    let report = ol_typecheck::check_project(&p);
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0160"),
        "{:?}",
        report.diagnostics
    );
}

// --- Simulation: exact values for the exactly-rounded family -------------------

fn math_model() -> serde_json::Value {
    // Every output is exactly representable, so the IR trace, the C `%g`
    // trace, and these assertions all print identical text.
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    serde_json::json!({
        "name": "mathy",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Mathy",
                "kind": "Function",
                "inputs": [
                    {"name": "a", "ty": {"kind": "Float64"}},
                    {"name": "b", "ty": {"kind": "Float64"}}
                ],
                "outputs": [
                    {"name": "root", "ty": {"kind": "Float64"}},
                    {"name": "mag", "ty": {"kind": "Float64"}},
                    {"name": "lo", "ty": {"kind": "Float64"}},
                    {"name": "hi", "ty": {"kind": "Float64"}},
                    {"name": "fl", "ty": {"kind": "Float64"}},
                    {"name": "ce", "ty": {"kind": "Float64"}},
                    {"name": "rd", "ty": {"kind": "Float64"}},
                    {"name": "pw", "ty": {"kind": "Float64"}}
                ],
                "equations": [
                    eq("root", "sqrt(a)"),
                    eq("mag", "abs(b)"),
                    eq("lo", "min(a, b)"),
                    eq("hi", "max(a, b)"),
                    eq("fl", "floor(b)"),
                    eq("ce", "ceil(b)"),
                    eq("rd", "round(b)"),
                    eq("pw", "pow(a, 2.0)")
                ]
            }]
        }],
        "main": "Mathy"
    })
}

#[test]
fn intrinsics_simulate_with_math_h_semantics() {
    let tmp = make_tempdir("fi_sim");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&math_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();
    let mut sim = ol_sim::Sim::new(&project, "Mathy").unwrap();
    let trace = sim.run_csv("a,b\n6.25,-3.5\n4,2.5\n").unwrap();
    let csv = trace.to_csv();
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines[0], "cycle,root,mag,lo,hi,fl,ce,rd,pw");
    // sqrt(6.25)=2.5, abs(-3.5)=3.5, min/max, floor(-3.5)=-4, ceil=-3,
    // round(-3.5)=-4 (half away from zero, like C), pow(6.25,2)=39.0625.
    assert_eq!(lines[1], "0,2.5,3.5,-3.5,6.25,-4,-3,-4,39.0625");
    // round(2.5)=3 — half away from zero again.
    assert_eq!(lines[2], "1,2,2.5,2.5,4,2,3,3,16");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn transcendental_intrinsics_evaluate_via_f64_libm() {
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    let project: ol_ir::Project = serde_json::from_value(serde_json::json!({
        "name": "trig",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Trig",
                "kind": "Function",
                "inputs": [{"name": "x", "ty": {"kind": "Float64"}}],
                "outputs": [
                    {"name": "s", "ty": {"kind": "Float64"}},
                    {"name": "c", "ty": {"kind": "Float64"}},
                    {"name": "e", "ty": {"kind": "Float64"}},
                    {"name": "l", "ty": {"kind": "Float64"}},
                    {"name": "t", "ty": {"kind": "Float64"}}
                ],
                "equations": [
                    eq("s", "sin(x)"),
                    eq("c", "cos(x)"),
                    eq("e", "exp(x)"),
                    eq("l", "log10(x)"),
                    eq("t", "atan2(x, 1.0)")
                ]
            }]
        }],
        "main": "Trig"
    }))
    .unwrap();
    let mut sim = ol_sim::Sim::new(&project, "Trig").unwrap();
    let trace = sim.run_csv("x\n1\n").unwrap();
    let csv = trace.to_csv();
    let cells: Vec<f64> = csv
        .trim()
        .lines()
        .nth(1)
        .unwrap()
        .split(',')
        .skip(1)
        .map(|s| s.parse().unwrap())
        .collect();
    assert!((cells[0] - 1f64.sin()).abs() < 1e-15);
    assert!((cells[1] - 1f64.cos()).abs() < 1e-15);
    assert!((cells[2] - 1f64.exp()).abs() < 1e-15);
    assert!((cells[3] - 0.0).abs() < 1e-15, "log10(1) = 0");
    assert!((cells[4] - 1f64.atan2(1.0)).abs() < 1e-15);
}

// --- Generated C: <math.h> calls, and the dual-backend equivalence run --------

#[test]
fn generated_c_calls_math_h_and_traces_match_compiled_c() {
    let tmp = make_tempdir("fi_c");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&math_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();

    let emitted = ol_clite_emit::emit_project(&project);
    assert!(emitted.header.contains("#include <math.h>"), "{}", emitted.header);
    for call in ["sqrt(", "fabs(", "fmin(", "fmax(", "floor(", "ceil(", "round(", "pow("] {
        assert!(emitted.source.contains(call), "generated C missing {call}:\n{}", emitted.source);
    }

    // Exactly-representable cases: the IR trace and the compiled C's %g trace
    // are compared cell-by-cell as text by the scenario harness.
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(
        scen.join("mathy.csv"),
        "a,b\n6.25,-3.5\n4,2.5\n0.25,0.5\n100,-0.5\n",
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
    assert!(out.contains("[PASS] mathy (ir)"), "{out}");
    assert!(out.contains("[PASS] mathy (c )"), "intrinsics C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

// --- Single-precision (float32) variants ---------------------------------------

#[test]
fn single_precision_intrinsics_parse_typecheck_and_round_trip() {
    let e = ol_stdlib::parse_expr("sqrtf(x)").expect("parse sqrtf");
    assert!(
        matches!(&e, ol_ir::Expr::FloatIntrinsic { op: ol_ir::FloatOp::Sqrt, single: true, .. }),
        "{e:?}"
    );
    assert_eq!(ol_lustre_emit::format_expr(&e), "sqrtf(x)");
    assert_eq!(e, ol_stdlib::parse_expr("sqrtf(x)").unwrap());
    assert_eq!(ol_lustre_emit::format_expr_lustre(&e), "sqrtf(x)");

    // sqrtf takes float32; a float64 operand is E0161 with the float32 hint.
    let p = one_eq_project(
        serde_json::json!({"kind": "Float32"}),
        serde_json::json!({"expr": "FloatIntrinsic", "op": "Sqrt", "single": true,
                           "args": [var("xf")]}),
    );
    let report = ol_typecheck::check_project(&p);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);

    let p = one_eq_project(
        serde_json::json!({"kind": "Float32"}),
        serde_json::json!({"expr": "FloatIntrinsic", "op": "Sqrt", "single": true,
                           "args": [var("xd")]}),
    );
    let report = ol_typecheck::check_project(&p);
    let d = report.diagnostics.iter().find(|d| d.code == "E0161").expect("E0161");
    assert!(d.message.contains("float32("), "hint missing: {}", d.message);

    // The `single` flag round-trips through JSON; absent means double, so
    // pre-existing models load unchanged.
    let txt = serde_json::to_string(&e).unwrap();
    assert!(txt.contains("\"single\":true"), "{txt}");
    let d64: ol_ir::Expr = serde_json::from_str(
        r#"{"expr":"FloatIntrinsic","op":"Sqrt","args":[{"expr":"Var","name":"x"}]}"#,
    )
    .unwrap();
    assert!(matches!(&d64, ol_ir::Expr::FloatIntrinsic { single: false, .. }));
}

fn mathf_model() -> serde_json::Value {
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    serde_json::json!({
        "name": "mathf",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Mathf",
                "kind": "Function",
                "inputs": [
                    {"name": "a", "ty": {"kind": "Float32"}},
                    {"name": "b", "ty": {"kind": "Float32"}}
                ],
                "outputs": [
                    {"name": "root", "ty": {"kind": "Float32"}},
                    {"name": "mag", "ty": {"kind": "Float32"}},
                    {"name": "lo", "ty": {"kind": "Float32"}},
                    {"name": "rd", "ty": {"kind": "Float32"}}
                ],
                "equations": [
                    eq("root", "sqrtf(a)"),
                    eq("mag", "absf(b)"),
                    eq("lo", "minf(a, b)"),
                    eq("rd", "roundf(b)")
                ]
            }]
        }],
        "main": "Mathf"
    })
}

#[test]
fn single_precision_traces_match_between_ir_and_compiled_c() {
    let tmp = make_tempdir("fi_f32");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&mathf_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();

    // The generated C calls the float functions, not the double ones.
    let emitted = ol_clite_emit::emit_project(&project);
    for call in ["sqrtf(", "fabsf(", "fminf(", "roundf("] {
        assert!(emitted.source.contains(call), "generated C missing {call}:\n{}", emitted.source);
    }

    // IR simulation computes in f32: sqrtf(6.25)=2.5, absf(-3.5)=3.5, …
    let mut sim = ol_sim::Sim::new(&project, "Mathf").unwrap();
    let trace = sim.run_csv("a,b\n6.25,-3.5\n").unwrap();
    let lines: Vec<&str> = trace.to_csv().trim().lines().map(str::to_owned)
        .collect::<Vec<_>>().leak().iter().map(|s| s.as_str()).collect();
    assert_eq!(lines[1], "0,2.5,3.5,-3.5,-4");

    // And the compiled C agrees cell by cell on exactly-rounded cases.
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("mathf.csv"), "a,b\n6.25,-3.5\n4,2.5\n0.25,0.5\n").unwrap();
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
    assert!(out.contains("[PASS] mathf (ir)"), "{out}");
    assert!(out.contains("[PASS] mathf (c )"), "float32 C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
