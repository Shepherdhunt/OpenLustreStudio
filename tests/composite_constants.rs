//! Composite (array / record / char) constant VALUES, end to end: a project
//! defines an `int32[3]` array constant and a `char` constant, an operator
//! indexes the array and emits the char, and we assert the value threads
//! correctly through the type checker, the IR simulator, and the generated
//! C-Lite. A `cc`-gated case proves the simulator and compiled C agree
//! byte-for-byte on an array-constant lookup.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_ir::{ConstDef, Equation, Expr, Literal, NodeDef, NodeKind, Package, Port, Project, Type};

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn int32(n: i64) -> Expr {
    Expr::Const { lit: Literal::int(n) }
}

/// `PALETTE : int32[3] = [10; 20; 30]` plus `NL : char = '\n'`, used by an
/// operator `Pick(idx) returns (chosen : int32, nl : char)` where
/// `chosen = PALETTE[idx]` and `nl = NL`.
fn build_project() -> Project {
    let palette = ConstDef {
        name: "PALETTE".into(),
        ty: Type::Array { elem: Box::new(Type::Int32), len: 3 },
        value: Expr::array(vec![int32(10), int32(20), int32(30)]),
    };
    let nl = ConstDef {
        name: "NL".into(),
        ty: Type::Char,
        value: Expr::Const { lit: Literal::char(b'\n') },
    };
    let pick = NodeDef {
        name: "Pick".into(),
        kind: NodeKind::Operator,
        inputs: vec![Port { name: "idx".into(), ty: Type::Int32 }],
        outputs: vec![
            Port { name: "chosen".into(), ty: Type::Int32 },
            Port { name: "nl".into(), ty: Type::Char },
        ],
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["chosen".into()],
                rhs: Expr::Index {
                    base: Box::new(Expr::var("PALETTE")),
                    index: Box::new(Expr::var("idx")),
                },
            },
            Equation { lhs: vec!["nl".into()], rhs: Expr::var("NL") },
        ],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
    };
    Project {
        name: "composite_consts".into(),
        packages: vec![Package {
            name: "user".into(),
            constants: vec![palette, nl],
            nodes: vec![pick],
            ..Default::default()
        }],
        main: Some("Pick".into()),
        ..Default::default()
    }
}

#[test]
fn composite_constants_typecheck_sim_and_emit() {
    let project = build_project();

    // 1. The type checker accepts the composite constants and the operator
    //    that indexes the array / reads the char.
    let report = ol_typecheck::check_project(&project);
    assert!(
        !report.has_errors(),
        "composite-constant project should typecheck: {:?}",
        report.errors().collect::<Vec<_>>()
    );

    // 2. The simulator evaluates the constants once and the operator reads
    //    them: chosen tracks PALETTE[idx]; nl is always the newline byte (10).
    let mut sim = ol_sim::Sim::new(&project, "Pick").unwrap();
    let trace = sim.run_csv("idx\n0\n1\n2\n").unwrap();
    // Columns: cycle, chosen, nl.
    let chosen: Vec<i64> = trace
        .rows
        .iter()
        .map(|r| r[1].as_int().expect("chosen is int"))
        .collect();
    assert_eq!(chosen, vec![10, 20, 30], "array-constant lookup in sim");
    for r in &trace.rows {
        assert_eq!(r[2].as_int(), Some(10), "char constant '\\n' == 10 in sim");
    }

    // 3. The generated C carries the array constant as real `static const`
    //    storage (so it can be indexed) and the scalar char as a `#define`.
    let bundle = ol_clite_emit::emit_project(&project);
    assert!(
        bundle.header.contains("static const int32_t PALETTE[3] = {(10), (20), (30)}"),
        "array constant emitted as static const:\n{}",
        bundle.header
    );
    assert!(
        bundle.header.contains("#define NL ((10))"),
        "char constant emitted as a scalar #define:\n{}",
        bundle.header
    );
    assert!(
        bundle.source.contains("PALETTE["),
        "operator indexes the array constant by name:\n{}",
        bundle.source
    );
}

/// `cc`-gated: the IR simulator and the compiled generated C must agree
/// byte-for-byte on an array-constant lookup. Skips where no `cc` is on PATH
/// (e.g. the dev Windows box, which uses the MSVC scenario harness instead);
/// runs in CI.
#[test]
fn array_constant_lookup_agrees_across_backends() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }

    // An int-only operator so the CSV driver and the simulator format every
    // column identically (a char column would print differently per backend).
    let project = Project {
        name: "array_const".into(),
        packages: vec![Package {
            name: "user".into(),
            constants: vec![ConstDef {
                name: "PALETTE".into(),
                ty: Type::Array { elem: Box::new(Type::Int32), len: 3 },
                value: Expr::array(vec![int32(10), int32(20), int32(30)]),
            }],
            nodes: vec![NodeDef {
                name: "ArrayPick".into(),
                kind: NodeKind::Operator,
                inputs: vec![Port { name: "idx".into(), ty: Type::Int32 }],
                outputs: vec![Port { name: "chosen".into(), ty: Type::Int32 }],
                locals: vec![],
                equations: vec![Equation {
                    lhs: vec!["chosen".into()],
                    rhs: Expr::Index {
                        base: Box::new(Expr::var("PALETTE")),
                        index: Box::new(Expr::var("idx")),
                    },
                }],
                contract: None,
                diagram: Default::default(),
                probes: vec![],
            }],
            ..Default::default()
        }],
        main: Some("ArrayPick".into()),
        ..Default::default()
    };

    const INPUT_CSV: &str = "idx\n0\n1\n2\n2\n1\n0\n";

    let ir_trace = {
        let mut sim = ol_sim::Sim::new(&project, "ArrayPick").unwrap();
        sim.run_csv(INPUT_CSV).unwrap().to_csv()
    };

    let bundle = ol_clite_emit::emit_project(&project);
    let entry = project.find_node("ArrayPick").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver(entry);

    let tmp = tempdir_in(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))).expect("temp dir");
    let header_path = tmp.join("openlustre_generated.h");
    let source_path = tmp.join("openlustre_generated.c");
    let driver_path = tmp.join("driver.c");
    let exe_path = tmp.join("array_const_driver");
    std::fs::write(&header_path, &bundle.header).unwrap();
    std::fs::write(&source_path, &bundle.source).unwrap();
    std::fs::write(&driver_path, &driver).unwrap();

    let cc = Command::new("cc")
        .args([
            "-std=c11", "-Wall", "-Wextra", "-Wno-unused-but-set-variable",
            "-Wno-unused-variable", "-Werror", "-o",
        ])
        .arg(&exe_path)
        .arg(&source_path)
        .arg(&driver_path)
        .arg(format!("-I{}", tmp.display()))
        .output()
        .expect("cc runs");
    if !cc.status.success() {
        panic!(
            "cc failed:\nstderr:\n{}\n--- header ---\n{}\n--- source ---\n{}",
            String::from_utf8_lossy(&cc.stderr),
            bundle.header,
            bundle.source,
        );
    }

    let mut child = Command::new(&exe_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("driver runs");
    use std::io::Write as _;
    child.stdin.as_mut().unwrap().write_all(INPUT_CSV.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("driver finishes");
    assert!(out.status.success(), "driver crashed: {:?}", out);
    let clite_trace = String::from_utf8(out.stdout).unwrap();

    let matched = ir_trace == clite_trace;
    let _ = std::fs::remove_dir_all(&tmp);
    if !matched {
        panic!("trace mismatch\n--- IR sim ---\n{ir_trace}\n--- C-Lite ---\n{clite_trace}");
    }
}

fn tempdir_in(parent: &std::path::Path) -> std::io::Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = parent.join(format!("__composite_tmp_{stamp}"));
    std::fs::create_dir_all(&p)?;
    Ok(p)
}
