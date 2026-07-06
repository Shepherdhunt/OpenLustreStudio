//! SCADE's `case` (multi-way enum selection) and `fby` (followed-by) as
//! surface operators: parse/format, typecheck rules (E0170–E0174),
//! simulation, generated C, the Kind 2 if-chain view, and the
//! IR-vs-compiled-C equivalence run.

use std::path::PathBuf;
use std::process::Command;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// --- fby ------------------------------------------------------------------

#[test]
fn fby_is_sugar_for_init_arrow_pre() {
    let e = ol_stdlib::parse_expr("fby(x, 0)").expect("parse fby");
    assert_eq!(e, ol_stdlib::parse_expr("0 -> pre x").unwrap(), "same IR");
    // The canonical form is what prints (and what the FBY symbol renders).
    assert_eq!(ol_lustre_emit::format_expr(&e), "0 -> pre x");
    // Profile rule: the delayed flow is a variable.
    assert!(ol_stdlib::parse_expr("fby(x + 1, 0)").is_err());
    assert!(ol_stdlib::parse_expr("fby(x)").is_err());
}

// --- case: parse + format round-trip ---------------------------------------

#[test]
fn case_parses_and_round_trips() {
    let e = ol_stdlib::parse_expr("case(mode, Off: 0, On: level, _: - 1)").expect("parse case");
    let ol_ir::Expr::Case { arms, default, .. } = &e else { panic!("{e:?}") };
    assert_eq!(arms.len(), 2);
    assert!(default.is_some());
    let text = ol_lustre_emit::format_expr(&e);
    assert_eq!(text, "case(mode, Off: 0, On: level, _: - 1)");
    assert_eq!(e, ol_stdlib::parse_expr(&text).unwrap(), "must round-trip");

    // The Kind 2 view is the equivalent if-chain (standard Lustre).
    assert_eq!(
        ol_lustre_emit::format_expr_lustre(&e),
        "if mode = Off then 0 else if mode = On then level else - 1"
    );
    // Exhaustive (no default): the last arm becomes the final else.
    let x = ol_stdlib::parse_expr("case(mode, Off: 0, On: 1)").unwrap();
    assert_eq!(ol_lustre_emit::format_expr_lustre(&x), "if mode = Off then 0 else 1");

    // Loud parse errors: no arms, default not last, two defaults.
    assert!(ol_stdlib::parse_expr("case(mode)").is_err());
    assert!(ol_stdlib::parse_expr("case(mode, _: 1, Off: 0)").is_err());
    assert!(ol_stdlib::parse_expr("case(mode, _: 1, _: 2)").is_err());
}

// --- case: typecheck rules ----------------------------------------------------

fn mode_project(rhs_text: &str) -> ol_ir::Project {
    serde_json::from_value(serde_json::json!({
        "name": "cs",
        "packages": [{
            "name": "user",
            "types": [{"body": {"kind": "Enum", "name": "Mode", "variants": ["Off", "Low", "High"]}}],
            "nodes": [{
                "name": "Sel",
                "kind": "Function",
                "inputs": [
                    {"name": "mode", "ty": {"kind": "Named", "name": "Mode"}},
                    {"name": "level", "ty": {"kind": "Int32"}}
                ],
                "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
                "equations": [{"lhs": ["y"], "rhs": ol_stdlib::parse_expr(rhs_text).unwrap()}]
            }]
        }],
        "main": "Sel"
    }))
    .unwrap()
}

#[test]
fn case_typecheck_rules_are_loud() {
    // Exhaustive over the enum: clean.
    let p = mode_project("case(mode, Off: 0, Low: level, High: level * 2)");
    assert!(ol_typecheck::check_project(&p).diagnostics.is_empty());
    // Default instead of full coverage: clean.
    let p = mode_project("case(mode, Off: 0, _: level)");
    assert!(ol_typecheck::check_project(&p).diagnostics.is_empty());

    let has = |rhs: &str, code: &str| {
        let p = mode_project(rhs);
        let r = ol_typecheck::check_project(&p);
        assert!(
            r.diagnostics.iter().any(|d| d.code == code),
            "expected {code} for `{rhs}`, got {:?}",
            r.diagnostics
        );
    };
    has("case(level, Off: 0, _: 1)", "E0170");           // selector not an enum
    has("case(mode, Nope: 0, _: 1)", "E0171");           // unknown variant
    has("case(mode, Off: 0, Off: 1, _: 2)", "E0172");    // duplicate arm
    has("case(mode, Off: 0, Low: 1)", "E0173");          // non-exhaustive, no default
    has("case(mode, Off: 0, _: true)", "E0174");         // arms disagree in type
}

// --- case + fby: simulate, generate C, dual-backend agreement ------------------

fn traffic_model() -> serde_json::Value {
    // `case` drives an output from an enum input; `fby` delays it a cycle.
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    serde_json::json!({
        "name": "tl",
        "packages": [{
            "name": "user",
            "types": [{"body": {"kind": "Enum", "name": "Mode", "variants": ["Off", "Low", "High"]}}],
            "nodes": [{
                "name": "Levels",
                "kind": "Operator",
                "inputs": [
                    {"name": "mode", "ty": {"kind": "Named", "name": "Mode"}},
                    {"name": "level", "ty": {"kind": "Int32"}}
                ],
                "outputs": [
                    {"name": "out", "ty": {"kind": "Int32"}},
                    {"name": "prev", "ty": {"kind": "Int32"}}
                ],
                "equations": [
                    eq("out", "case(mode, Off: 0, Low: level, High: level * 2)"),
                    eq("prev", "fby(out, -1)")
                ]
            }]
        }],
        "main": "Levels"
    })
}

#[test]
fn case_and_fby_simulate_and_match_compiled_c() {
    let tmp = make_tempdir("case_fby");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&traffic_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();

    // IR simulation: arm selection + the one-cycle delay.
    let mut sim = ol_sim::Sim::new(&project, "Levels").unwrap();
    let trace = sim.run_csv("mode,level\nOff,5\nLow,5\nHigh,5\n").unwrap();
    let lines: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert_eq!(lines[1], "0,0,-1");
    assert_eq!(lines[2], "1,5,0");
    assert_eq!(lines[3], "2,10,5");

    // Generated C: a ternary chain over the enum constants.
    let emitted = ol_clite_emit::emit_project(&project);
    assert!(
        emitted.source.contains("== Off") && emitted.source.contains("== Low"),
        "expected a ternary chain over enum constants:\n{}",
        emitted.source
    );

    // And the compiled C agrees cell by cell.
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("tl.csv"), "mode,level\nOff,5\nLow,5\nHigh,7\nOff,9\nHigh,2\n").unwrap();
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
    assert!(out.contains("[PASS] tl (ir)"), "{out}");
    assert!(out.contains("[PASS] tl (c )"), "case/fby C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
