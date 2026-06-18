//! Fixed-point (Q-format) types `sfix<bits>_<frac>` / `ufix<bits>_<frac>`,
//! stored as `round(real·2^frac)` in a backing integer. The parser, type
//! checker, IR simulator and generated C-Lite must agree bit-for-bit: casts
//! (int/float ↔ fixed) rescale, add/sub/compare are integer ops on the stored
//! value, and multiply is `(intN)(((int64_t)a*b) >> frac)`.

use std::path::PathBuf;
use std::process::Command;

use ol_ir::{Equation, Expr, Local, NodeDef, NodeKind, Package, Port, Project, Type};

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn fix(signed: bool, bits: u32, frac: u32) -> Type {
    Type::Fixed { signed, bits, frac }
}

/// A function exercising every fixed-point path: int→fixed casts, add, multiply,
/// fixed→fixed rescale, a fixed compare (in an int-typed `if`), and fixed→int /
/// fixed→float casts back out. Inputs/outputs are int/float so the CSV boundary
/// (and the generated driver) need no fixed I/O. Values stay in range so width
/// narrowing never triggers (the same contract the other dual-backend tests use).
fn fixed_model() -> Project {
    let e = |s: &str| ol_stdlib::parse_expr(s).expect(s);
    let node = NodeDef {
        name: "Fixedy".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port { name: "a".into(), ty: Type::Int32 },
            Port { name: "b".into(), ty: Type::Int32 },
        ],
        outputs: vec![
            Port { name: "osum".into(), ty: Type::Int32 },
            Port { name: "oprod".into(), ty: Type::Int32 },
            Port { name: "owide".into(), ty: Type::Int32 },
            Port { name: "as_real".into(), ty: Type::Float64 },
            Port { name: "pick".into(), ty: Type::Int32 },
        ],
        locals: vec![
            Local { name: "fa".into(), ty: fix(true, 16, 8) },
            Local { name: "fb".into(), ty: fix(true, 16, 8) },
            Local { name: "sum".into(), ty: fix(true, 16, 8) },
            Local { name: "prod".into(), ty: fix(true, 16, 8) },
            Local { name: "wide".into(), ty: fix(true, 32, 16) },
        ],
        equations: vec![
            Equation { lhs: vec!["fa".into()], rhs: e("sfix16_8(a)") },
            Equation { lhs: vec!["fb".into()], rhs: e("sfix16_8(b)") },
            Equation { lhs: vec!["sum".into()], rhs: e("fa + fb") },
            Equation { lhs: vec!["prod".into()], rhs: e("fa * fb") },
            Equation { lhs: vec!["wide".into()], rhs: e("sfix32_16(fa)") },
            Equation { lhs: vec!["osum".into()], rhs: e("int32(sum)") },
            Equation { lhs: vec!["oprod".into()], rhs: e("int32(prod)") },
            Equation { lhs: vec!["owide".into()], rhs: e("int32(wide)") },
            Equation { lhs: vec!["as_real".into()], rhs: e("float64(prod)") },
            Equation { lhs: vec!["pick".into()], rhs: e("if fa > fb then a else b") },
        ],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
        requirements: vec![],
        sysml: None,
        generics: vec![],
    };
    Project {
        name: "fixedpt".into(),
        packages: vec![Package { name: "user".into(), nodes: vec![node], ..Default::default() }],
        main: Some("Fixedy".into()),
        ..Default::default()
    }
}

fn has_error(report: &ol_typecheck::CheckReport) -> bool {
    report.diagnostics.iter().any(|d| d.severity == ol_ir::Severity::Error)
}

// --- Parser + surface formatter round-trip ----------------------------------

#[test]
fn fixed_types_parse_and_round_trip() {
    // Function-style fixed casts, like `int16(x)`.
    let e = ol_stdlib::parse_expr("sfix16_8(x)").expect("parse sfix cast");
    assert!(
        matches!(&e, Expr::Cast { to: Type::Fixed { signed: true, bits: 16, frac: 8 }, .. }),
        "{e:?}"
    );
    assert_eq!(ol_lustre_emit::format_expr(&e), "sfix16_8(x)");
    assert_eq!(ol_stdlib::parse_expr(&ol_lustre_emit::format_expr(&e)).unwrap(), e);
    // The Kind 2 view abstracts a fixed cast to the user-suppliable int_cast.
    assert_eq!(ol_lustre_emit::format_expr_lustre(&e), "int_cast(x)");

    let u = ol_stdlib::parse_expr("ufix32_0(y)").expect("parse ufix cast");
    assert!(
        matches!(&u, Expr::Cast { to: Type::Fixed { signed: false, bits: 32, frac: 0 }, .. }),
        "{u:?}"
    );

    // Type annotations.
    assert_eq!(ol_stdlib::parse_type("sfix16_8").unwrap(), fix(true, 16, 8));
    assert_eq!(ol_stdlib::parse_type("ufix8_4").unwrap(), fix(false, 8, 4));

    // Malformed fixed types are rejected, not silently treated as named types.
    assert!(ol_stdlib::parse_type("sfix12_4").is_err(), "bits must be 8/16/32/64");
    assert!(ol_stdlib::parse_type("sfix16_16").is_err(), "frac must be < bits");
}

// --- IR simulator: exact stored-value semantics ------------------------------

#[test]
fn fixed_point_simulates_with_exact_values() {
    let project = fixed_model();
    let report = ol_typecheck::check_project(&project);
    assert!(!has_error(&report), "typecheck errors: {:?}", report.diagnostics);

    let mut sim = ol_sim::Sim::new(&project, "Fixedy").unwrap();
    let trace = sim.run_csv("a,b\n3,2\n5,-4\n0,7\n-6,-1\n").unwrap();
    let csv = trace.to_csv();
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines[0], "cycle,osum,oprod,owide,as_real,pick");
    // (3,2):  sum=5, prod=6, wide→3, real=6.0, fa>fb ⇒ a=3
    assert_eq!(lines[1], "0,5,6,3,6,3");
    // (5,-4): sum=1, prod=-20, wide→5, real=-20.0, fa>fb ⇒ a=5
    assert_eq!(lines[2], "1,1,-20,5,-20,5");
    // (0,7):  sum=7, prod=0, wide→0, real=0.0, !(fa>fb) ⇒ b=7
    assert_eq!(lines[3], "2,7,0,0,0,7");
    // (-6,-1): sum=-7, prod=6, wide→-6, real=6.0, !(fa>fb) ⇒ b=-1
    assert_eq!(lines[4], "3,-7,6,-6,6,-1");
}

// --- The hard guarantee: IR sim and compiled C agree cell-for-cell -----------

#[test]
fn fixed_point_traces_match_between_ir_and_compiled_c() {
    let tmp = make_tempdir("fixed_c");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&fixed_model()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("sweep.csv"), "a,b\n3,2\n5,-4\n0,7\n-6,-1\n7,7\n-3,4\n").unwrap();

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
    let (ok, out) =
        run(&["test", "record", model.to_str().unwrap(), "--scenarios", scen.to_str().unwrap()]);
    assert!(ok, "record: {out}");
    let (ok, out) = run(&[
        "test",
        "run",
        model.to_str().unwrap(),
        "--scenarios",
        scen.to_str().unwrap(),
        "--backend",
        "both",
    ]);
    assert!(ok, "run: {out}");
    assert!(out.contains("[PASS] sweep (ir)"), "{out}");
    assert!(out.contains("[PASS] sweep (c )"), "fixed-point C backend diverged: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

// --- Type checker rules ------------------------------------------------------

fn project_json(v: serde_json::Value) -> Project {
    serde_json::from_value(v).unwrap()
}

#[test]
fn fixed_division_is_rejected_as_roadmap() {
    let f = || serde_json::json!({"kind": "Fixed", "signed": true, "bits": 16, "frac": 8});
    let report = ol_typecheck::check_project(&project_json(serde_json::json!({
        "name": "d",
        "packages": [{"name": "user", "nodes": [{
            "name": "D", "kind": "Function",
            "inputs": [{"name": "x", "ty": f()}, {"name": "y", "ty": f()}],
            "outputs": [{"name": "q", "ty": f()}],
            "equations": [{"lhs": ["q"], "rhs": {"expr": "Binary", "op": "Div",
                "lhs": {"expr": "Var", "name": "x"}, "rhs": {"expr": "Var", "name": "y"}}}]
        }]}]
    })));
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0088"),
        "expected E0088 (fixed divide roadmap), got: {:?}",
        report.diagnostics
    );
}

#[test]
fn mismatched_fixed_formats_cannot_be_combined() {
    let s16 = || serde_json::json!({"kind": "Fixed", "signed": true, "bits": 16, "frac": 8});
    let s32 = || serde_json::json!({"kind": "Fixed", "signed": true, "bits": 32, "frac": 16});
    let report = ol_typecheck::check_project(&project_json(serde_json::json!({
        "name": "m",
        "packages": [{"name": "user", "nodes": [{
            "name": "M", "kind": "Function",
            "inputs": [{"name": "x", "ty": s16()}, {"name": "y", "ty": s32()}],
            "outputs": [{"name": "q", "ty": s16()}],
            "equations": [{"lhs": ["q"], "rhs": {"expr": "Binary", "op": "Add",
                "lhs": {"expr": "Var", "name": "x"}, "rhs": {"expr": "Var", "name": "y"}}}]
        }]}]
    })));
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0086"),
        "expected E0086 (mismatched fixed arithmetic), got: {:?}",
        report.diagnostics
    );
}

#[test]
fn an_unstorable_fixed_cast_target_is_rejected() {
    // bits=12 is not a storable width — the cast target check (E0095) catches it
    // even when the type is hand-built in JSON, bypassing the parser.
    let bad = || serde_json::json!({"kind": "Fixed", "signed": true, "bits": 12, "frac": 4});
    let report = ol_typecheck::check_project(&project_json(serde_json::json!({
        "name": "b",
        "packages": [{"name": "user", "nodes": [{
            "name": "B", "kind": "Function",
            "inputs": [{"name": "x", "ty": {"kind": "Int32"}}],
            "outputs": [{"name": "q", "ty": bad()}],
            "equations": [{"lhs": ["q"], "rhs": {"expr": "Cast", "to": bad(),
                "arg": {"expr": "Var", "name": "x"}}}]
        }]}]
    })));
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0095"),
        "expected E0095 (invalid fixed-point type), got: {:?}",
        report.diagnostics
    );
}

// --- Generated C carries the scaling shifts ----------------------------------

#[test]
fn generated_c_scales_fixed_casts_and_multiply() {
    let bundle = ol_clite_emit::emit_project(&fixed_model());
    let src = &bundle.source;
    // int→fixed shifts left by frac; fixed→int divides by 2^frac.
    assert!(src.contains("<< 8"), "int→fixed scale missing:\n{src}");
    assert!(src.contains("/ ((int64_t)1 << 8)"), "fixed→int truncation missing:\n{src}");
    // Fixed multiply uses an int64 intermediate then `>> frac`.
    assert!(src.contains("(int64_t)") && src.contains(">> 8"), "fixed multiply missing:\n{src}");
}
