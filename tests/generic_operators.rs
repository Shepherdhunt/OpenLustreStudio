//! Generic operator templates: `'T` type polymorphism and `N` array-size
//! parameters, implemented by per-call-site monomorphization. Covers
//! template checking, inference, constraint enforcement, instantiation
//! (type-, size-, and iterator-driven), stateful instances, and the
//! IR-vs-compiled-C equivalence of a monomorphized project.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use ol_sim::Value;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn ty(name: &str) -> serde_json::Value {
    ol_stdlib::parse_type(name).map(|t| serde_json::to_value(t).unwrap()).unwrap()
}

fn eq(lhs: &str, body: &str) -> serde_json::Value {
    serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
}

/// One project exercising every dimension:
/// - `SaturateG('T: numeric)`: type-generic clamp,
/// - `HoldG('T)`: STATEFUL type-generic (x -> pre x),
/// - `SumN`: size-generic over `int32[N]` (via fold of Add2),
/// - `Top`: instantiates SaturateG at float64 AND int32, HoldG at int32,
///   SumN at N=4 and N=2, plus a map over SaturateG (iterator-driven
///   instantiation).
fn generic_model() -> serde_json::Value {
    serde_json::json!({
        "name": "gen",
        "packages": [{
            "name": "user",
            "nodes": [
                {
                    "name": "SaturateG", "kind": "Function",
                    "generics": [{"kind": "type", "name": "T", "constraint": "numeric"}],
                    "inputs": [
                        {"name": "x",  "ty": ty("'T")},
                        {"name": "lo", "ty": ty("'T")},
                        {"name": "hi", "ty": ty("'T")}
                    ],
                    "outputs": [{"name": "y", "ty": ty("'T")}],
                    "equations": [eq("y", "if x > hi then hi else if x < lo then lo else x")]
                },
                {
                    "name": "HoldG", "kind": "Operator",
                    "inputs": [{"name": "x", "ty": ty("'T")}],
                    "outputs": [{"name": "y", "ty": ty("'T")}],
                    "equations": [eq("y", "x -> pre x")]
                },
                {
                    "name": "Add2", "kind": "Function",
                    "inputs": [{"name": "acc", "ty": ty("int32")}, {"name": "e", "ty": ty("int32")}],
                    "outputs": [{"name": "s", "ty": ty("int32")}],
                    "equations": [eq("s", "acc + e")]
                },
                {
                    "name": "SumN", "kind": "Function",
                    "inputs": [{"name": "a", "ty": ty("int32[N]")}],
                    "outputs": [{"name": "s", "ty": ty("int32")}],
                    "equations": [eq("s", "fold(Add2, 0, a)")]
                },
                {
                    "name": "Top", "kind": "Operator",
                    "inputs": [
                        {"name": "f",  "ty": ty("float64")},
                        {"name": "i",  "ty": ty("int32")},
                        {"name": "a4", "ty": ty("int32[4]")},
                        {"name": "a2", "ty": ty("int32[2]")},
                        {"name": "fa", "ty": ty("float64[3]")}
                    ],
                    "outputs": [
                        {"name": "fs", "ty": ty("float64")},
                        {"name": "is", "ty": ty("int32")},
                        {"name": "held", "ty": ty("int32")},
                        {"name": "s4", "ty": ty("int32")},
                        {"name": "s2", "ty": ty("int32")},
                        {"name": "fm", "ty": ty("float64[3]")}
                    ],
                    "equations": [
                        eq("fs", "SaturateG(f, 0.0, 10.0)"),
                        eq("is", "SaturateG(i, -5, 5)"),
                        eq("held", "HoldG(i)"),
                        eq("s4", "SumN(a4)"),
                        eq("s2", "SumN(a2)"),
                        eq("fm", "map(SaturateG, fa, fa, fa)")
                    ]
                }
            ]
        }],
        "main": "Top"
    })
}

fn monomorphized() -> ol_ir::Project {
    let mut p: ol_ir::Project = serde_json::from_value(generic_model()).unwrap();
    let diags = ol_typecheck::monomorphize(&mut p);
    assert!(diags.is_empty(), "{diags:?}");
    p
}

#[test]
fn templates_instantiate_per_call_site_and_typecheck() {
    let p = monomorphized();
    let names: Vec<&str> = p.all_nodes().map(|n| n.name.as_str()).collect();
    for inst in ["SaturateG_float64", "SaturateG_int32", "HoldG_int32", "SumN_4", "SumN_2"] {
        assert!(names.contains(&inst), "missing instance {inst} in {names:?}");
    }
    // Call sites were rewritten; the map now iterates the instance.
    let top = p.find_node("Top").unwrap();
    let texts: Vec<String> =
        top.equations.iter().map(|e| ol_lustre_emit::format_expr(&e.rhs)).collect();
    assert!(texts.iter().any(|t| t == "SaturateG_float64(f, 0.0, 10.0)"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "map(SaturateG_float64, fa, fa, fa)"), "{texts:?}");

    // The monomorphized project is fully type-correct; templates stay in the
    // project (Studio-editable) and check as representative instantiations.
    let r = ol_typecheck::check_project(&p);
    assert!(!r.has_errors(), "{:?}", r.errors().map(|d| d.render()).collect::<Vec<_>>());

    // Neither backend ever sees a template.
    let c = ol_clite_emit::emit_project(&p);
    assert!(c.source.contains("SaturateG_float64_step"), "instance in C");
    assert!(!c.header.contains("'T"), "no type variable leaks into C:\n{}", c.header);
    let lus = ol_lustre_emit::emit_project(&p);
    assert!(lus.contains("SaturateG_float64"), "instance in Lustre");
    assert!(!lus.contains("'T"), "no type variable leaks into Lustre:\n{lus}");
}

#[test]
fn instances_behave_per_binding_including_state() {
    let p = monomorphized();
    let mut sim = ol_sim::Sim::new(&p, "Top").unwrap();
    let step = |sim: &mut ol_sim::Sim, f: f64, i: i64| {
        let mut inputs = BTreeMap::new();
        inputs.insert("f".into(), Value::Float(f));
        inputs.insert("i".into(), Value::Int(i));
        inputs.insert("a4".into(), Value::Array(vec![1, 2, 3, 4].into_iter().map(Value::Int).collect()));
        inputs.insert("a2".into(), Value::Array(vec![10, 20].into_iter().map(Value::Int).collect()));
        inputs.insert("fa".into(), Value::Array(vec![-1.0, 5.0, 99.0].into_iter().map(Value::Float).collect()));
        sim.step(&inputs).unwrap()
    };
    let out = step(&mut sim, 12.5, -9);
    assert_eq!(out["fs"], Value::Float(10.0), "float64 clamp hi");
    assert_eq!(out["is"], Value::Int(-5), "int32 clamp lo");
    assert_eq!(out["held"], Value::Int(-9), "first cycle holds x");
    assert_eq!(out["s4"], Value::Int(10));
    assert_eq!(out["s2"], Value::Int(30));
    // map(SaturateG_float64, fa, fa, fa): clamp(v, v, v) == v per element.
    assert_eq!(
        out["fm"],
        Value::Array(vec![-1.0, 5.0, 99.0].into_iter().map(Value::Float).collect())
    );
    // The stateful instance really carries state: held = previous i.
    let out = step(&mut sim, 3.0, 7);
    assert_eq!(out["held"], Value::Int(-9), "held is last cycle's i");
    assert_eq!(out["fs"], Value::Float(3.0));
}

#[test]
fn inference_and_constraints_are_loud() {
    // Constraint: SaturateG('T: numeric) on bool is E0191.
    let mut m = generic_model();
    m["packages"][0]["nodes"][4]["equations"][0] =
        eq("fs", "SaturateG(true, false, true)");
    // fs stays float64: the constraint fires before any result-type check.
    let mut p: ol_ir::Project = serde_json::from_value(m).unwrap();
    let diags = ol_typecheck::monomorphize(&mut p);
    assert!(
        diags.iter().any(|d| d.code == "E0191" && d.message.contains("numeric")),
        "{diags:?}"
    );

    // Conflicting bindings: 'T can't be float64 and int32 at once.
    let mut m = generic_model();
    m["packages"][0]["nodes"][4]["equations"][0] = eq("fs", "SaturateG(f, i, f)");
    let mut p: ol_ir::Project = serde_json::from_value(m).unwrap();
    let diags = ol_typecheck::monomorphize(&mut p);
    assert!(
        diags.iter().any(|d| d.code == "E0190" && d.message.contains("would bind both")),
        "{diags:?}"
    );

    // An undetermined parameter: 'T appears only in the output.
    let mut m = generic_model();
    m["packages"][0]["nodes"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "ZeroG", "kind": "Function",
        "inputs": [{"name": "n", "ty": ty("int32")}],
        "outputs": [{"name": "y", "ty": ty("'U")}],
        "equations": [eq("y", "n")]
    }));
    m["packages"][0]["nodes"][4]["equations"][1] = eq("is", "ZeroG(i)");
    let mut p: ol_ir::Project = serde_json::from_value(m).unwrap();
    let diags = ol_typecheck::monomorphize(&mut p);
    assert!(
        diags.iter().any(|d| d.code == "E0190" && d.message.contains("not determined")),
        "{diags:?}"
    );

    // A generic template as main is refused.
    let mut m = generic_model();
    m["main"] = serde_json::json!("SaturateG");
    let mut p: ol_ir::Project = serde_json::from_value(m).unwrap();
    let diags = ol_typecheck::monomorphize(&mut p);
    assert!(diags.iter().any(|d| d.code == "E0192"), "{diags:?}");
}

#[test]
fn template_bodies_check_representatively() {
    // An UNCONSTRAINED 'T doing arithmetic is caught at template level: the
    // body needs `'T: numeric` before any instantiation exists.
    let m = serde_json::json!({
        "name": "gen",
        "packages": [{"name": "user", "nodes": [{
            "name": "BadG", "kind": "Function",
            "inputs": [{"name": "x", "ty": ty("'T")}],
            "outputs": [{"name": "y", "ty": ty("'T")}],
            "equations": [eq("y", "x + x")]
        }]}]
    });
    let p: ol_ir::Project = serde_json::from_value(m).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.has_errors(), "unconstrained arithmetic must fail the template check");

    // The same body with `'T: numeric` checks clean (int32 representative).
    let m = serde_json::json!({
        "name": "gen",
        "packages": [{"name": "user", "nodes": [{
            "name": "GoodG", "kind": "Function",
            "generics": [{"kind": "type", "name": "T", "constraint": "numeric"}],
            "inputs": [{"name": "x", "ty": ty("'T")}],
            "outputs": [{"name": "y", "ty": ty("'T")}],
            "equations": [eq("y", "x + x")]
        }]}]
    });
    let p: ol_ir::Project = serde_json::from_value(m).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(!r.has_errors(), "{:?}", r.errors().map(|d| d.render()).collect::<Vec<_>>());
}

#[test]
fn monomorphized_project_matches_compiled_c() {
    let tmp = make_tempdir("generics");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&generic_model()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(
        scen.join("gen.csv"),
        "f,i,a4,a2,fa\n12.5,-9,[1;2;3;4],[10;20],[-1;5;99]\n3.25,7,[0;0;1;0],[-3;3],[2;2;2]\n-4.5,2,[9;9;9;9],[1;1],[0.5;0.25;100]\n",
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
    assert!(out.contains("[PASS] gen (ir)"), "{out}");
    assert!(out.contains("[PASS] gen (c )"), "generic instances C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The generic library blocks work end to end from a user model through the
/// stdlib path (SaturateG / SumN from `libraries/generic/generic.yaml`).
#[test]
fn generic_library_blocks_instantiate_from_a_user_model() {
    let tmp = make_tempdir("generics_lib");
    let model = tmp.join("model.json");
    let m = serde_json::json!({
        "name": "genlib",
        "packages": [{"name": "user", "nodes": [{
            "name": "Use", "kind": "Operator",
            "inputs": [
                {"name": "v", "ty": ty("float32")},
                {"name": "a", "ty": ty("int32[3]")}
            ],
            "outputs": [
                {"name": "c", "ty": ty("float32")},
                {"name": "t", "ty": ty("int32")}
            ],
            "equations": [
                eq("c", "SaturateG(v, 0.0_f32, 1.0_f32)"),
                eq("t", "SumN(a)")
            ]
        }]}],
        "main": "Use"
    });
    std::fs::write(&model, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    let libs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries");
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "check"])
        .arg(&model)
        .arg("--with-stdlib")
        .arg(&libs)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    let _ = std::fs::remove_dir_all(&tmp);
}
