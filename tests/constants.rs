//! Const definitions end-to-end: typecheck, simulate, Lustre emit, and
//! C-Lite emit (with cc compile when available) all see project-wide
//! constants.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_ir::{
    BinOp, ConstDef, Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type,
};
use ol_sim::{Sim, Value};

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `const THRESHOLD : int32 = 10; const ENABLE : bool = true;`
/// node Gate(x: int32) returns (y: int32);
///   y = if ENABLE and (x > THRESHOLD) then x else 0;
fn gate_project() -> Project {
    let consts = vec![
        ConstDef {
            name: "THRESHOLD".into(),
            ty: Type::Int32,
            value: Expr::int_lit(10),
        },
        ConstDef {
            name: "ENABLE".into(),
            ty: Type::Bool,
            value: Expr::bool_lit(true),
        },
    ];
    let node = NodeDef {
        name: "Gate".into(),
        kind: NodeKind::Function,
        inputs: vec![Port { name: "x".into(), ty: Type::Int32 }],
        outputs: vec![Port { name: "y".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::if_then_else(
                Expr::and(
                    Expr::var("ENABLE"),
                    Expr::bin(BinOp::Gt, Expr::var("x"), Expr::var("THRESHOLD")),
                ),
                Expr::var("x"),
                Expr::int_lit(0),
            ),
        }],
        contract: None,
        diagram: Default::default(),
    };
    Project {
        name: "gate".into(),
        packages: vec![Package {
            name: "user".into(),
            constants: consts,
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some("Gate".into()),
    }
}

#[test]
fn typecheck_resolves_const_references() {
    let project = gate_project();
    let report = ol_typecheck::check_project(&project);
    assert!(
        !report.has_errors(),
        "errors: {:?}",
        report.errors().map(|d| d.render()).collect::<Vec<_>>()
    );
}

#[test]
fn typecheck_flags_const_value_type_mismatch() {
    let mut project = gate_project();
    // Declare WIDTH as uint8 but give it a value of 500 (out of range).
    project.packages[0].constants.push(ConstDef {
        name: "WIDTH".into(),
        ty: Type::Uint8,
        value: Expr::int_lit(500),
    });
    let report = ol_typecheck::check_project(&project);
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(
        codes.contains(&"E0005"),
        "expected E0005, got {codes:?}"
    );
}

#[test]
fn typecheck_flags_duplicate_constants() {
    let mut project = gate_project();
    project.packages[0].constants.push(ConstDef {
        name: "THRESHOLD".into(),
        ty: Type::Int32,
        value: Expr::int_lit(5),
    });
    let report = ol_typecheck::check_project(&project);
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0003"), "got {codes:?}");
}

#[test]
fn typecheck_rejects_temporal_in_const_value() {
    let mut project = gate_project();
    project.packages[0].constants.push(ConstDef {
        name: "LATCHED".into(),
        ty: Type::Bool,
        value: Expr::arrow(Expr::bool_lit(true), Expr::pre(Expr::var("ENABLE"))),
    });
    let report = ol_typecheck::check_project(&project);
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0004"), "got {codes:?}");
}

#[test]
fn simulator_uses_const_values() {
    let project = gate_project();
    let mut sim = Sim::new(&project, "Gate").unwrap();

    let cases = [
        (5, 0),   // below THRESHOLD
        (10, 0),  // == THRESHOLD, not strictly >
        (11, 11), // > THRESHOLD, ENABLE=true
        (42, 42),
    ];
    for (x, expected) in cases {
        let mut inputs = BTreeMap::new();
        inputs.insert("x".into(), Value::Int(x));
        let out = sim.step(&inputs).unwrap();
        assert_eq!(
            out.get("y"),
            Some(&Value::Int(expected)),
            "x={x} expected y={expected}"
        );
    }
}

#[test]
fn lustre_emitter_renders_const_declarations() {
    let project = gate_project();
    let lus = ol_lustre_emit::emit_project(&project);
    assert!(lus.contains("const THRESHOLD : int = "), "lus:\n{lus}");
    assert!(lus.contains("const ENABLE : bool = "));
}

#[test]
fn clite_emitter_emits_define_macros_for_consts() {
    let project = gate_project();
    let bundle = ol_clite_emit::emit_project(&project);
    assert!(bundle.header.contains("#define THRESHOLD"));
    assert!(bundle.header.contains("#define ENABLE"));
}

#[test]
fn generated_c_for_consts_compiles_and_runs() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }
    let project = gate_project();
    let bundle = ol_clite_emit::emit_project(&project);
    let entry = project.find_node("Gate").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver(entry);

    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(tmp.join("driver.c"), &driver).unwrap();
    let exe = tmp.join("gate_driver");

    let cc = Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Wno-unused-but-set-variable",
            "-Wno-unused-variable",
            "-Werror",
            "-o",
        ])
        .arg(&exe)
        .arg(tmp.join("openlustre_generated.c"))
        .arg(tmp.join("driver.c"))
        .arg(format!("-I{}", tmp.display()))
        .output()
        .expect("cc runs");
    if !cc.status.success() {
        let stderr = String::from_utf8_lossy(&cc.stderr).to_string();
        let _ = std::fs::remove_dir_all(&tmp);
        panic!("cc failed:\n{stderr}\n--- header ---\n{}", bundle.header);
    }

    let input = "x\n5\n10\n11\n42\n";
    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.as_mut().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let success = out.status.success();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(success);
    assert_eq!(
        stdout.trim(),
        "cycle,y\n0,0\n1,0\n2,11\n3,42",
        "got: {stdout}"
    );
}

fn make_tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_const_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}
