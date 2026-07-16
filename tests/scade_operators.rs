//! SCADE predefined-operator parity: `#` (sharp), bounds-safe dynamic
//! projection `(a.[i] default d)`, replication `replicate(v, n)`, slice
//! `a[lo .. hi]`, `transpose`, and functional update `(a with [i]=v)` /
//! `(r with .f=v)`. Each is exercised through parse/format round-trip,
//! typecheck (accept + reject), IR simulation, and — for the value-carrying
//! ones — the IR-vs-compiled-C equivalence run.

use std::path::PathBuf;
use std::process::Command;

use ol_ir::Expr;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn parse(s: &str) -> Expr {
    ol_stdlib::parse_expr(s).unwrap_or_else(|e| panic!("parse {s:?}: {e}"))
}

/// Parse → format → parse must be a fixed point (surface syntax round-trips).
fn assert_round_trips(s: &str) {
    let e = parse(s);
    let f = ol_lustre_emit::format_expr(&e);
    let e2 = ol_stdlib::parse_expr(&f).unwrap_or_else(|err| panic!("reparse {f:?}: {err}"));
    assert_eq!(e, e2, "round-trip changed the IR: {s:?} -> {f:?}");
}

// --- Parse / format round-trips --------------------------------------------------

#[test]
fn scade_operators_parse_and_round_trip() {
    // `#` sharp.
    let e = parse("#(a, b, c)");
    assert!(matches!(&e, Expr::Sharp { args } if args.len() == 3), "{e:?}");
    assert_eq!(ol_lustre_emit::format_expr(&e), "#(a, b, c)");
    // The Kind 2 view expands sharp to a provable boolean formula.
    assert_eq!(
        ol_lustre_emit::format_expr_lustre(&e),
        "not (a and b) and not (a and c) and not (b and c)"
    );

    // Dynamic projection.
    let e = parse("(xs.[i] default 0)");
    assert!(matches!(&e, Expr::DynIndex { .. }), "{e:?}");
    assert_round_trips("(xs.[i] default 0)");

    // Replication, slice, transpose, updates.
    assert!(matches!(parse("replicate(v, 4)"), Expr::Replicate { .. }));
    assert!(matches!(parse("xs[1 .. 3]"), Expr::Slice { .. }));
    assert!(matches!(parse("transpose(m)"), Expr::Transpose { .. }));
    assert!(matches!(parse("(xs with [2] = 9)"), Expr::Update { index: Some(_), .. }));
    assert!(matches!(parse("(r with .field = 7)"), Expr::Update { field: Some(_), .. }));

    assert_round_trips("replicate(v, 4)");
    assert_round_trips("xs[1 .. 3]");
    assert_round_trips("transpose(m)");
    assert_round_trips("(xs with [2] = 9)");
    assert_round_trips("(r with .field = 7)");

    // A plain index and a slice are distinct; a plain field access still works.
    assert!(matches!(parse("xs[2]"), Expr::Index { .. }));
    assert!(matches!(parse("r.field"), Expr::Field { .. }));

    // Malformed forms are rejected, not silently mis-parsed.
    assert!(ol_stdlib::parse_expr("(xs.[i])").is_err(), "projection needs a default");
    assert!(ol_stdlib::parse_expr("replicate(v)").is_err());
    assert!(ol_stdlib::parse_expr("transpose(a, b)").is_err());
}

// --- Sharp: boolean, dual-backend --------------------------------------------------

fn sharp_model() -> serde_json::Value {
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": parse(body)})
    };
    serde_json::json!({
        "name": "shp",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "AtMostOne",
                "kind": "Function",
                "inputs": [
                    {"name": "a", "ty": {"kind": "Bool"}},
                    {"name": "b", "ty": {"kind": "Bool"}},
                    {"name": "c", "ty": {"kind": "Bool"}}
                ],
                "outputs": [{"name": "ok", "ty": {"kind": "Bool"}}],
                "equations": [eq("ok", "#(a, b, c)")]
            }]
        }],
        "main": "AtMostOne"
    })
}

#[test]
fn sharp_typechecks_simulates_and_matches_c() {
    let p: ol_ir::Project = serde_json::from_value(sharp_model()).unwrap();
    assert!(ol_typecheck::check_project(&p).diagnostics.is_empty());

    // A non-boolean operand is E0195.
    let mut bad = sharp_model();
    bad["packages"][0]["nodes"][0]["inputs"][0]["ty"] = serde_json::json!({"kind": "Int32"});
    let pb: ol_ir::Project = serde_json::from_value(bad).unwrap();
    assert!(ol_typecheck::check_project(&pb).diagnostics.iter().any(|d| d.code == "E0195"));

    let mut sim = ol_sim::Sim::new(&p, "AtMostOne").unwrap();
    let trace = sim
        .run_csv("a,b,c\nfalse,false,false\ntrue,false,false\ntrue,true,false\ntrue,true,true\n")
        .unwrap();
    let lines: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert_eq!(lines[1], "0,true", "none true → ok");
    assert_eq!(lines[2], "1,true", "one true → ok");
    assert_eq!(lines[3], "2,false", "two true → not ok");
    assert_eq!(lines[4], "3,false", "three true → not ok");

    dual_backend("sharp", &sharp_model(), "AtMostOne",
        "a,b,c\nfalse,false,false\ntrue,false,false\nfalse,true,true\ntrue,true,true\n");
}

// --- Dynamic projection, replication, slice, update: int arrays, dual-backend -----

fn arr_ty(len: u32) -> serde_json::Value {
    serde_json::json!({"kind": "Array", "elem": {"kind": "Int32"}, "len": len})
}

fn arrays_model() -> serde_json::Value {
    let eq = |lhs: &str, body: &str| serde_json::json!({"lhs": [lhs], "rhs": parse(body)});
    serde_json::json!({
        "name": "arrops",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Arr",
                "kind": "Function",
                "inputs": [
                    {"name": "xs", "ty": arr_ty(4)},
                    {"name": "i", "ty": {"kind": "Int32"}},
                    {"name": "v", "ty": {"kind": "Int32"}}
                ],
                "outputs": [
                    {"name": "pick", "ty": {"kind": "Int32"}},     // dynamic projection
                    {"name": "rep", "ty": arr_ty(3)},              // replication
                    {"name": "sub", "ty": arr_ty(2)},              // slice
                    {"name": "upd", "ty": arr_ty(4)}               // functional update
                ],
                "equations": [
                    eq("pick", "(xs.[i] default -1)"),
                    eq("rep", "replicate(v, 3)"),
                    eq("sub", "xs[1 .. 2]"),
                    eq("upd", "(xs with [i] = v)")
                ]
            }]
        }],
        "main": "Arr"
    })
}

#[test]
fn array_operators_simulate_and_match_c() {
    let p: ol_ir::Project = serde_json::from_value(arrays_model()).unwrap();
    let diags = ol_typecheck::check_project(&p);
    assert!(diags.diagnostics.is_empty(), "{:?}", diags.diagnostics);

    let mut sim = ol_sim::Sim::new(&p, "Arr").unwrap();
    // xs = [10;20;30;40], i = 2, v = 99.
    let trace = sim.run_csv("xs,i,v\n[10;20;30;40],2,99\n").unwrap();
    let l: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert_eq!(l[0], "cycle,pick,rep,sub,upd");
    // pick = xs[2] = 30; rep = [99;99;99]; sub = xs[1..2] = [20;30];
    // upd = xs with [2]=99 = [10;20;99;40].
    assert_eq!(l[1], "0,30,[99;99;99],[20;30],[10;20;99;40]");

    // Out-of-range index: dynamic projection returns the default, and the
    // functional update is a no-op (both bounds-safe, never a fault).
    let mut sim = ol_sim::Sim::new(&p, "Arr").unwrap();
    let trace = sim.run_csv("xs,i,v\n[10;20;30;40],9,0\n").unwrap();
    let l: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert!(l[1].starts_with("0,-1,"), "out-of-range projection is the default -1: {}", l[1]);
    assert!(l[1].ends_with(",[10;20;30;40]"), "out-of-range update leaves the array unchanged: {}", l[1]);

    // Every scenario includes an out-of-range index — the safety guarantee is
    // exercised on both backends, which must still agree.
    dual_backend("arrops", &arrays_model(), "Arr",
        "xs,i,v\n[10;20;30;40],2,99\n[1;2;3;4],0,-5\n[7;7;7;7],9,0\n[5;6;7;8],3,42\n");
}

// --- Slice out-of-range is a static type error ------------------------------------

#[test]
fn slice_out_of_range_is_rejected() {
    let mut m = arrays_model();
    // xs[1 .. 9] on a length-4 array.
    m["packages"][0]["nodes"][0]["equations"][2]["rhs"] = serde_json::to_value(parse("xs[1 .. 9]")).unwrap();
    m["packages"][0]["nodes"][0]["outputs"][2]["ty"] = arr_ty(9);
    let p: ol_ir::Project = serde_json::from_value(m).unwrap();
    let diags = ol_typecheck::check_project(&p);
    assert!(diags.diagnostics.iter().any(|d| d.code == "E0198"), "{:?}", diags.diagnostics);
}

// --- Transpose: 2-D int matrix ----------------------------------------------------

#[test]
fn transpose_simulates_and_matches_c() {
    // The CSV boundary carries scalars and 1-D arrays; a 2-D matrix is built
    // internally from six scalar inputs, transposed, and read back as scalars,
    // so the whole flow (including the dual-backend run) stays at the boundary.
    let row = |len: u32| serde_json::json!({"kind": "Array", "elem": {"kind": "Int32"}, "len": len});
    let mat = |rows: u32, cols: u32| serde_json::json!({"kind": "Array", "elem": row(cols), "len": rows});
    let sc = |n: &str| serde_json::json!({"name": n, "ty": {"kind": "Int32"}});
    let eq = |lhs: &str, body: &str| serde_json::json!({"lhs": [lhs], "rhs": parse(body)});
    let model = serde_json::json!({
        "name": "tp",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "T",
                "kind": "Function",
                "inputs": [sc("a"), sc("b"), sc("c"), sc("d"), sc("e"), sc("f")],
                // mt is the transpose of [[a;b;c];[d;e;f]] = [[a;d];[b;e];[c;f]];
                // read four corners back out.
                "outputs": [sc("o00"), sc("o01"), sc("o10"), sc("o21")],
                "locals": [
                    {"name": "m", "ty": mat(2, 3)},
                    {"name": "mt", "ty": mat(3, 2)}
                ],
                "equations": [
                    eq("m", "[[a; b; c]; [d; e; f]]"),
                    eq("mt", "transpose(m)"),
                    eq("o00", "mt[0][0]"),  // = a
                    eq("o01", "mt[0][1]"),  // = d
                    eq("o10", "mt[1][0]"),  // = b
                    eq("o21", "mt[2][1]")   // = f
                ]
            }]
        }],
        "main": "T"
    });
    let p: ol_ir::Project = serde_json::from_value(model.clone()).unwrap();
    let diags = ol_typecheck::check_project(&p);
    assert!(diags.diagnostics.is_empty(), "{:?}", diags.diagnostics);

    let mut sim = ol_sim::Sim::new(&p, "T").unwrap();
    let trace = sim.run_csv("a,b,c,d,e,f\n1,2,3,4,5,6\n").unwrap();
    let l: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    // mt = [[1;4];[2;5];[3;6]] → o00=1, o01=4, o10=2, o21=6.
    assert_eq!(l[1], "0,1,4,2,6", "transpose corners");

    dual_backend("tp", &model, "T", "a,b,c,d,e,f\n1,2,3,4,5,6\n0,0,0,9,8,7\n-1,-2,-3,-4,-5,-6\n");
}

// --- Functional update of a record ------------------------------------------------

#[test]
fn record_update_simulates_and_matches_c() {
    // Build the record internally from scalars, update one field, read both
    // fields back out — keeping the CSV boundary scalar for the dual run.
    let sc = |n: &str| serde_json::json!({"name": n, "ty": {"kind": "Int32"}});
    let eq = |lhs: &str, body: &str| serde_json::json!({"lhs": [lhs], "rhs": parse(body)});
    let model = serde_json::json!({
        "name": "recu",
        "packages": [{
            "name": "user",
            "types": [{"body": {"kind": "Record", "name": "Pt",
                "fields": [{"name": "x", "ty": {"kind": "Int32"}}, {"name": "y", "ty": {"kind": "Int32"}}]}}],
            "nodes": [{
                "name": "MoveX",
                "kind": "Function",
                "inputs": [sc("ix"), sc("iy"), sc("nx")],
                "outputs": [sc("ox"), sc("oy")],
                "locals": [
                    {"name": "p", "ty": {"kind": "Named", "name": "Pt"}},
                    {"name": "q", "ty": {"kind": "Named", "name": "Pt"}}
                ],
                "equations": [
                    eq("p", "Pt { x: ix, y: iy }"),
                    eq("q", "(p with .x = nx)"),
                    eq("ox", "q.x"),
                    eq("oy", "q.y")
                ]
            }]
        }],
        "main": "MoveX"
    });
    let p: ol_ir::Project = serde_json::from_value(model.clone()).unwrap();
    let diags = ol_typecheck::check_project(&p);
    assert!(diags.diagnostics.is_empty(), "{:?}", diags.diagnostics);

    let mut sim = ol_sim::Sim::new(&p, "MoveX").unwrap();
    let trace = sim.run_csv("ix,iy,nx\n1,2,7\n").unwrap();
    let l: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    // q = {x:1,y:2} with .x = 7 → x=7, y=2 (untouched).
    assert_eq!(l[1], "0,7,2", "record update keeps the other field");

    dual_backend("recu", &model, "MoveX", "ix,iy,nx\n1,2,7\n5,6,-3\n0,0,100\n");

    // Updating a field that doesn't exist is E0200.
    let mut bad = model.clone();
    bad["packages"][0]["nodes"][0]["equations"][1]["rhs"] =
        serde_json::to_value(parse("(p with .z = nx)")).unwrap();
    let pb: ol_ir::Project = serde_json::from_value(bad).unwrap();
    assert!(ol_typecheck::check_project(&pb).diagnostics.iter().any(|d| d.code == "E0200"));
}

// --- Shared dual-backend runner ---------------------------------------------------

fn dual_backend(name: &str, model: &serde_json::Value, _node: &str, scenario_csv: &str) {
    let tmp = make_tempdir(name);
    let model_path = tmp.join("model.json");
    std::fs::write(&model_path, serde_json::to_string_pretty(model).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join(format!("{name}.csv")), scenario_csv).unwrap();

    let run = |args: &[&str]| -> (bool, String) {
        let out = Command::new(env!("CARGO"))
            .args(["run", "-q", "-p", "ol_cli", "--"])
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        )
    };
    let (ok, out) = run(&["test", "record", model_path.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap()]);
    assert!(ok, "record: {out}");
    let (ok, out) = run(&["test", "run", model_path.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "both"]);
    assert!(ok, "run: {out}");
    assert!(out.contains(&format!("[PASS] {name} (ir)")), "IR backend: {out}");
    assert!(out.contains(&format!("[PASS] {name} (c )")), "C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
