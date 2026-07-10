//! `mapi(F, a…)` / `foldi(F, init, a)` — SCADE's indexed iterators: the
//! iterated function receives the element index (int32, 0-based) as its
//! first argument. Parse/format, typecheck (index input must be an integer;
//! arity rules), simulation, generated C loops, and the IR-vs-compiled-C
//! equivalence run.

use std::path::PathBuf;
use std::process::Command;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn arr_ty(len: u32) -> serde_json::Value {
    serde_json::json!({"kind": "Array", "elem": {"kind": "Int32"}, "len": len})
}

fn i32_ty() -> serde_json::Value {
    serde_json::json!({"kind": "Int32"})
}

/// `Weighted(k, e) = e * (k + 1)` and `AccIdx(k, acc, e) = acc + e * k`:
/// index-dependent bodies, so an off-by-one in EITHER backend flips the
/// trace. `Deck` applies both: `scaled = mapi(Weighted, a)` and
/// `dot = foldi(AccIdx, 0, a)`.
fn deck_model() -> serde_json::Value {
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    serde_json::json!({
        "name": "ii",
        "packages": [{
            "name": "user",
            "nodes": [
                {
                    "name": "Weighted", "kind": "Function",
                    "inputs": [{"name": "k", "ty": i32_ty()}, {"name": "e", "ty": i32_ty()}],
                    "outputs": [{"name": "y", "ty": i32_ty()}],
                    "equations": [eq("y", "e * (k + 1)")]
                },
                {
                    "name": "AccIdx", "kind": "Function",
                    "inputs": [
                        {"name": "k", "ty": i32_ty()},
                        {"name": "acc", "ty": i32_ty()},
                        {"name": "e", "ty": i32_ty()}
                    ],
                    "outputs": [{"name": "y", "ty": i32_ty()}],
                    "equations": [eq("y", "acc + e * k")]
                },
                {
                    "name": "Deck", "kind": "Function",
                    "inputs": [{"name": "a", "ty": arr_ty(4)}],
                    "outputs": [
                        {"name": "scaled", "ty": arr_ty(4)},
                        {"name": "dot", "ty": i32_ty()}
                    ],
                    "equations": [
                        eq("scaled", "mapi(Weighted, a)"),
                        eq("dot", "foldi(AccIdx, 0, a)")
                    ]
                }
            ]
        }],
        "main": "Deck"
    })
}

#[test]
fn indexed_iterators_parse_typecheck_and_reject_misuse() {
    let e = ol_stdlib::parse_expr("mapi(F, a)").expect("parse mapi");
    assert!(matches!(&e, ol_ir::Expr::Iterate { kind: ol_ir::IterKind::Mapi, .. }), "{e:?}");
    assert_eq!(ol_lustre_emit::format_expr(&e), "mapi(F, a)");
    let e = ol_stdlib::parse_expr("foldi(F, 0, a)").expect("parse foldi");
    assert!(matches!(&e, ol_ir::Expr::Iterate { kind: ol_ir::IterKind::Foldi, .. }), "{e:?}");
    assert_eq!(ol_lustre_emit::format_expr(&e), "foldi(F, 0, a)");
    assert!(ol_stdlib::parse_expr("foldi(F, a)").is_err(), "foldi needs (F, init, array)");
    assert!(ol_stdlib::parse_expr("mapi(F)").is_err(), "mapi needs at least one array");

    // The well-shaped model is clean.
    let p: ol_ir::Project = serde_json::from_value(deck_model()).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);

    let has = |mutate: fn(&mut serde_json::Value), code: &str, why: &str| {
        let mut m = deck_model();
        mutate(&mut m);
        let p: ol_ir::Project = serde_json::from_value(m).unwrap();
        let r = ol_typecheck::check_project(&p);
        assert!(
            r.diagnostics.iter().any(|d| d.code == code),
            "{why}: expected {code}, got {:?}",
            r.diagnostics
        );
    };
    // The index input must be an integer type.
    has(|m| {
        m["packages"][0]["nodes"][0]["inputs"][0]["ty"] = serde_json::json!({"kind": "Bool"});
    }, "E0145", "bool index input");
    // Arity: mapi over a 2-input F with two arrays would need 3 inputs.
    has(|m| {
        m["packages"][0]["nodes"][2]["equations"][0]["rhs"] =
            serde_json::to_value(ol_stdlib::parse_expr("mapi(Weighted, a, a)").unwrap()).unwrap();
    }, "E0145", "mapi arity");
    // foldi's F must take exactly (index, accumulator, element).
    has(|m| {
        m["packages"][0]["nodes"][2]["equations"][1]["rhs"] =
            serde_json::to_value(ol_stdlib::parse_expr("foldi(Weighted, 0, a)").unwrap()).unwrap();
    }, "E0145", "foldi arity");
}

#[test]
fn indexed_iterators_simulate_and_match_compiled_c() {
    let tmp = make_tempdir("indexed_iter");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&deck_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();

    // a = [5, 6, 7, 8]:
    //   scaled = [5*1, 6*2, 7*3, 8*4] = [5, 12, 21, 32]
    //   dot    = 5*0 + 6*1 + 7*2 + 8*3 = 44
    let mut sim = ol_sim::Sim::new(&project, "Deck").unwrap();
    let trace = sim.run_csv("a\n[5;6;7;8]\n").unwrap();
    let lines: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert_eq!(lines[0], "cycle,scaled,dot");
    assert_eq!(lines[1], "0,[5;12;21;32],44");

    // The generated C feeds the loop index into F's first input.
    let emitted = ol_clite_emit::emit_project(&project);
    assert!(
        emitted.source.contains(".k = (int32_t)__it"),
        "index feed missing:\n{}",
        emitted.source
    );

    // Dual-backend equivalence.
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("ii.csv"), "a\n[5;6;7;8]\n[-1;0;3;-9]\n[0;0;0;0]\n").unwrap();
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
    assert!(out.contains("[PASS] ii (ir)"), "{out}");
    assert!(out.contains("[PASS] ii (c )"), "indexed iterators C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
