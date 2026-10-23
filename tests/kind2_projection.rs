//! The Kind 2 view (`ol_cocospec_emit::kind2`): the one file the prover
//! reads, and the claim that it means what the simulator and the generated C
//! execute.
//!
//! * Clock elimination (`ol_ir::declock`) is checked against the clocked
//!   original by simulation, cycle by cycle.
//! * The view's shape: contracts imported in node headers, `--%MAIN;` in the
//!   root, C integer semantics through helper functions, iterators unrolled,
//!   Kind 2 keywords refused.
//! * With Kind 2 installed (`kind2` on PATH or `OPENLUSTRE_KIND2`, a solver on
//!   PATH or `OPENLUSTRE_Z3`), the examples are proved for real. Set
//!   `OPENLUSTRE_REQUIRE_KIND2=1` (CI does) to fail instead of skipping when
//!   it is missing.

use std::path::PathBuf;

use ol_ir::{
    ActivationBranch, ActivationDef, BinOp, Equation, Expr, Local, NodeDef, NodeKind, Package, Port, Project, Type,
};

fn p(s: &str) -> Expr {
    ol_stdlib::parse_expr(s).unwrap_or_else(|e| panic!("parse `{s}` failed: {e}"))
}

fn port(name: &str, ty: Type) -> Port {
    Port { name: name.into(), ty }
}

fn eq(lhs: &str, rhs: Expr) -> Equation {
    Equation { lhs: vec![lhs.into()], rhs }
}

fn operator(name: &str, inputs: Vec<Port>, outputs: Vec<Port>, locals: Vec<(&str, Type)>, equations: Vec<Equation>) -> NodeDef {
    NodeDef {
        name: name.into(),
        kind: NodeKind::Operator,
        inputs,
        outputs,
        locals: locals.into_iter().map(|(n, ty)| Local { name: n.into(), ty }).collect(),
        equations,
        contract: None,
        diagram: Default::default(),
        probes: Vec::new(),
    }
}

fn project(nodes: Vec<NodeDef>, main: &str) -> Project {
    Project {
        name: "k2".into(),
        packages: vec![Package { name: "user".into(), nodes, ..Default::default() }],
        main: Some(main.into()),
        ..Default::default()
    }
}

/// A deterministic pseudo-random bool/int input sequence.
fn inputs(header: &[(&str, &str)], cycles: usize, seed: u64) -> String {
    let mut s = seed;
    let mut next = || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 33) as i64
    };
    let mut out = header.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(",");
    out.push('\n');
    for _ in 0..cycles {
        let row: Vec<String> = header
            .iter()
            .map(|(_, ty)| if *ty == "bool" { (next() % 2 == 0).to_string() } else { (next() % 21 - 10).to_string() })
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

fn column(csv: &str, name: &str) -> Vec<String> {
    let rows: Vec<&str> = csv.lines().collect();
    let col = rows[0].split(',').position(|c| c == name).unwrap_or_else(|| panic!("no column {name} in {}", rows[0]));
    rows[1..].iter().map(|r| r.split(',').nth(col).unwrap().to_string()).collect()
}

/// Simulate `node` in the clocked project and in its declocked form on the
/// same inputs: every output must agree on every cycle.
fn declock_agrees(project: &Project, node: &str, input: &str) -> String {
    let tc = ol_typecheck::check_project(project);
    assert!(!tc.has_errors(), "{:?}", tc.errors().collect::<Vec<_>>());
    let declocked = ol_ir::declock_project(project).expect("declocks");
    let rewritten = declocked.find_node(node).unwrap();
    for e in &rewritten.equations {
        e.rhs.visit(|x| assert!(!matches!(x, Expr::When { .. } | Expr::Merge { .. }), "clock left in {x:?}"));
    }
    let clocked = ol_sim::Sim::new(project, node).unwrap().run_csv(input).unwrap().to_csv();
    let flat = ol_sim::Sim::new(&declocked, node).unwrap().run_csv(input).unwrap().to_csv();
    for o in &project.find_node(node).unwrap().outputs {
        assert_eq!(column(&clocked, &o.name), column(&flat, &o.name), "output `{}` differs\n{clocked}\n{flat}", o.name);
    }
    clocked
}

#[test]
fn declocked_gated_counter_matches_the_clocked_original() {
    let n = operator(
        "GatedCounter",
        vec![port("tick", Type::Bool)],
        vec![port("count", Type::Int32)],
        vec![("gated_one", Type::Int32), ("cnt_on", Type::Int32)],
        vec![
            eq("gated_one", p("1 when tick")),
            eq("cnt_on", p("0 -> pre cnt_on + gated_one")),
            eq("count", p("merge(tick, cnt_on, (0 -> pre count) when not tick)")),
        ],
    );
    let pr = project(vec![n], "GatedCounter");
    let csv = declock_agrees(&pr, "GatedCounter", "tick\nfalse\nfalse\ntrue\ntrue\nfalse\ntrue\n");
    // The clocked `->` counts ticks of its clock: 0 on the first true cycle.
    assert_eq!(column(&csv, "count"), ["0", "0", "0", "1", "1", "2"]);
}

#[test]
fn declocked_nested_clocks_and_clocked_calls_match_the_original() {
    let tick = operator(
        "Tick",
        vec![port("inc", Type::Int32)],
        vec![port("c", Type::Int32)],
        vec![],
        vec![eq("c", p("(0 -> pre c) + inc"))],
    );
    let n = operator(
        "Nested",
        vec![port("c", Type::Bool), port("d", Type::Bool), port("v", Type::Int32)],
        vec![port("o", Type::Int32), port("t", Type::Int32), port("first", Type::Int32)],
        vec![
            ("dc", Type::Bool),
            ("vcd", Type::Int32),
            ("acc", Type::Int32),
            ("inner", Type::Int32),
            ("tcd", Type::Int32),
            ("tin", Type::Int32),
            ("f", Type::Int32),
            ("fin", Type::Int32),
        ],
        vec![
            eq("dc", p("d when c")),
            eq("vcd", p("v when c when dc")),
            // Accumulates v on cycles where c and d both hold.
            eq("acc", p("(0 -> pre acc) + vcd")),
            eq("inner", p("merge(dc, acc, (0 -> pre inner) when not dc)")),
            eq("o", p("merge(c, inner, (0 -> pre o) when not c)")),
            // A stateful call on the nested clock steps only there.
            eq("tcd", p("Tick(vcd)")),
            eq("tin", p("merge(dc, tcd, (0 -> pre tin) when not dc)")),
            eq("t", p("merge(c, tin, (0 -> pre t) when not c)")),
            // `->` on the nested clock: v at the first c-and-d cycle, held.
            eq("f", p("vcd -> pre f")),
            eq("fin", p("merge(dc, f, (0 -> pre fin) when not dc)")),
            eq("first", p("merge(c, fin, (0 -> pre first) when not c)")),
        ],
    );
    let pr = project(vec![n, tick], "Nested");
    let input = inputs(&[("c", "bool"), ("d", "bool"), ("v", "int")], 80, 7);
    let csv = declock_agrees(&pr, "Nested", &input);
    // Sanity: the accumulator really moved, and matches Tick's running sum.
    assert_eq!(column(&csv, "o"), column(&csv, "t"));
    assert!(column(&csv, "o").iter().any(|v| v != "0"));

    // Kind 2 agrees for every input: the condact-wrapped Tick steps exactly
    // when the inline accumulator does.
    let Some(opts) = kind2() else { return };
    let mut pr = pr;
    pr.packages[0].nodes[0].contract = Some("Nested_contract".into());
    pr.packages[0].contracts.push(serde_json::json!({
        "name": "Nested_contract",
        "inputs": [{"name": "c", "ty": {"kind": "Bool"}}, {"name": "d", "ty": {"kind": "Bool"}}, {"name": "v", "ty": {"kind": "Int32"}}],
        "outputs": [{"name": "o", "ty": {"kind": "Int32"}}, {"name": "t", "ty": {"kind": "Int32"}}, {"name": "first", "ty": {"kind": "Int32"}}],
        "guarantees": [
            {"name": "same_sum", "expr": serde_json::to_value(p("o = t")).unwrap()},
            {"name": "holds_off_clock", "expr": serde_json::to_value(p("true -> not (c and d) => o = pre o")).unwrap()},
        ],
    }));
    let (text, result) = prove(&pr, "Nested", opts);
    assert_eq!(result.properties.len(), 2, "{:?}", result.properties);
    for p in &result.properties {
        assert_eq!(p.outcome(), ol_kind2::Outcome::Holds, "{} is {}\n{text}", p.label, p.status);
    }
}

#[test]
fn declocked_activation_branches_freeze_like_the_original() {
    let tick = operator(
        "Tick",
        vec![port("inc", Type::Int32)],
        vec![port("c", Type::Int32)],
        vec![],
        vec![eq("c", Expr::bin(BinOp::Add, Expr::arrow(Expr::int_lit(0), Expr::pre(Expr::var("c"))), Expr::var("inc")))],
    );
    let call_tick = Expr::call("Tick", vec![Expr::int_lit(1)]);
    let act = ActivationDef {
        name: "Mode".into(),
        outputs: vec![port("k", Type::Int32), port("t", Type::Int32), port("h", Type::Int32)],
        branches: vec![ActivationBranch {
            name: "On".into(),
            condition: Expr::var("sel"),
            equations: vec![
                eq("k", Expr::bin(BinOp::Add, Expr::arrow(Expr::int_lit(0), Expr::pre(Expr::var("k"))), Expr::int_lit(1))),
                eq("t", call_tick.clone()),
                eq("h", Expr::var("x")),
            ],
        }],
        else_equations: vec![
            eq("k", Expr::bin(BinOp::Sub, Expr::arrow(Expr::int_lit(100), Expr::pre(Expr::var("k"))), Expr::int_lit(1))),
            eq("t", call_tick),
            eq("h", Expr::call("last", vec![Expr::var("h"), Expr::int_lit(-1)])),
        ],
        owner: "Frozen".into(),
    };
    let owner = operator(
        "Frozen",
        vec![port("sel", Type::Bool), port("x", Type::Int32)],
        vec![port("k", Type::Int32), port("t", Type::Int32), port("h", Type::Int32)],
        vec![],
        vec![],
    );
    let mut pr = project(vec![owner, tick], "Frozen");
    pr.packages[0].activations.push(act);
    pr.lower_activations().expect("lowers");
    let input = "sel,x\ntrue,5\ntrue,6\nfalse,7\nfalse,8\ntrue,9\nfalse,10\ntrue,11\ntrue,12\n";
    let csv = declock_agrees(&pr, "Frozen", input);
    assert_eq!(column(&csv, "k"), ["1", "2", "99", "98", "3", "97", "4", "5"]);
    assert_eq!(column(&csv, "t"), ["1", "2", "1", "2", "3", "3", "4", "5"]);
    let random = inputs(&[("sel", "bool"), ("x", "int")], 120, 11);
    declock_agrees(&pr, "Frozen", &random);

    // The Kind 2 view carries no clocks: branch calls run under condact.
    let view = ol_cocospec_emit::kind2::emit(&pr).expect("view").text;
    assert!(!view.contains(" when ") && !view.contains("merge "), "{view}");
    assert!(view.contains("condact(__ck"), "{view}");
}

fn release_logic() -> Project {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/release_logic/model/release_logic.json");
    ol_ir::load_project(&path).expect("example loads")
}

#[test]
fn the_view_is_one_self_contained_kind2_file() {
    let view = ol_cocospec_emit::kind2::emit(&release_logic()).expect("view").text;
    // Declared once, with its contract imported in the header, the entry
    // point marked inside its body, and the contract defined in the file.
    assert_eq!(view.matches("node ReleaseLogic(").count(), 1, "{view}");
    let sig = view.find("node ReleaseLogic(").unwrap();
    let after = &view[sig..];
    let second = after.lines().nth(1).unwrap();
    assert!(
        second.starts_with("(*@contract import ReleaseLogic_contract(master_arm, station_selected, consent, fault_present, release_request) returns (release_cmd, inhibit); *)"),
        "{second}"
    );
    assert!(after.contains("let\n  --%MAIN;\n"), "{view}");
    assert!(view.contains("contract ReleaseLogic_contract("), "{view}");
    assert!(!view.contains("@*)") && !view.contains("see node body"), "{view}");
    // The plain projection marks the entry point the same (valid) way.
    let plain = ol_lustre_emit::emit_project(&release_logic());
    assert!(plain.contains("let\n  --%MAIN;\n") && !plain.contains("entry:"), "{plain}");
}

#[test]
fn the_view_follows_c_integer_semantics() {
    let n = operator(
        "Arith",
        vec![port("a", Type::Int32), port("b", Type::Int32), port("x", Type::Float64), port("xs", Type::Array { elem: Box::new(Type::Int32), len: 3 })],
        vec![
            port("q", Type::Int32),
            port("r", Type::Int32),
            port("i", Type::Int32),
            port("n", Type::Uint8),
            port("y", Type::Float64),
            port("s", Type::Int32),
            port("m", Type::Array { elem: Box::new(Type::Int32), len: 3 }),
            port("h", Type::Float64),
        ],
        vec![],
        vec![
            eq("q", p("a / b")),
            eq("r", p("a mod b")),
            eq("i", p("int32(x)")),
            eq("n", p("uint8(a)")),
            eq("y", p("float64(a) / 2.0")),
            eq("s", p("fold(Add2, 0, xs)")),
            eq("m", p("map(Add2, xs, xs)")),
            eq("h", p("x / 2.0")),
        ],
    );
    let add2 = NodeDef {
        kind: NodeKind::Function,
        ..operator("Add2", vec![port("u", Type::Int32), port("w", Type::Int32)], vec![port("z", Type::Int32)], vec![], vec![eq("z", p("u + w"))])
    };
    let pr = project(vec![n, add2], "Arith");
    let tc = ol_typecheck::check_project(&pr);
    assert!(!tc.has_errors(), "{:?}", tc.errors().collect::<Vec<_>>());
    // The contract view (with runtime-error checks, calls are hoisted to
    // equations of their own).
    let contracts_only = ol_cocospec_emit::kind2::EmitOptions { runtime_errors: false };
    let view = ol_cocospec_emit::kind2::emit_with(&pr, contracts_only).expect("view");
    let t = &view.text;
    assert!(t.contains("q = __ol_div(a, b);") && t.contains("r = __ol_mod(a, b);"), "{t}");
    assert!(t.contains("i = __ol_trunc(x);"), "{t}");
    assert!(t.contains("n = __ol_wrap_uint8(a);") && t.contains("y = x mod 256;"), "{t}");
    assert!(t.contains("y = real(a) / 2.0;"), "{t}");
    // Real division stays real division.
    assert!(t.contains("h = x / 2.0;"), "{t}");
    // Iterators unrolled over the static length.
    assert!(t.contains("s = Add2(Add2(Add2(0, xs[0]), xs[1]), xs[2]);"), "{t}");
    assert!(t.contains("m = [Add2(xs[0], xs[0]), Add2(xs[1], xs[1]), Add2(xs[2], xs[2])];"), "{t}");
    for helper in ["function __ol_div(", "function __ol_mod(", "function __ol_trunc(", "function __ol_wrap_uint8("] {
        assert!(t.contains(helper), "{helper} missing:\n{t}");
    }
    assert!(view.notes.iter().any(|n| n.contains("mathematical integers")), "{:?}", view.notes);
    assert!(view.notes.iter().any(|n| n.contains("exact reals")), "{:?}", view.notes);
}

#[test]
fn kind2_keywords_are_refused_with_a_rename_hint() {
    let n = operator("K", vec![port("check", Type::Bool)], vec![port("y", Type::Bool)], vec![], vec![eq("y", p("check"))]);
    let errs = ol_cocospec_emit::kind2::emit(&project(vec![n], "K")).expect_err("keyword");
    assert!(errs[0].contains("`check`") && errs[0].contains("rename"), "{errs:?}");
}

// --- Real proofs ------------------------------------------------------------------

/// Kind 2 and solver arguments, or `None` (skip) when not installed.
fn kind2() -> Option<ol_kind2::Kind2Options> {
    let on_path = |exe: &str| {
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(exe)).find(|f| f.is_file()))
    };
    let bin = std::env::var_os("OPENLUSTRE_KIND2").map(PathBuf::from).or_else(|| on_path("kind2"));
    let z3 = std::env::var_os("OPENLUSTRE_Z3").map(PathBuf::from).or_else(|| on_path("z3"));
    match (bin, z3) {
        (Some(bin), Some(z3)) => Some(ol_kind2::Kind2Options {
            kind2_binary: bin.display().to_string(),
            extra_args: vec!["--smt_solver".into(), "Z3".into(), "--z3_bin".into(), z3.display().to_string()],
            timeout_seconds: Some(120),
            ..Default::default()
        }),
        _ => {
            assert!(
                std::env::var_os("OPENLUSTRE_REQUIRE_KIND2").is_none(),
                "OPENLUSTRE_REQUIRE_KIND2 is set but kind2 / z3 were not found"
            );
            eprintln!("kind2 or z3 not found: skipping the real proof");
            None
        }
    }
}

/// Prove the contracts of the view. Runtime errors are off: these models
/// count without bound on purpose (`tests/runtime_errors.rs` proves those).
fn prove(project: &Project, root: &str, opts: ol_kind2::Kind2Options) -> (String, ol_kind2::Kind2Result) {
    let contracts_only = ol_cocospec_emit::kind2::EmitOptions { runtime_errors: false };
    let input = ol_cocospec_emit::kind2::emit_with(project, contracts_only).expect("view");
    let stamp = openlustre_integration_tests::unique_stamp();
    let dir = std::env::temp_dir().join(format!("ol_kind2_projection_{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("model.lus");
    std::fs::write(&path, &input.text).unwrap();
    let result = ol_kind2::run_kind2(&path, &ol_kind2::Kind2Options { main_node: Some(root.into()), ..opts }).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result.errors.is_empty(), "Kind 2 rejected the view: {:?}\n{}", result.errors, input.text);
    (input.text, result)
}

#[test]
fn release_logic_is_proved_by_kind2() {
    let Some(opts) = kind2() else { return };
    let (text, result) = prove(&release_logic(), "ReleaseLogic", opts);
    assert!(result.properties.len() >= 10, "{:?}", result.properties);
    for p in &result.properties {
        assert_eq!(p.outcome(), ol_kind2::Outcome::Holds, "{} is {}\n{text}", p.label, p.status);
    }
    // Every mode is reachable, and one is always active.
    assert!(result.properties.iter().any(|p| p.source.as_deref() == Some("OneModeActive")));
    assert_eq!(result.properties.iter().filter(|p| p.source.as_deref() == Some("NonVacuityCheck")).count(), 4);
}

#[test]
fn clocked_activation_is_proved_by_kind2_and_a_wrong_claim_is_falsified() {
    let Some(opts) = kind2() else { return };
    // Counter2: a resettable up-counter as a clocked decision tree.
    let act = ActivationDef {
        name: "Count".into(),
        outputs: vec![port("n", Type::Int32)],
        branches: vec![
            ActivationBranch { name: "Reset".into(), condition: Expr::var("reset"), equations: vec![eq("n", Expr::int_lit(0))] },
            ActivationBranch {
                name: "Up".into(),
                condition: Expr::var("up"),
                equations: vec![eq("n", Expr::bin(BinOp::Add, Expr::call("last", vec![Expr::var("n")]), Expr::int_lit(1)))],
            },
        ],
        else_equations: vec![eq("n", Expr::call("last", vec![Expr::var("n")]))],
        owner: "Counter2".into(),
    };
    let mut owner = operator("Counter2", vec![port("up", Type::Bool), port("reset", Type::Bool)], vec![port("n", Type::Int32)], vec![], vec![]);
    owner.contract = Some("Counter2_contract".into());
    let mut pr = project(vec![owner], "Counter2");
    pr.packages[0].activations.push(act);
    let contract = |extra: serde_json::Value| {
        let g = |name: &str, e: &str| serde_json::json!({"name": name, "expr": serde_json::to_value(p(e)).unwrap()});
        let mut guarantees = vec![
            g("never_negative", "n >= 0"),
            g("reset_clears", "reset => n = 0"),
            g("counts_up", "true -> (up and not reset) => n = pre n + 1"),
            g("holds", "true -> (not up and not reset) => n = pre n"),
        ];
        if !extra.is_null() {
            guarantees.push(extra);
        }
        serde_json::json!({
            "name": "Counter2_contract",
            "inputs": [{"name": "up", "ty": {"kind": "Bool"}}, {"name": "reset", "ty": {"kind": "Bool"}}],
            "outputs": [{"name": "n", "ty": {"kind": "Int32"}}],
            "guarantees": guarantees,
        })
    };
    pr.packages[0].contracts.push(contract(serde_json::Value::Null));
    pr.lower_activations().expect("lowers");
    let (text, result) = prove(&pr, "Counter2", opts.clone());
    assert!(text.contains("condact") || text.contains("__ck"), "{text}");
    for p in &result.properties {
        assert_eq!(p.outcome(), ol_kind2::Outcome::Holds, "{} is {}\n{text}", p.label, p.status);
    }
    assert_eq!(result.properties.len(), 4);

    // A claim the model does not meet: Kind 2 finds the counterexample, and
    // replaying it in the simulator shows the same violation.
    let mut wrong = pr.clone();
    let bad = serde_json::json!({"name": "bounded", "expr": serde_json::to_value(p("n < 3")).unwrap()});
    wrong.packages[0].contracts = vec![contract(bad)];
    let (_, result) = prove(&wrong, "Counter2", opts);
    let bounded = result.properties.iter().find(|p| p.label.ends_with("bounded")).expect("bounded reported");
    assert_eq!(bounded.outcome(), ol_kind2::Outcome::Fails);
    let cex = ol_kind2::counterexample_streams(bounded.counterexample.as_ref().unwrap()).unwrap();
    let col = |name: &str| cex.streams.iter().find(|s| s.name == name).unwrap().values.clone();
    let (up, reset, n) = (col("up"), col("reset"), col("n"));
    let mut csv = String::from("up,reset\n");
    for c in 0..cex.cycles {
        csv.push_str(&format!("{},{}\n", up[c], reset[c]));
    }
    let trace = ol_sim::Sim::new(&pr, "Counter2").unwrap().run_csv(&csv).unwrap().to_csv();
    assert_eq!(column(&trace, "n"), n, "the simulator replays Kind 2's counterexample");
    assert!(n.last().unwrap().parse::<i64>().unwrap() >= 3);
}
