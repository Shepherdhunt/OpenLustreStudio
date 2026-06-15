//! Array iterators (`map` / `fold`) end to end: surface syntax, typecheck
//! rules, held element-wise simulation, and IR-vs-compiled-C trace
//! equivalence over array I/O.
//!
//! map applies a stateless function across same-length arrays to build an
//! array; fold left-reduces an array to a scalar. The dual-backend scenario
//! test compiles the generated C (with array CSV I/O) and checks it agrees
//! with the IR simulator cell-by-cell.

use std::path::PathBuf;
use std::process::Command;

use ol_ir::Expr;

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

fn p(s: &str) -> Expr {
    ol_stdlib::parse_expr(s).unwrap_or_else(|e| panic!("parse `{s}` failed: {e}"))
}

fn load(model: serde_json::Value) -> ol_ir::Project {
    serde_json::from_value(model).expect("model deserializes")
}

// --- Surface syntax -----------------------------------------------------------

#[test]
fn map_and_fold_parse_and_round_trip() {
    assert_eq!(
        p("map(Scale, xs)"),
        Expr::map("Scale", vec![Expr::var("xs")])
    );
    assert_eq!(
        p("map(Add, xs, ys)"),
        Expr::map("Add", vec![Expr::var("xs"), Expr::var("ys")])
    );
    assert_eq!(
        p("fold(Add, 0, xs)"),
        Expr::fold("Add", Expr::int_lit(0), Expr::var("xs"))
    );

    for src in ["map(Scale, xs)", "map(Add, xs, ys)", "fold(Add, 0, xs)"] {
        let e = p(src);
        let text = ol_lustre_emit::format_expr(&e);
        assert_eq!(p(&text), e, "`{src}` -> `{text}` must round-trip");
    }

    // The first argument must be a function name; arities are checked.
    assert!(ol_stdlib::parse_expr("map(1, xs)").is_err());
    assert!(ol_stdlib::parse_expr("map(F)").is_err());
    assert!(ol_stdlib::parse_expr("fold(F, init)").is_err());
}

// --- The models: a saturating-scale map and a sum fold ------------------------
//
//   function Scale(x: int32) returns (y: int32)   y = x * 3;
//   function AddF(acc: int32, e: int32) returns (s: int32)  s = acc + e;
//   node VecOps(v: int32[4]) returns (scaled: int32[4], total: int32)
//     scaled = map(Scale, v);
//     total  = fold(AddF, 0, v);

fn vecops_model() -> serde_json::Value {
    let rhs = |s: &str| serde_json::to_value(p(s)).unwrap();
    serde_json::json!({
        "name": "iterators",
        "packages": [{
            "name": "user",
            "nodes": [
                {
                    "name": "Scale", "kind": "Function",
                    "inputs": [{"name": "x", "ty": {"kind": "Int32"}}],
                    "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
                    "equations": [{"lhs": ["y"], "rhs": rhs("x * 3")}]
                },
                {
                    "name": "AddF", "kind": "Function",
                    "inputs": [
                        {"name": "acc", "ty": {"kind": "Int32"}},
                        {"name": "e", "ty": {"kind": "Int32"}}
                    ],
                    "outputs": [{"name": "s", "ty": {"kind": "Int32"}}],
                    "equations": [{"lhs": ["s"], "rhs": rhs("acc + e")}]
                },
                {
                    "name": "VecOps", "kind": "Operator",
                    "inputs": [{"name": "v",
                        "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}}],
                    "outputs": [
                        {"name": "scaled",
                         "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}},
                        {"name": "total", "ty": {"kind": "Int32"}}
                    ],
                    "equations": [
                        {"lhs": ["scaled"], "rhs": rhs("map(Scale, v)")},
                        {"lhs": ["total"], "rhs": rhs("fold(AddF, 0, v)")}
                    ]
                }
            ]
        }],
        "main": "VecOps"
    })
}

// --- Typecheck ---------------------------------------------------------------

#[test]
fn well_typed_iterators_check_clean() {
    let report = ol_typecheck::check_project(&load(vecops_model()));
    let errors: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.severity == ol_ir::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn iterator_discipline_violations_are_loud() {
    let one_node = |name: &str, kind: &str, node_json: serde_json::Value| {
        load(serde_json::json!({
            "name": name, "packages": [{"name": "user", "nodes": [
                {
                    "name": "F", "kind": kind,
                    "inputs": [{"name": "x", "ty": {"kind": "Int32"}}],
                    "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
                    "equations": [{"lhs": ["y"], "rhs": serde_json::to_value(p("x + 1")).unwrap()}]
                },
                node_json
            ]}]
        }))
    };
    let array_in = serde_json::json!({"name": "v",
        "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}});

    // Stateful iterated operator: E0141.
    let report = ol_typecheck::check_project(&one_node("bad_stateful", "Operator",
        serde_json::json!({
            "name": "M", "kind": "Operator",
            "inputs": [array_in.clone()],
            "outputs": [{"name": "o", "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}}],
            "equations": [{"lhs": ["o"], "rhs": serde_json::to_value(p("map(F, v)")).unwrap()}]
        })));
    assert!(report.diagnostics.iter().any(|d| d.code == "E0141"),
        "stateful F must be E0141: {:?}", report.diagnostics);

    // map arity: F takes 1 input but two arrays given: E0145.
    let report = ol_typecheck::check_project(&one_node("bad_arity", "Function",
        serde_json::json!({
            "name": "M", "kind": "Operator",
            "inputs": [array_in.clone(),
                {"name": "w", "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}}],
            "outputs": [{"name": "o", "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}}],
            "equations": [{"lhs": ["o"], "rhs": serde_json::to_value(p("map(F, v, w)")).unwrap()}]
        })));
    assert!(report.diagnostics.iter().any(|d| d.code == "E0145"),
        "map arity must be E0145: {:?}", report.diagnostics);

    // Unequal array lengths: E0144.
    let report = ol_typecheck::check_project(&load(serde_json::json!({
        "name": "bad_len", "packages": [{"name": "user", "nodes": [
            {
                "name": "Add2", "kind": "Function",
                "inputs": [{"name": "a", "ty": {"kind": "Int32"}}, {"name": "b", "ty": {"kind": "Int32"}}],
                "outputs": [{"name": "c", "ty": {"kind": "Int32"}}],
                "equations": [{"lhs": ["c"], "rhs": serde_json::to_value(p("a + b")).unwrap()}]
            },
            {
                "name": "M", "kind": "Operator",
                "inputs": [
                    {"name": "v", "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}},
                    {"name": "w", "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 3}}
                ],
                "outputs": [{"name": "o", "ty": {"kind": "Array", "elem": {"kind": "Int32"}, "len": 4}}],
                "equations": [{"lhs": ["o"], "rhs": serde_json::to_value(p("map(Add2, v, w)")).unwrap()}]
            }
        ]}]
    })));
    assert!(report.diagnostics.iter().any(|d| d.code == "E0144"),
        "unequal lengths must be E0144: {:?}", report.diagnostics);

    // Nested iterator (not the whole RHS): E0146.
    let report = ol_typecheck::check_project(&one_node("bad_nest", "Function",
        serde_json::json!({
            "name": "M", "kind": "Operator",
            "inputs": [array_in.clone()],
            "outputs": [{"name": "o", "ty": {"kind": "Int32"}}],
            "equations": [{"lhs": ["o"],
                "rhs": serde_json::to_value(p("fold(F, 0, v) + 1")).unwrap()}]
        })));
    assert!(report.diagnostics.iter().any(|d| d.code == "E0146"),
        "nested iterator must be E0146: {:?}", report.diagnostics);
}

#[test]
fn slicing_keeps_iterated_functions() {
    let project = load(vecops_model());
    let sliced = project.slice_for_root("VecOps").unwrap();
    let mut names: Vec<String> = sliced.all_nodes().map(|n| n.name.clone()).collect();
    names.sort();
    // The iterated Scale/AddF must survive the slice or the generated C
    // would reference undefined functions.
    assert_eq!(names, vec!["AddF", "Scale", "VecOps"]);
}

// --- Simulation --------------------------------------------------------------

#[test]
fn map_builds_an_array_and_fold_reduces_it() {
    let tmp = make_tempdir("iter_sim");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&vecops_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();
    let mut sim = ol_sim::Sim::new(&project, "VecOps").unwrap();

    // v = [1;2;3;4]: scaled = [3;6;9;12], total = 10.
    let trace = sim.run_csv("v\n[1;2;3;4]\n[10;20;0;-5]\n").unwrap();
    let csv = trace.to_csv();
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines[0], "cycle,scaled,total");
    assert_eq!(lines[1], "0,[3;6;9;12],10");
    // v = [10;20;0;-5]: scaled = [30;60;0;-15], total = 25.
    assert_eq!(lines[2], "1,[30;60;0;-15],25");
    let _ = std::fs::remove_dir_all(&tmp);
}

// --- IR vs compiled C, cell by cell ------------------------------------------

#[test]
fn iterator_traces_match_between_ir_and_compiled_c() {
    let tmp = make_tempdir("iter_c");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&vecops_model()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(
        scen.join("vectors.csv"),
        "v\n[1;2;3;4]\n[0;0;0;0]\n[-1;-2;-3;-4]\n[100;200;300;400]\n",
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
    assert!(out.contains("[PASS] vectors (ir)"), "{out}");
    assert!(out.contains("[PASS] vectors (c )"), "iterator C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
