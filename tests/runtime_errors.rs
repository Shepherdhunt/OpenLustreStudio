//! Runtime-error checks (`ol_cocospec_emit::rte`): the Kind 2 view proves
//! that no integer leaves its C type — no overflow at the width C computes
//! in, no narrow store that wraps, no division by zero, no index out of
//! bounds, no out-of-range real-to-integer conversion — in the context of
//! the root, so the mathematical integers Kind 2 reasons about are exactly
//! the values the simulator and the generated C compute.
//!
//! The checks themselves are tested without Kind 2; with Kind 2 installed
//! (`kind2` on PATH or `OPENLUSTRE_KIND2`, a solver on PATH or
//! `OPENLUSTRE_Z3`) they are proved and refuted for real. Set
//! `OPENLUSTRE_REQUIRE_KIND2=1` (CI does) to fail instead of skipping.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_cocospec_emit::kind2::{emit, emit_with, EmitOptions, Kind2Input};
use ol_cocospec_emit::rte::RteKind;
use ol_ir::{Equation, Expr, Local, NodeDef, NodeKind, Package, Port, Project, Type};

fn p(s: &str) -> Expr {
    ol_stdlib::parse_expr(s).unwrap_or_else(|e| panic!("parse `{s}` failed: {e}"))
}

fn ports(ps: &[(&str, Type)]) -> Vec<Port> {
    ps.iter().map(|(n, ty)| Port { name: (*n).into(), ty: ty.clone() }).collect()
}

fn node(name: &str, kind: NodeKind, inputs: &[(&str, Type)], outputs: &[(&str, Type)], locals: &[(&str, Type)], eqs: &[(&str, &str)]) -> NodeDef {
    NodeDef {
        name: name.into(),
        kind,
        inputs: ports(inputs),
        outputs: ports(outputs),
        locals: locals.iter().map(|(n, ty)| Local { name: (*n).into(), ty: ty.clone() }).collect(),
        equations: eqs
            .iter()
            .map(|(l, r)| Equation { lhs: l.split(',').map(|x| x.trim().to_string()).collect(), rhs: p(r) })
            .collect(),
        contract: None,
        diagram: Default::default(),
        probes: Vec::new(),
    }
}

fn project(nodes: Vec<NodeDef>, main: &str) -> Project {
    Project {
        name: "rte".into(),
        packages: vec![Package { name: "user".into(), nodes, ..Default::default() }],
        main: Some(main.into()),
        ..Default::default()
    }
}

/// `(kind, what)` of every check, in property order.
fn checks(input: &Kind2Input) -> Vec<(RteKind, String)> {
    input.checks.iter().map(|c| (c.kind, c.what.clone())).collect()
}

fn has(input: &Kind2Input, kind: RteKind, what: &str) -> bool {
    input.checks.iter().any(|c| c.kind == kind && c.what == what)
}

use Type::{Bool, Float64, Int16, Int32, Int8, Uint16};

#[test]
fn operations_are_checked_at_the_width_c_computes_them_in() {
    let top = node(
        "Top",
        NodeKind::Function,
        &[("a", Int8), ("b", Int8), ("p", Int32), ("q", Int32), ("u", Uint16), ("v", Uint16)],
        &[("avg", Int8), ("r", Int32), ("w", Int32), ("s", Int8), ("k", Int8)],
        &[],
        &[
            // int8 + int8 is computed in int: it cannot overflow, and the
            // halved sum always fits back into int8.
            ("avg", "(a + b) / 2"),
            // int32 * int32 can; dividing by a positive literal cannot.
            ("r", "p * q / 1000"),
            // uint16 * uint16 is computed in (signed) int: it can overflow.
            ("w", "int32(u * v)"),
            // Stored back into int8, a + b may not fit.
            ("s", "a + b"),
            // An explicit cast wraps by definition: not a runtime error.
            ("k", "int8(p)"),
        ],
    );
    let input = emit(&project(vec![top], "Top")).expect("view");
    assert!(has(&input, RteKind::Overflow, "p * q fits int32"), "{:?}", checks(&input));
    assert!(has(&input, RteKind::Overflow, "u * v fits int32"), "{:?}", checks(&input));
    assert!(has(&input, RteKind::Narrowing, "s = a + b fits int8"), "{:?}", checks(&input));
    assert_eq!(input.checks.len(), 3, "only what can fail is checked: {:?}", checks(&input));
    // Each check is a named property of the root.
    for c in &input.checks {
        assert!(input.text.contains(&format!("--%PROPERTY \"{}\"", c.name)), "{}", input.text);
    }
    // The root's integer inputs are assumed within their C types.
    assert!(input.text.contains("assert - 128 <= a and a <= 127;"), "{}", input.text);
    assert!(input.text.contains("assert 0 <= u and u <= 65535;"), "{}", input.text);
    assert!(input.notes.iter().any(|n| n.contains("3 runtime-error check(s)")), "{:?}", input.notes);
}

#[test]
fn divisions_indexes_and_conversions_are_checked_where_c_evaluates_them() {
    let top = node(
        "Top",
        NodeKind::Function,
        &[("a", Int16), ("b", Int16), ("i", Int32), ("arr", Type::Array { elem: Box::new(Int32), len: 4 }), ("f", Float64)],
        &[("d", Int32), ("g", Int32), ("e", Int32), ("e2", Int32), ("c", Int16)],
        &[],
        &[
            ("d", "int32(a / b)"),
            // Only evaluated when b <> 0: the check says so.
            ("g", "if b <> 0 then int32(a mod b) else 0"),
            ("e", "arr[i]"),
            // Clamped into range by construction: nothing to prove.
            ("e2", "arr[2]"),
            ("c", "int16(f)"),
        ],
    );
    let pr = project(vec![top], "Top");
    let input = emit(&pr).expect("view");
    let got = checks(&input);
    assert!(has(&input, RteKind::DivisionByZero, "b <> 0 in a / b"), "{got:?}");
    assert!(has(&input, RteKind::DivisionByZero, "b <> 0 in a mod b"), "{got:?}");
    assert!(has(&input, RteKind::IndexOutOfBounds, "i indexes arr (0..3)"), "{got:?}");
    assert!(has(&input, RteKind::Conversion, "f converts to int16"), "{got:?}");
    // int16 / int16 is computed in int: MIN / -1 cannot overflow there.
    assert!(!got.iter().any(|(k, _)| *k == RteKind::Overflow), "{got:?}");
    assert_eq!(got.len(), 4, "{got:?}");
    // The guarded remainder is checked under its guard.
    assert!(input.text.contains("b <> 0 => b <> 0"), "{}", input.text);
    // Proving contracts only: no checks, no input assumptions.
    let plain = emit_with(&pr, EmitOptions { runtime_errors: false }).expect("view");
    assert!(plain.checks.is_empty() && !plain.text.contains("assert"), "{}", plain.text);
}

#[test]
fn checks_in_called_operators_are_proved_per_call_instance_at_the_root() {
    let abs = node("Abs", NodeKind::Function, &[("x", Int32)], &[("y", Int32)], &[], &[("y", "if x < 0 then - x else x")]);
    let cnt = node(
        "Cnt",
        NodeKind::Operator,
        &[("inc", Int32)],
        &[("n", Int32)],
        &[],
        &[("n", "(0 -> pre n) + inc")],
    );
    let top = node(
        "Top",
        NodeKind::Operator,
        &[("c", Bool), ("x", Int8), ("z", Int32)],
        &[("y", Int32), ("m", Int32), ("k", Int32)],
        &[],
        &[
            // A call inside a branch is hoisted: C evaluates it every cycle.
            ("y", "if c then Cnt(1) else 0"),
            ("m", "Abs(int32(x)) + Abs(z)"),
            ("k", "Cnt(2)"),
        ],
    );
    let input = emit(&project(vec![abs, cnt, top], "Top")).expect("view");
    let at: Vec<(String, String, String)> =
        input.checks.iter().map(|c| (c.node.clone(), c.path.clone(), c.what.clone())).collect();
    for (node, path, what) in [
        ("Cnt", "Cnt#1", "(0 -> pre n) + inc fits int32"),
        ("Cnt", "Cnt#2", "(0 -> pre n) + inc fits int32"),
        ("Abs", "Abs#1", "- x fits int32"),
        ("Abs", "Abs#2", "- x fits int32"),
        ("Top", "", "__rtc1 + __rtc2 fits int32"),
    ] {
        assert!(at.iter().any(|(n, p, w)| n == node && p == path && w == what), "{node} {path} {what}: {at:#?}");
    }
    // Callees carry their checks out as extra outputs, after their own.
    assert!(input.text.contains("node Cnt(inc: int) returns (n: int; __rte0: bool);"), "{}", input.text);
    assert!(input.checks.iter().all(|c| input.check(&c.name).is_some()));
    assert!(input.checks[0].describe().starts_with("overflow in "), "{}", input.checks[0].describe());
}

#[test]
fn a_contract_calling_an_instrumented_function_keeps_its_signature() {
    let abs = node("Abs", NodeKind::Function, &[("x", Int32)], &[("y", Int32)], &[], &[("y", "if x < 0 then - x else x")]);
    let mut top = node("Top", NodeKind::Function, &[("x", Int32)], &[("y", Int32)], &[], &[("y", "Abs(x)")]);
    top.contract = Some("Top_contract".into());
    let mut pr = project(vec![abs, top], "Top");
    pr.packages[0].contracts.push(serde_json::json!({
        "name": "Top_contract",
        "inputs": [{"name": "x", "ty": {"kind": "Int32"}}],
        "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
        "guarantees": [{"name": "is_abs", "expr": serde_json::to_value(p("y = Abs(x)")).unwrap()}],
    }));
    let input = emit(&pr).expect("view");
    assert!(input.text.contains("function __ol_spec_Abs(x: int) returns (y: int);"), "{}", input.text);
    assert!(input.text.contains("y = __ol_spec_Abs(x)"), "{}", input.text);
    // The import names the node's own outputs, not the checks.
    assert!(input.text.contains("import Top_contract(x) returns (y);"), "{}", input.text);
}

// ---------------------------------------------------------------------------
// The simulator steps a call in an untaken branch, as the generated C does.

fn cc_available() -> bool {
    Command::new("cc").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

#[test]
fn a_call_in_an_untaken_branch_is_stepped_on_the_model_and_in_c() {
    let cnt = node("Cnt", NodeKind::Operator, &[("inc", Int32)], &[("n", Int32)], &[], &[("n", "(0 -> pre n) + inc")]);
    let top = node("Top", NodeKind::Operator, &[("c", Bool)], &[("y", Int32)], &[], &[("y", "if c then Cnt(1) else 0")]);
    let pr = project(vec![cnt, top], "Top");
    let csv = "c\nfalse\nfalse\ntrue\nfalse\ntrue\n";
    let ir = ol_sim::Sim::new(&pr, "Top").unwrap().run_csv(csv).unwrap().to_csv();
    let ys: Vec<&str> = ir.lines().skip(1).map(|l| l.rsplit(',').next().unwrap()).collect();
    assert_eq!(ys, ["0", "0", "3", "0", "5"], "Cnt counts every cycle:\n{ir}");
    if !cc_available() {
        eprintln!("skipping the C half: cc not available");
        return;
    }
    let bundle = ol_clite_emit::emit_project(&pr);
    let driver = ol_clite_emit::harness::emit_csv_driver(pr.find_node("Top").unwrap());
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("ol_rte_untaken_{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(dir.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(dir.join("driver.c"), &driver).unwrap();
    let exe = dir.join("drv");
    let cc = Command::new("cc")
        .args(["-std=c11", "-o"])
        .arg(&exe)
        .arg(dir.join("openlustre_generated.c"))
        .arg(dir.join("driver.c"))
        .arg(format!("-I{}", dir.display()))
        .output()
        .unwrap();
    assert!(cc.status.success(), "{}", String::from_utf8_lossy(&cc.stderr));
    let mut child = Command::new(&exe).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    use std::io::Write as _;
    child.stdin.as_mut().unwrap().write_all(csv.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8(out.stdout).unwrap(), ir, "the model and the generated C agree");
}

// ---------------------------------------------------------------------------
// With Kind 2: proved and refuted for real.

fn kind2() -> Option<ol_kind2::Kind2Options> {
    let on_path = |exe: &str| {
        std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(exe)).find(|p| p.is_file()))
    };
    let bin = std::env::var_os("OPENLUSTRE_KIND2").map(PathBuf::from).or_else(|| on_path("kind2"));
    let z3 = std::env::var_os("OPENLUSTRE_Z3").map(PathBuf::from).or_else(|| on_path("z3"));
    match (bin, z3) {
        (Some(bin), Some(z3)) => Some(ol_kind2::Kind2Options {
            kind2_binary: bin.display().to_string(),
            extra_args: vec!["--smt_solver".into(), "Z3".into(), "--z3_bin".into(), z3.display().to_string()],
            timeout_seconds: Some(60),
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

/// Each check's outcome, by what it checks.
fn prove(project: &Project, opts: ol_kind2::Kind2Options) -> Vec<(String, ol_kind2::Outcome)> {
    let input = emit(project).expect("view");
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("ol_rte_{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("model.lus");
    std::fs::write(&path, &input.text).unwrap();
    let main = project.main.clone();
    let result = ol_kind2::run_kind2(&path, &ol_kind2::Kind2Options { main_node: main, ..opts }).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result.errors.is_empty(), "Kind 2 rejected the view: {:?}\n{}", result.errors, input.text);
    result
        .properties
        .iter()
        .filter_map(|p| input.check(&p.name).map(|c| (c.describe(), p.outcome())))
        .collect()
}

fn outcome(results: &[(String, ol_kind2::Outcome)], what: &str) -> ol_kind2::Outcome {
    results.iter().find(|(d, _)| d == what).map(|(_, o)| *o).unwrap_or_else(|| panic!("no check `{what}` in {results:#?}"))
}

#[test]
fn overflows_are_found_and_safe_code_is_proved_by_kind2() {
    let Some(opts) = kind2() else { return };
    use ol_kind2::Outcome::{Fails, Holds};
    let top = node(
        "Top",
        NodeKind::Operator,
        &[("a", Int8), ("b", Int8), ("p", Int32), ("q", Int32), ("i", Int32), ("go", Bool)],
        &[("r", Int32), ("s", Int8), ("g", Int32), ("e", Int32), ("e2", Int32), ("n", Int32)],
        &[("arr", Type::Array { elem: Box::new(Int32), len: 4 })],
        &[
            ("r", "p * q / 1000"),
            ("s", "a + b"),
            ("g", "if b <> 0 then int32(a) / int32(b) else 0"),
            ("arr", "[p; q; 0; 1]"),
            ("e", "arr[i]"),
            ("e2", "arr[if i < 0 then 0 else if i > 3 then 3 else i]"),
            // A saturating counter never overflows, however long it runs.
            ("n", "0 -> if go and pre n < 1000 then pre n + 1 else pre n"),
        ],
    );
    let results = prove(&project(vec![top], "Top"), opts);
    assert_eq!(outcome(&results, "overflow in Top: p * q fits int32"), Fails);
    assert_eq!(outcome(&results, "narrowing in Top: s = a + b fits int8"), Fails);
    assert_eq!(outcome(&results, "division by zero in Top: int32(b) <> 0 in int32(a) / int32(b)"), Holds);
    assert_eq!(outcome(&results, "overflow in Top: int32(a) / int32(b) fits int32"), Holds);
    assert_eq!(outcome(&results, "index out of bounds in Top: i indexes arr (0..3)"), Fails);
    assert_eq!(
        outcome(&results, "index out of bounds in Top: if i < 0 then 0 else if i > 3 then 3 else i indexes arr (0..3)"),
        Holds
    );
    assert_eq!(outcome(&results, "overflow in Top: pre n + 1 fits int32"), Holds);
}

#[test]
fn a_callee_is_proved_safe_in_the_context_of_its_caller() {
    let Some(opts) = kind2() else { return };
    use ol_kind2::Outcome::{Fails, Holds};
    let abs = node("Abs", NodeKind::Function, &[("x", Int32)], &[("y", Int32)], &[], &[("y", "if x < 0 then - x else x")]);
    // From an int8 the negation cannot overflow; from any int32 it can
    // (Abs(-2147483648)).
    let top = node(
        "Top",
        NodeKind::Function,
        &[("x8", Int8), ("x", Int32)],
        &[("y", Int32), ("z", Int32)],
        &[],
        &[("y", "Abs(int32(x8))"), ("z", "Abs(x)")],
    );
    let results = prove(&project(vec![abs, top], "Top"), opts);
    assert_eq!(outcome(&results, "overflow in Abs#1: - x fits int32"), Holds);
    assert_eq!(outcome(&results, "overflow in Abs#2: - x fits int32"), Fails);
}
