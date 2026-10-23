//! Structs, enums, and arrays end-to-end: the IR simulator computes with
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
            probes: vec![],
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
            ty: Type::named("AdsbMsg"),
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
            probes: vec![],
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
        outputs: vec![Port { name: "m".into(), ty: Type::named("Mode") }],
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
            probes: vec![],
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
        ..Default::default()
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
    let stamp = openlustre_integration_tests::unique_stamp();
    let p =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_compound_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// Studio's input boundary must construct the same runtime values as authored
// model fields; neither a whole-struct Boolean nor a stringified scalar works.
mod typed_inputs {
    use super::*;

    fn sample_project() -> Project {
        let field = |name: &str, ty| RecordField { name: name.into(), ty };
        let types = vec![
            TypeDef { body: TypeBody::Enum(EnumDef {
                name: "Health".into(), variants: vec!["Healthy".into(), "Unknown".into()],
            }) },
            TypeDef { body: TypeBody::Record { name: "Sample".into(), fields: vec![
                field("valid", Type::Bool), field("count", Type::Int64), field("reading", Type::Float32),
            ] } },
            TypeDef { body: TypeBody::Alias { name: "SampleAlias".into(), target: Type::named("Sample") } },
            TypeDef { body: TypeBody::Record { name: "Envelope".into(), fields: vec![
                field("sample", Type::named("SampleAlias")),
                field("samples", Type::Array { elem: Box::new(Type::named("Sample")), len: 2 }),
                field("grid", Type::Array { elem: Box::new(Type::Array { elem: Box::new(Type::Int32), len: 2 }), len: 2 }),
                field("health", Type::named("Health")), field("tag", Type::Char),
            ] } },
            TypeDef { body: TypeBody::Alias { name: "EnvelopeAlias".into(), target: Type::named("Envelope") } },
        ];
        let root = NodeDef {
            name: "Read".into(), kind: NodeKind::Operator,
            inputs: vec![Port { name: "msg".into(), ty: Type::named("EnvelopeAlias") }, Port { name: "enable".into(), ty: Type::Bool }],
            outputs: vec![Port { name: "all_valid".into(), ty: Type::Bool }, Port { name: "count".into(), ty: Type::Int64 }],
            locals: vec![],
            equations: vec![
                Equation { lhs: vec!["all_valid".into()], rhs: ol_stdlib::parse_expr("msg.sample.valid and msg.samples[0].valid and msg.samples[1].valid and enable").unwrap() },
                Equation { lhs: vec!["count".into()], rhs: ol_stdlib::parse_expr("msg.sample.count").unwrap() },
            ],
            contract: None, diagram: Default::default(), probes: vec![],
        };
        project_with(vec![root], types)
    }

    fn input_json() -> serde_json::Value {
        serde_json::json!({
            "sample": {"valid": true, "count": 9007199254740993i64, "reading": 1.25},
            "samples": [
                {"valid": true, "count": 1, "reading": 2.5},
                {"valid": false, "count": 2, "reading": 3.75},
            ],
            "grid": [[1, 2], [3, 4]], "health": "Healthy", "tag": 65,
        })
    }

    #[test]
    fn nested_struct_alias_enum_and_array_json_reaches_runtime_with_exact_values() {
        let project = sample_project();
        assert!(!ol_typecheck::check_project(&project).has_errors());
        let mut sim = Sim::new(&project, "Read").unwrap();
        let parsed = sim.parse_input("msg", &input_json().to_string()).unwrap();
        let csv_text = parsed.to_csv();
        assert_eq!(sim.parse_input("msg", &csv_text).unwrap(), parsed, "nested trace text round-trips");
        let mut inputs = BTreeMap::from([("msg".into(), parsed), ("enable".into(), Value::Bool(true))]);
        let out = sim.step(&inputs).unwrap();
        assert_eq!(out["all_valid"], Value::Bool(false));
        assert_eq!(out["count"], Value::Int(9007199254740993));
        let mut json = input_json();
        json["samples"][1]["valid"] = serde_json::json!(true);
        inputs.insert("msg".into(), sim.parse_input("msg", &json.to_string()).unwrap());
        assert_eq!(sim.step(&inputs).unwrap()["all_valid"], Value::Bool(true));
        inputs.insert("enable".into(), Value::Bool(false));
        assert_eq!(sim.step(&inputs).unwrap()["all_valid"], Value::Bool(false));
    }

    #[test]
    fn input_errors_identify_missing_extra_and_wrong_nested_fields() {
        let project = sample_project();
        let sim = Sim::new(&project, "Read").unwrap();
        let mut missing = input_json();
        missing["sample"].as_object_mut().unwrap().remove("valid");
        let mut extra = input_json();
        extra["sample"]["healthy"] = serde_json::json!(true);
        let mut bool_string = input_json();
        bool_string["samples"][1]["valid"] = serde_json::json!("true");
        let mut number_string = input_json();
        number_string["sample"]["count"] = serde_json::json!("17");
        let mut bad_enum = input_json();
        bad_enum["health"] = serde_json::json!("Good");
        let mut short_array = input_json();
        short_array["grid"][0] = serde_json::json!([1]);
        for (value, path, reason) in [
            (missing, "msg.sample.valid", "missing required field"),
            (extra, "msg.sample.healthy", "unexpected field"),
            (bool_string, "msg.samples[1].valid", "got JSON string"),
            (number_string, "msg.sample.count", "JSON value must be an integer"),
            (bad_enum, "msg.health", "choose an enum variant"),
            (short_array, "msg.grid[0]", "array requires 2 elements"),
        ] {
            let error = sim.parse_input("msg", &value.to_string()).unwrap_err().to_string();
            assert!(error.contains(path) && error.contains(reason), "{error}");
            assert!(!error.contains("record"), "human-facing terminology: {error}");
        }
        for raw in ["", "0", "false", "[]"] {
            let error = sim.parse_input("msg", raw).unwrap_err().to_string();
            assert!(error.contains("struct") && error.contains("msg"), "{error}");
        }
        let good = sim.parse_input("msg", &input_json().to_string()).unwrap();
        let duplicate = good.to_csv().replacen("valid=true", "valid=true;valid=false", 1);
        assert!(sim.parse_input("msg", &duplicate).unwrap_err().to_string().contains("duplicate field"));
    }

    #[test]
    fn schemas_expose_nested_fields_aliases_arrays_and_enum_choices() {
        let project = sample_project();
        let schema = ol_sim::input_schema(&Type::named("EnvelopeAlias"), &project).unwrap();
        assert_eq!(schema["kind"], "alias");
        assert_eq!(schema["target"]["kind"], "struct");
        assert_eq!(schema["target"]["name"], "Envelope");
        let fields = schema["target"]["fields"].as_array().unwrap();
        assert_eq!(fields[0]["name"], "sample");
        assert_eq!(fields[0]["schema"]["target"]["fields"][0]["name"], "valid");
        assert_eq!(fields[0]["schema"]["target"]["fields"][0]["type"], "bool");
        assert_eq!(fields[1]["schema"]["elem"]["kind"], "struct");
        assert_eq!(fields[2]["schema"]["elem"]["len"], 2);
        assert_eq!(fields[3]["schema"]["variants"], serde_json::json!(["Healthy", "Unknown"]));
        let serialized = serde_json::to_value(&project).unwrap();
        assert_eq!(serialized["packages"][0]["types"][1]["body"]["kind"], "Record", "IR compatibility is preserved");
    }

    #[test]
    fn placeholders_are_valid_inactive_values_and_never_choose_an_active_first_enum() {
        let project = sample_project();
        let ty = Type::named("EnvelopeAlias");
        let placeholder = ol_sim::input_placeholder(&ty, &project).unwrap();
        let raw = placeholder.to_csv();
        assert!(raw.contains("valid=false") && raw.contains("health=Unknown"), "{raw}");
        let sim = Sim::new(&project, "Read").unwrap();
        assert_eq!(sim.parse_input("msg", &raw).unwrap(), placeholder);
        let mut ambiguous = project.clone();
        ambiguous.packages[0].types[0] = TypeDef { body: TypeBody::Enum(EnumDef {
            name: "Health".into(), variants: vec!["Healthy".into(), "Unknown".into(), "Invalid".into()],
        }) };
        let error = ol_sim::input_placeholder(&ty, &ambiguous).unwrap_err();
        assert!(error.contains("EnvelopeAlias.health") && error.contains("choose a variant"), "{error}");
        let error = ol_sim::input_placeholder(&Type::Array { elem: Box::new(Type::Bool), len: u32::MAX }, &project).unwrap_err();
        assert!(error.contains("placeholder budget"), "oversized defaults fail before allocation: {error}");
    }

    #[test]
    fn aliases_and_recursive_struct_types_return_bounded_errors() {
        let mut project = sample_project();
        project.packages[0].types.extend([
            TypeDef { body: TypeBody::Alias { name: "A".into(), target: Type::named("B") } },
            TypeDef { body: TypeBody::Alias { name: "B".into(), target: Type::named("A") } },
            TypeDef { body: TypeBody::Record { name: "Loop".into(), fields: vec![RecordField { name: "child".into(), ty: Type::named("Loop") }] } },
        ]);
        for name in ["A", "Loop"] {
            let ty = Type::named(name);
            assert!(ol_sim::input_schema(&ty, &project).unwrap_err().contains("type nesting exceeds 64"));
            assert!(ol_sim::input_placeholder(&ty, &project).unwrap_err().contains("type nesting exceeds 64"));
        }
        project.packages[0].nodes[0].inputs.push(Port { name: "cycle".into(), ty: Type::named("A") });
        let sim = Sim::new(&project, "Read").unwrap();
        assert!(sim.parse_input("cycle", "0").unwrap_err().to_string().contains("type nesting exceeds 64"));
    }

    #[test]
    fn branching_acyclic_type_graph_does_not_expand_unbounded_schema_or_defaults() {
        let mut project = sample_project();
        project.packages[0].types.push(TypeDef { body: TypeBody::Alias { name: "D0".into(), target: Type::Bool } });
        for i in 1..=20 {
            project.packages[0].types.push(TypeDef { body: TypeBody::Record { name: format!("D{i}"), fields: vec![
                RecordField { name: "a".into(), ty: Type::named(format!("D{}", i - 1)) },
                RecordField { name: "b".into(), ty: Type::named(format!("D{}", i - 1)) },
            ] } });
        }
        let ty = Type::named("D20");
        assert!(ol_sim::input_schema(&ty, &project).unwrap_err().contains("10000 schema nodes"));
        assert!(ol_sim::input_placeholder(&ty, &project).unwrap_err().contains("limited to 10000 values"));
        assert!(ol_sim::simulation_type_size(&ty, &project).unwrap_err().contains("shape exceeds 10000 values"));
    }

    #[test]
    fn engine_initialization_shape_is_bounded_for_outputs_and_nested_arrays() {
        let project = sample_project();
        let huge = Type::Array { elem: Box::new(Type::Bool), len: u32::MAX };
        assert!(ol_sim::input_schema(&huge, &project).is_ok(), "array element schema itself is small");
        assert!(ol_sim::simulation_type_size(&huge, &project).unwrap_err().contains("shape exceeds 10000 values"));
        let matrix = Type::Array { elem: Box::new(Type::Array { elem: Box::new(Type::Bool), len: 100 }), len: 100 };
        assert!(ol_sim::simulation_type_size(&matrix, &project).unwrap_err().contains("shape exceeds 10000 values"));
        assert_eq!(ol_sim::simulation_type_size(&Type::Array { elem: Box::new(Type::Bool), len: 5 }, &project).unwrap(), 6);
        assert!(ol_sim::simulation_type_size(&Type::named("EnvelopeAlias"), &project).is_ok(), "ordinary named struct shape remains supported");
    }

    #[test]
    fn unsupported_float_and_byte_values_are_precise_and_bad_entry_can_be_corrected() {
        let project = sample_project();
        let sim = Sim::new(&project, "Read").unwrap();
        for value in [serde_json::json!(1e100), serde_json::json!("NaN")] {
            let mut json = input_json();
            json["sample"]["reading"] = value;
            let error = sim.parse_input("msg", &json.to_string()).unwrap_err().to_string();
            assert!(error.contains("msg.sample.reading") && error.contains("finite float32"), "{error}");
        }
        let mut json = input_json();
        json["tag"] = serde_json::json!(256);
        assert!(sim.parse_input("msg", &json.to_string()).unwrap_err().to_string().contains("0 to 255"));
        json["tag"] = serde_json::json!(65);
        json["sample"]["count"] = serde_json::json!(u64::MAX);
        assert!(sim.parse_input("msg", &json.to_string()).unwrap_err().to_string().contains("larger uint64 inputs are unsupported"));
        assert!(sim.parse_input("msg", &input_json().to_string()).is_ok(), "corrected entry recovers");
    }
}
