//! Reals in the simulator are the generated C's reals: `float32` computes in
//! single precision (`float op float` stays `float`), a `double` operand or a
//! real literal (a C double literal) promotes, a conditional takes its
//! branches' common type like `?:`, and storing, passing an argument or
//! reading an input converts to the declared type. Traces print reals the
//! same way on both sides, so model and code agree byte for byte — checked
//! here over a long run that used to drift apart.

use std::path::PathBuf;
use std::process::Command;

fn make_tempdir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn reals_model() -> serde_json::Value {
    let e = |s: &str| serde_json::to_value(ol_stdlib::parse_expr(s).unwrap()).unwrap();
    let f32t = serde_json::json!({"kind": "Float32"});
    let f64t = serde_json::json!({"kind": "Float64"});
    serde_json::json!({
        "name": "reals",
        "packages": [{
            "name": "user",
            "nodes": [
                {
                    "name": "Square",
                    "kind": "Function",
                    "inputs": [{"name": "v", "ty": f32t}],
                    "outputs": [{"name": "o", "ty": f32t}],
                    "equations": [{"lhs": ["o"], "rhs": e("v * v")}]
                },
                {
                    "name": "Reals",
                    "kind": "Operator",
                    "inputs": [
                        {"name": "x", "ty": f32t}, {"name": "a", "ty": f32t},
                        {"name": "b", "ty": f32t}, {"name": "xd", "ty": f64t}
                    ],
                    "outputs": [
                        {"name": "pos", "ty": f32t}, {"name": "y", "ty": f32t},
                        {"name": "z", "ty": f32t}, {"name": "w", "ty": f64t},
                        {"name": "c", "ty": f32t}, {"name": "s", "ty": f32t}
                    ],
                    "equations": [
                        // An integrator: a double literal → computed in double,
                        // rounded to float on store (the drift case).
                        {"lhs": ["pos"], "rhs": e("(0.0 -> pre pos) + x * 0.01")},
                        // `?:` of a double literal and a float is double, so the
                        // product is double; `x * b` alone is float.
                        {"lhs": ["y"], "rhs": e("(0.0 -> pre y) * a + x * b")},
                        // All float: each operation rounds to float.
                        {"lhs": ["z"], "rhs": e("x * b + a / x")},
                        {"lhs": ["w"], "rhs": e("xd * 0.1 + 0.2")},
                        {"lhs": ["c"], "rhs": e("float32(xd) * b")},
                        // A double argument converts to the float input.
                        {"lhs": ["s"], "rhs": e("Square(x * 0.1)")}
                    ]
                }
            ]
        }],
        "main": "Reals"
    })
}

fn inputs(rows: usize) -> String {
    let mut csv = String::from("x,a,b,xd\n");
    for k in 0..rows {
        let x = 1.3 + (k % 17) as f64 * 0.0123;
        let xd = (k as f64) * 0.1;
        csv.push_str(&format!("{x},0.9,0.1,{xd}\n"));
    }
    csv
}

#[test]
fn float32_runs_in_single_precision_like_the_generated_c() {
    let tmp = make_tempdir("reals_sim");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&reals_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();
    let mut sim = ol_sim::Sim::new(&project, "Reals").unwrap();
    let csv = sim.run_csv("x,a,b,xd\n1.3,0.9,0.1,0.1\n1.3,0.9,0.1,0.2\n1.3,0.9,0.1,0.3\n").unwrap().to_csv();
    let rows: Vec<Vec<&str>> = csv.trim().lines().skip(1).map(|l| l.split(',').collect()).collect();
    // The integrator, computed independently the way C does it.
    let mut pos = 0f32;
    for row in &rows {
        pos = (pos as f64 + (1.3f32 as f64) * 0.01) as f32;
        assert_eq!(row[1], ol_sim::fmt_real(pos as f64, true));
    }
    // Columns: cycle, pos, y, z, w, c, s.
    // All-float arithmetic rounds to float at each operation.
    let z = 1.3f32 * 0.1f32 + 0.9f32 / 1.3f32;
    assert_eq!(rows[0][3], ol_sim::fmt_real(z as f64, true), "float op float rounds each step");
    // A float64 stays double and prints by its double spelling.
    assert_eq!(rows[2][4], ol_sim::fmt_real(0.3f64 * 0.1 + 0.2, false), "w is a double");
    // `?:` with a double literal is double: y = (double)0 * a + (float)(x*b), stored as float.
    let y0 = (0.0f64 * 0.9f32 as f64 + (1.3f32 * 0.1f32) as f64) as f32;
    assert_eq!(rows[0][2], ol_sim::fmt_real(y0 as f64, true), "conditional promotes to double");
    // A double argument converts to the float input: Square((float)(x*0.1)).
    let v = (1.3f32 as f64 * 0.1) as f32;
    assert_eq!(rows[0][6], ol_sim::fmt_real((v * v) as f64, true), "argument converts on passing");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn float_traces_match_the_compiled_c_byte_for_byte() {
    let tmp = make_tempdir("reals_c");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&reals_model()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("long_run.csv"), inputs(3000)).unwrap();
    let run = |args: &[&str]| -> (bool, String) {
        let out = Command::new(env!("CARGO"))
            .args(["run", "-q", "-p", "ol_cli", "--"])
            .args(args)
            .output()
            .unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    };
    let (ok, out) = run(&["test", "record", model.to_str().unwrap(), "--scenarios", scen.to_str().unwrap()]);
    assert!(ok, "record: {out}");
    let (ok, out) = run(&["test", "run", model.to_str().unwrap(), "--scenarios", scen.to_str().unwrap(), "--backend", "both"]);
    assert!(ok, "run: {out}");
    assert!(out.contains("[PASS] long_run (ir)"), "{out}");
    if !out.contains("[SKIP] long_run (c )") {
        assert!(out.contains("[PASS] long_run (c )"), "3000 cycles of float32 and float64, identical in C: {out}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
