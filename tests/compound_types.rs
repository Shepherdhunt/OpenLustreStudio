//! Records, enums, and arrays end-to-end: the IR simulator computes with
//! Value::Record/Array/Enum, and the C-Lite emitter produces matching
//! `typedef struct`/`typedef enum` declarations that compile under
//! `cc -Wall -Wextra -Werror`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_ir::{
    EnumDef, Equation, Expr, NodeDef, NodeKind, Package, Port, Project, RecordField, Type,
    TypeBody, TypeDef,
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

fn record_typedef() -> TypeDef {
    TypeDef {
        body: TypeBody::Record {
            name: "AdsbMsg".into(),
            fields: vec![
                RecordField { name: "altitude".into(), ty: Type::Int32 },
                RecordField { name: "valid".into(), ty: Type::Bool },
            ],
        },
    }
}

fn enum_typedef() -> TypeDef {
    TypeDef {
        body: TypeBody::Enum(EnumDef {
            name: "Mode".into(),
            variants: vec!["Idle".into(), "Active".into(), "Fault".into()],
        }),
    }
}

fn array_node() -> NodeDef {
    // node Pick(xs: int32[4], i: int32) returns (y: int32)
    //   y = xs[i];
    NodeDef {
        name: "Pick".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port {
                name: "xs".into(),
                ty: Type::Array { elem: Box::new(Type::Int32), len: 4 },
            },
            Port { name: "i".into(), ty: Type::Int32 },
        ],
        outputs: vec![Port { name: "y".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: Expr::Index {
                base: Box::new(Expr::var("xs")),
                index: Box::new(Expr::var("i")),
            },
        }],
        contract: None,
        diagram: Default::default(),
    }
}

fn record_node() -> NodeDef {
    // node ExtractAlt(msg: AdsbMsg) returns (alt: int32, ok: bool)
    //   alt = msg.altitude;
    //   ok  = msg.valid;
    NodeDef {
        name: "ExtractAlt".into(),
        kind: NodeKind::Function,
        inputs: vec![Port {
            name: "msg".into(),
            ty: Type::Named("AdsbMsg".into()),
        }],
        outputs: vec![
            Port { name: "alt".into(), ty: Type::Int32 },
            Port { name: "ok".into(), ty: Type::Bool },
        ],
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["alt".into()],
                rhs: Expr::Field {
                    base: Box::new(Expr::var("msg")),
                    field: "altitude".into(),
                },
            },
            Equation {
                lhs: vec!["ok".into()],
                rhs: Expr::Field {
                    base: Box::new(Expr::var("msg")),
                    field: "valid".into(),
                },
            },
        ],
        contract: None,
        diagram: Default::default(),
    }
}

fn enum_node() -> NodeDef {
    // node Classify(fault: bool, armed: bool) returns (m: Mode)
    //   m = if fault then Fault else if armed then Active else Idle;
    NodeDef {
        name: "Classify".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port { name: "fault".into(), ty: Type::Bool },
            Port { name: "armed".into(), ty: Type::Bool },
        ],
        outputs: vec![Port { name: "m".into(), ty: Type::Named("Mode".into()) }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["m".into()],
            rhs: Expr::if_then_else(
                Expr::var("fault"),
                Expr::var("Fault"),
                Expr::if_then_else(Expr::var("armed"), Expr::var("Active"), Expr::var("Idle")),
            ),
        }],
        contract: None,
        diagram: Default::default(),
    }
}

fn project_with(nodes: Vec<NodeDef>, types: Vec<TypeDef>) -> Project {
    Project {
        name: "compound_types".into(),
        packages: vec![Package {
            name: "user".into(),
            types,
            nodes,
            ..Default::default()
        }],
        main: None,
    }
}

#[test]
fn simulator_reads_record_fields_through_a_function() {
    let project = project_with(vec![record_node()], vec![record_typedef()]);
    // Typechecker must accept the model.
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "ExtractAlt").unwrap();
    let mut msg = BTreeMap::new();
    msg.insert("altitude".into(), Value::Int(33_000));
    msg.insert("valid".into(), Value::Bool(true));
    let mut inputs = BTreeMap::new();
    inputs.insert("msg".into(), Value::Record(msg));
    let out = sim.step(&inputs).unwrap();
    assert_eq!(out.get("alt"), Some(&Value::Int(33_000)));
    assert_eq!(out.get("ok"), Some(&Value::Bool(true)));
}

#[test]
fn simulator_indexes_into_arrays_with_int_indices() {
    let project = project_with(vec![array_node()], vec![]);
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "Pick").unwrap();
    let xs = Value::Array(vec![
        Value::Int(10),
        Value::Int(20),
        Value::Int(30),
        Value::Int(40),
    ]);
    let mut inputs = BTreeMap::new();
    inputs.insert("xs".into(), xs);
    inputs.insert("i".into(), Value::Int(2));
    let out = sim.step(&inputs).unwrap();
    assert_eq!(out.get("y"), Some(&Value::Int(30)));
}

#[test]
fn simulator_resolves_enum_variants_in_expressions() {
    let project = project_with(vec![enum_node()], vec![enum_typedef()]);
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let mut sim = Sim::new(&project, "Classify").unwrap();

    let mut inputs = BTreeMap::new();
    inputs.insert("fault".into(), Value::Bool(true));
    inputs.insert("armed".into(), Value::Bool(false));
    let out = sim.step(&inputs).unwrap();
    assert_eq!(out.get("m"), Some(&Value::Enum("Fault".into())));

    let mut sim2 = Sim::new(&project, "Classify").unwrap();
    let mut inputs = BTreeMap::new();
    inputs.insert("fault".into(), Value::Bool(false));
    inputs.insert("armed".into(), Value::Bool(true));
    let out = sim2.step(&inputs).unwrap();
    assert_eq!(out.get("m"), Some(&Value::Enum("Active".into())));
}

#[test]
fn simulator_array_out_of_bounds_errors_cleanly() {
    let project = project_with(vec![array_node()], vec![]);
    let mut sim = Sim::new(&project, "Pick").unwrap();
    let xs = Value::Array(vec![Value::Int(1), Value::Int(2)]);
    let mut inputs = BTreeMap::new();
    inputs.insert("xs".into(), xs);
    inputs.insert("i".into(), Value::Int(5));
    let err = sim.step(&inputs).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("out of bounds"), "got {msg}");
}

#[test]
fn clite_emitter_emits_typedefs_for_records_and_enums() {
    let project = project_with(
        vec![record_node(), enum_node()],
        vec![record_typedef(), enum_typedef()],
    );
    let bundle = ol_clite_emit::emit_project(&project);
    assert!(bundle.header.contains("typedef struct {"));
    assert!(bundle.header.contains("} AdsbMsg;"));
    assert!(bundle.header.contains("typedef enum {"));
    assert!(bundle.header.contains("} Mode;"));
    assert!(bundle.header.contains("Idle"));
    assert!(bundle.header.contains("Active"));
    assert!(bundle.header.contains("Fault"));
}

#[test]
fn generated_c_lite_for_compound_types_compiles_clean() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }
    let project = project_with(
        vec![record_node(), enum_node(), array_node()],
        vec![record_typedef(), enum_typedef()],
    );
    let bundle = ol_clite_emit::emit_project(&project);

    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();

    let out = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-c", "-o"])
        .arg(tmp.join("model.o"))
        .arg(tmp.join("openlustre_generated.c"))
        .arg(format!("-I{}", tmp.display()))
        .output()
        .expect("cc runs");

    let success = out.status.success();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        success,
        "cc failed:\n{stderr}\n--- header ---\n{}\n--- source ---\n{}",
        bundle.header, bundle.source
    );
}

fn make_tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_compound_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}
