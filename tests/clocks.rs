//! Boolean clocks (`when` / `when not` / `merge`) end to end: surface syntax,
//! clock inference and discipline errors, held-value simulation semantics,
//! and IR-vs-compiled-C trace equivalence.
//!
//! The flagship semantic claim: a clocked `->` counts ticks of ITS clock.
//! `cnt = 0 -> pre cnt + one` on clock `tick` produces 0 on the FIRST true
//! cycle of `tick` (whenever that happens), not on global cycle 0 — and the
//! generated C agrees cell by cell.

use std::path::PathBuf;
use std::process::Command;

use ol_ir::{Clock, Expr};

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

// --- Surface syntax: parse + format round-trip --------------------------------

#[test]
fn when_and_merge_parse_and_round_trip() {
    assert_eq!(p("x when c"), Expr::when(Expr::var("x"), "c", true));
    assert_eq!(p("x when not c"), Expr::when(Expr::var("x"), "c", false));
    assert_eq!(
        p("merge(c, a, b)"),
        Expr::merge("c", Expr::var("a"), Expr::var("b"))
    );

    // `when` binds looser than arithmetic, tighter than `->`.
    let e = p("0 -> x + 1 when c");
    match &e {
        Expr::Arrow { body, .. } => assert!(matches!(body.as_ref(), Expr::When { .. })),
        other => panic!("expected arrow, got {other:?}"),
    }

    // Round trips through the surface formatter.
    for src in [
        "x when c",
        "x when not c",
        "merge(c, a, b)",
        "0 -> x + 1 when c",
        "merge(c, x when c, (0 -> pre y) when not c)",
        "x when c when d",
    ] {
        let e = p(src);
        let text = ol_lustre_emit::format_expr(&e);
        assert_eq!(p(&text), e, "`{src}` formatted as `{text}` must re-parse identically");
    }

    // The Kind 2 view uses Lustre V6 merge-case syntax.
    let lus = ol_lustre_emit::format_expr_lustre(&p("merge(c, a, b)"));
    assert_eq!(lus, "merge c (true -> a) (false -> b)");

    // merge needs exactly (clock-variable, expr, expr).
    assert!(ol_stdlib::parse_expr("merge(c, a)").is_err());
    assert!(ol_stdlib::parse_expr("merge(1 + 2, a, b)").is_err());
    // The clock condition must be a variable name.
    assert!(ol_stdlib::parse_expr("x when (a and b)").is_err());
}

// --- The clocked-counter model --------------------------------------------------
//
//   gated_one = 1 when tick;                                  -- on clock tick
//   cnt_on    = 0 -> pre cnt_on + gated_one;                  -- inferred on tick
//   count     = merge(tick, cnt_on, (0 -> pre count) when not tick);
//
// count increments on true cycles of `tick` and holds through false ones.

fn gated_counter_model() -> serde_json::Value {
    let rhs = |s: &str| serde_json::to_value(p(s)).unwrap();
    serde_json::json!({
        "name": "clocks",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "GatedCounter",
                "kind": "Operator",
                "inputs": [{"name": "tick", "ty": {"kind": "Bool"}}],
                "outputs": [{"name": "count", "ty": {"kind": "Int32"}}],
                "locals": [
                    {"name": "gated_one", "ty": {"kind": "Int32"}},
                    {"name": "cnt_on", "ty": {"kind": "Int32"}}
                ],
                "equations": [
                    {"lhs": ["gated_one"], "rhs": rhs("1 when tick")},
                    {"lhs": ["cnt_on"], "rhs": rhs("0 -> pre cnt_on + gated_one")},
                    {"lhs": ["count"],
                     "rhs": rhs("merge(tick, cnt_on, (0 -> pre count) when not tick)")}
                ]
            }]
        }],
        "main": "GatedCounter"
    })
}

fn load(model: serde_json::Value) -> ol_ir::Project {
    serde_json::from_value(model).expect("model deserializes")
}

// --- Clock inference -------------------------------------------------------------

#[test]
fn clock_inference_assigns_equation_clocks_and_chains() {
    let project = load(gated_counter_model());
    let node = project.find_node("GatedCounter").unwrap();
    let info = ol_ir::infer_clocks(node);
    assert!(info.errors.is_empty(), "clean model: {:?}", info.errors);

    let keys: Vec<String> = info.equation_clocks.iter().map(|c| c.key()).collect();
    assert_eq!(keys, vec!["base/tick+", "base/tick+", "base"]);

    // The `->` in cnt_on counts ticks of `base/tick+`; the held-output arrow
    // counts the base clock. Both chains with pre/-> sites are tracked.
    let chain_keys: Vec<String> = info.chains.iter().map(|c| c.key()).collect();
    assert_eq!(chain_keys, vec!["base/tick+"]);
    assert!(info
        .site_clocks
        .values()
        .any(|c| matches!(c, Clock::Base)), "the held-count arrow is base-clocked");

    // The typechecker accepts the model.
    let report = ol_typecheck::check_project(&project);
    let errors: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.severity == ol_ir::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn clock_discipline_violations_are_loud() {
    // Mixing a sampled stream into a base-clocked sum: E0132.
    let mixed = serde_json::json!({
        "name": "bad", "packages": [{"name": "user", "nodes": [{
            "name": "Mixed", "kind": "Operator",
            "inputs": [
                {"name": "x", "ty": {"kind": "Int32"}},
                {"name": "c", "ty": {"kind": "Bool"}}
            ],
            "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
            "equations": [
                {"lhs": ["y"],
                 "rhs": serde_json::to_value(p("x + (x when c)")).unwrap()}
            ]
        }]}]
    });
    let report = ol_typecheck::check_project(&load(mixed));
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0132"),
        "expected E0132, got {:?}",
        report.diagnostics
    );

    // An output is base-clocked; defining it with a bare `when` is an error
    // that tells the user about merge.
    let clocked_output = serde_json::json!({
        "name": "bad2", "packages": [{"name": "user", "nodes": [{
            "name": "Out", "kind": "Operator",
            "inputs": [
                {"name": "x", "ty": {"kind": "Int32"}},
                {"name": "c", "ty": {"kind": "Bool"}}
            ],
            "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
            "equations": [
                {"lhs": ["y"], "rhs": serde_json::to_value(p("x when c")).unwrap()}
            ]
        }]}]
    });
    let report = ol_typecheck::check_project(&load(clocked_output));
    let e = report
        .diagnostics
        .iter()
        .find(|d| d.code == "E0132")
        .expect("clocked output must be rejected");
    assert!(e.message.contains("merge"), "should point at merge: {}", e.message);
    assert!(
        e.context.iter().any(|c| c.contains("equation 0")),
        "pinned to its equation: {:?}",
        e.context
    );

    // A non-bool clock: E0130.
    let int_clock = serde_json::json!({
        "name": "bad3", "packages": [{"name": "user", "nodes": [{
            "name": "IntClock", "kind": "Operator",
            "inputs": [{"name": "n", "ty": {"kind": "Int32"}}],
            "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
            "equations": [
                {"lhs": ["y"],
                 "rhs": serde_json::to_value(p("merge(n, 1, 2)")).unwrap()}
            ]
        }]}]
    });
    let report = ol_typecheck::check_project(&load(int_clock));
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0130"),
        "expected E0130, got {:?}",
        report.diagnostics
    );

    // Functions hold nothing: a clocked equation inside one is E0134.
    let fn_clock = serde_json::json!({
        "name": "bad4", "packages": [{"name": "user", "nodes": [{
            "name": "F", "kind": "Function",
            "inputs": [
                {"name": "x", "ty": {"kind": "Int32"}},
                {"name": "c", "ty": {"kind": "Bool"}}
            ],
            "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
            "locals": [{"name": "l", "ty": {"kind": "Int32"}}],
            "equations": [
                {"lhs": ["l"], "rhs": serde_json::to_value(p("x when c")).unwrap()},
                {"lhs": ["y"], "rhs": serde_json::to_value(p("merge(c, l, 0 when not c)")).unwrap()}
            ]
        }]}]
    });
    let report = ol_typecheck::check_project(&load(fn_clock));
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0134"),
        "expected E0134, got {:?}",
        report.diagnostics
    );
}

// --- Simulation: held values + first-tick arrows ---------------------------------

#[test]
fn clocked_counter_counts_ticks_and_holds_between_them() {
    let tmp = make_tempdir("clock_sim");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&gated_counter_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();
    let mut sim = ol_sim::Sim::new(&project, "GatedCounter").unwrap();

    // tick:   f      t      t      f      t
    // cnt_on: 0(hold) 0(init!) 1     1(hold) 2
    // count:  0      0      1      1      2
    let trace = sim
        .run_csv_full("tick\nfalse\ntrue\ntrue\nfalse\ntrue\n")
        .unwrap();
    let csv = trace.to_csv();
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines[0], "cycle,tick,gated_one,cnt_on,count");
    assert_eq!(lines[1], "0,false,0,0,0");
    // THE distinguishing cycle: the first tick arrives at global cycle 1,
    // and the clocked `0 -> …` must still take its init branch (0), not the
    // body a global initialized-flag would choose.
    assert_eq!(lines[2], "1,true,1,0,0");
    assert_eq!(lines[3], "2,true,1,1,1");
    assert_eq!(lines[4], "3,false,1,1,1", "holds through the off cycle");
    assert_eq!(lines[5], "4,true,1,2,2");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn clock_variables_computed_by_later_equations_still_gate_correctly() {
    // The clock is a local defined AFTER its consumers — dependency order
    // must schedule it first because clock conditions are same-cycle reads.
    let rhs = |s: &str| serde_json::to_value(p(s)).unwrap();
    let model = serde_json::json!({
        "name": "fwd", "packages": [{"name": "user", "nodes": [{
            "name": "Fwd", "kind": "Operator",
            "inputs": [{"name": "level", "ty": {"kind": "Int32"}}],
            "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
            "locals": [
                {"name": "high", "ty": {"kind": "Bool"}},
                {"name": "sampled", "ty": {"kind": "Int32"}}
            ],
            "equations": [
                {"lhs": ["sampled"], "rhs": rhs("level when high")},
                {"lhs": ["y"], "rhs": rhs("merge(high, sampled, (0 -> pre y) when not high)")},
                {"lhs": ["high"], "rhs": rhs("level > 10")}
            ]
        }]}],
        "main": "Fwd"
    });
    let project = load(model);
    let mut sim = ol_sim::Sim::new(&project, "Fwd").unwrap();
    let trace = sim.run_csv("level\n5\n20\n7\n30\n").unwrap();
    let csv = trace.to_csv();
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines[1], "0,0", "below threshold: held initial 0");
    assert_eq!(lines[2], "1,20", "sampled on the high cycle");
    assert_eq!(lines[3], "2,20", "holds the last sample");
    assert_eq!(lines[4], "3,30");
}

// --- Generated C: the same trace, cell by cell -----------------------------------

#[test]
fn clocked_traces_match_between_ir_and_compiled_c() {
    let tmp = make_tempdir("clock_c");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&gated_counter_model()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    // Off-start, on-start, bursts, long holds — the patterns that expose
    // wrong first-tick handling and missing holds.
    std::fs::write(
        scen.join("gating.csv"),
        "tick\nfalse\ntrue\ntrue\nfalse\ntrue\nfalse\nfalse\ntrue\ntrue\ntrue\nfalse\n",
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
    assert!(out.contains("[PASS] gating (ir)"), "{out}");
    assert!(out.contains("[PASS] gating (c )"), "clocked C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}
