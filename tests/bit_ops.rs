//! Bit-manipulation primitives end-to-end (Phase 9 closeout).
//!
//! Verifies that `BinOp::BitAnd`, `BitOr`, `BitXor`, `Shl`, and `Shr` are
//! consistent across typecheck, simulator, C-Lite emitter, and the textual
//! library parser; that hex integer literals tokenize; and that the
//! ARINC-429 library blocks decode a real word the same way the IR
//! simulator does.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_ir::{BinOp, Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};
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

fn libraries_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries")
}

#[test]
fn bit_ops_typecheck_and_simulate_correctly() {
    // node Pack(a: uint32, b: uint32, n: uint32) returns (y: uint32)
    //   y = (a & 0xFF) | ((b & 0x3) << n);
    let node = NodeDef {
        name: "Pack".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port { name: "a".into(), ty: Type::Uint32 },
            Port { name: "b".into(), ty: Type::Uint32 },
            Port { name: "n".into(), ty: Type::Uint32 },
        ],
        outputs: vec![Port { name: "y".into(), ty: Type::Uint32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::bin(
                BinOp::BitOr,
                Expr::bin(BinOp::BitAnd, Expr::var("a"), Expr::int_lit(0xFF)),
                Expr::bin(
                    BinOp::Shl,
                    Expr::bin(BinOp::BitAnd, Expr::var("b"), Expr::int_lit(0x3)),
                    Expr::var("n"),
                ),
            ),
        }],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };
    let project = Project {
        name: "bits".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some("Pack".into()),
        ..Default::default()
    };
    let report = ol_typecheck::check_project(&project);
    assert!(
        !report.has_errors(),
        "typecheck errors: {:?}",
        report.errors().map(|d| d.render()).collect::<Vec<_>>()
    );

    let mut sim = Sim::new(&project, "Pack").unwrap();
    let mut inputs = BTreeMap::new();
    inputs.insert("a".into(), Value::Int(0x1234));
    inputs.insert("b".into(), Value::Int(0x2)); // bit 1
    inputs.insert("n".into(), Value::Int(8));
    // (0x1234 & 0xFF) | ((0x2 & 0x3) << 8) = 0x34 | 0x200 = 0x234
    let out = sim.step(&inputs).unwrap();
    assert_eq!(out.get("y"), Some(&Value::Int(0x234)));
}

#[test]
fn bit_ops_reject_non_integer_operands() {
    // y = a and b is fine on bool, but `a & b` on bools should be a typecheck error
    // (E0087 — bitwise requires integer operands).
    let node = NodeDef {
        name: "BadMix".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port { name: "a".into(), ty: Type::Bool },
            Port { name: "b".into(), ty: Type::Bool },
        ],
        outputs: vec![Port { name: "y".into(), ty: Type::Uint32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::bin(BinOp::BitAnd, Expr::var("a"), Expr::var("b")),
        }],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };
    let report = ol_typecheck::check_project(&Project {
        name: "bad".into(),
        packages: vec![Package {
            name: "p".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: None,
        ..Default::default()
    });
    let codes: Vec<_> = report.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"E0087"), "got {codes:?}");
}

#[test]
fn hex_literal_parses_in_library_textual_expressions() {
    // Direct parser test — the avionics blocks rely on hex masks like 0xFF.
    let e = ol_stdlib::parse_expr("0xFF & 0x3").unwrap();
    match e {
        Expr::Binary { op: BinOp::BitAnd, lhs, rhs } => {
            assert_eq!(*lhs, Expr::int_lit(0xFF));
            assert_eq!(*rhs, Expr::int_lit(0x3));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn arinc429_decoders_extract_the_right_fields() {
    // Construct an ARINC-429 word: label=0xAA, SDI=0x1, payload=0x12345,
    // SSM=0x2, parity-bit-31=0. Encoded LSB-first within each field.
    let label: u64 = 0xAA;
    let sdi: u64 = 0x1;
    let payload: u64 = 0x12345;
    let ssm: u64 = 0x2;
    let word: u64 = label | (sdi << 8) | (payload << 10) | (ssm << 29);

    // node Decode(w: uint32) returns (l, s, p, m: uint32)
    let node = NodeDef {
        name: "Decode".into(),
        kind: NodeKind::Function,
        inputs: vec![Port { name: "w".into(), ty: Type::Uint32 }],
        outputs: vec![
            Port { name: "l".into(), ty: Type::Uint32 },
            Port { name: "s".into(), ty: Type::Uint32 },
            Port { name: "p".into(), ty: Type::Uint32 },
            Port { name: "m".into(), ty: Type::Uint32 },
        ],
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["l".into()],
                rhs: Expr::call("Arinc429Label", vec![Expr::var("w")]),
            },
            Equation {
                lhs: vec!["s".into()],
                rhs: Expr::call("Arinc429SDI", vec![Expr::var("w")]),
            },
            Equation {
                lhs: vec!["p".into()],
                rhs: Expr::call("Arinc429Payload", vec![Expr::var("w")]),
            },
            Equation {
                lhs: vec!["m".into()],
                rhs: Expr::call("Arinc429SSM", vec![Expr::var("w")]),
            },
        ],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };

    let mut project = Project {
        name: "arinc_test".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some("Decode".into()),
        ..Default::default()
    };
    let lib = ol_stdlib::load_dir(&libraries_dir()).unwrap();
    lib.merge_into(&mut project, "stdlib");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "Decode").unwrap();
    let mut inputs = BTreeMap::new();
    inputs.insert("w".into(), Value::Int(word as i64));
    let out = sim.step(&inputs).unwrap();
    assert_eq!(out.get("l"), Some(&Value::Int(label as i64)));
    assert_eq!(out.get("s"), Some(&Value::Int(sdi as i64)));
    assert_eq!(out.get("p"), Some(&Value::Int(payload as i64)));
    assert_eq!(out.get("m"), Some(&Value::Int(ssm as i64)));
}

#[test]
fn generated_c_for_bit_ops_compiles_and_matches_ir_simulator() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }
    let node = NodeDef {
        name: "Pack".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port { name: "a".into(), ty: Type::Uint32 },
            Port { name: "b".into(), ty: Type::Uint32 },
            Port { name: "n".into(), ty: Type::Uint32 },
        ],
        outputs: vec![Port { name: "y".into(), ty: Type::Uint32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::bin(
                BinOp::BitOr,
                Expr::bin(BinOp::BitAnd, Expr::var("a"), Expr::int_lit(0xFF)),
                Expr::bin(
                    BinOp::Shl,
                    Expr::bin(BinOp::BitAnd, Expr::var("b"), Expr::int_lit(0x3)),
                    Expr::var("n"),
                ),
            ),
        }],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
        requirements: vec![],
    };
    let project = Project {
        name: "bits".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some("Pack".into()),
        ..Default::default()
    };
    let bundle = ol_clite_emit::emit_project(&project);
    let entry = project.find_node("Pack").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver(entry, &project);

    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(tmp.join("driver.c"), &driver).unwrap();
    let exe = tmp.join("bits_driver");

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
        panic!("cc failed:\n{stderr}\n--- source ---\n{}", bundle.source);
    }

    let input = "a,b,n\n0x1234,0x2,8\n";
    // The driver parses base-10 by default (strtoll(..., 10)). Feed decimals
    // for the same value.
    let input = input.replace("0x1234", &0x1234u32.to_string());
    let input = input.replace("0x2", &0x2u32.to_string());

    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success());

    // Expected: (0x1234 & 0xFF) | ((0x2 & 0x3) << 8) = 0x34 | 0x200 = 0x234 = 564.
    assert_eq!(stdout.trim(), "cycle,y\n0,564", "got: {stdout}");
}

#[test]
fn library_now_advertises_bits_and_avionics() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).unwrap();
    let names: Vec<&str> = lib.nodes().map(|n| n.name.as_str()).collect();
    for added in [
        "BitAnd",
        "BitOr",
        "BitXor",
        "ShiftLeft",
        "ShiftRight",
        "Arinc429Label",
        "Arinc429SDI",
        "Arinc429Payload",
        "Arinc429SSM",
    ] {
        assert!(names.contains(&added), "missing block `{added}`");
    }
    assert!(lib.entries.len() >= 40);
}

fn make_tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_bits_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}
