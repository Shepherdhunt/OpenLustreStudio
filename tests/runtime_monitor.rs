//! Phase 6 runtime-monitor wiring: a model whose implementation lies to its
//! contract some cycles. Both the IR simulator and the generated C-Lite
//! `_monitor_check` must flag the same violations at the same cycles, and
//! their CSV traces (cycle + outputs + active_mode + violations) must agree
//! byte-for-byte.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_ir::{Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};
use ol_sim::Sim;

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `node FaultyLatch(set: bool, fault: bool) returns (q: bool)`
///   q = set or fault;             — implementation lies: latches under fault
/// contract:
///   guarantee q_implies_set: q => set
///
/// Contract is statically well-formed; the IMPLEMENTATION violates it whenever
/// `fault and not set`.
fn build_faulty_latch_project() -> Project {
    let inputs = vec![
        Port { name: "set".into(), ty: Type::Bool },
        Port { name: "fault".into(), ty: Type::Bool },
    ];
    let outputs = vec![Port { name: "q".into(), ty: Type::Bool }];

    let node = NodeDef {
        name: "FaultyLatch".into(),
        kind: NodeKind::Operator,
        inputs: inputs.clone(),
        outputs: outputs.clone(),
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["q".into()],
            rhs: Expr::or(Expr::var("set"), Expr::var("fault")),
        }],
        contract: Some("FaultyLatch_contract".into()),
        diagram: Default::default(),
            probes: vec![],
    };

    // Contract: one named guarantee.
    let contract = serde_json::json!({
        "name": "FaultyLatch_contract",
        "inputs": inputs,
        "outputs": outputs,
        "ghost_vars": [],
        "assumptions": [],
        "guarantees": [
            { "name": "q_implies_set", "expr": Expr::implies(Expr::var("q"), Expr::var("set")) }
        ],
        "modes": [],
        "imports": []
    });

    Project {
        name: "faulty_latch".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            contracts: vec![contract],
            ..Default::default()
        }],
        main: Some("FaultyLatch".into()),
        ..Default::default()
    }
}

const INPUT_CSV: &str = "\
set,fault
true,false
false,true
false,false
true,false
false,true
true,true
";

#[test]
fn ir_simulator_populates_violations_in_trace() {
    let project = build_faulty_latch_project();
    let mut sim = Sim::new(&project, "FaultyLatch").unwrap();
    let trace = sim.run_csv(INPUT_CSV).unwrap();

    // Cycles 1 and 4 violate (`fault and not set` => q latches true but
    // `q => set` requires set). Cycle 5 has both set and fault — q=true and
    // set=true, so no violation.
    let violated_cycles: Vec<usize> = trace.violations.iter().map(|(_, c)| *c).collect();
    assert_eq!(violated_cycles, vec![1, 4]);
    assert!(
        trace
            .violations
            .iter()
            .all(|(label, _)| label == "q_implies_set")
    );

    // The CSV must include the `violations` column with `<none>` where clean
    // and the guarantee label where violated.
    let csv = trace.to_csv();
    assert!(csv.contains("violations"));
    let lines: Vec<&str> = csv.lines().collect();
    // line 0 is header; cycles start at line 1.
    assert!(lines[1].ends_with("<none>"), "cycle 0 should be clean: {}", lines[1]);
    assert!(
        lines[2].ends_with("q_implies_set"),
        "cycle 1 should flag the guarantee: {}",
        lines[2]
    );
}

#[test]
fn ir_simulator_and_generated_c_monitor_agree_on_violations() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }

    let project = build_faulty_latch_project();

    // 1. IR simulator trace, including the new violations column.
    let ir_csv = {
        let mut sim = Sim::new(&project, "FaultyLatch").unwrap();
        sim.run_csv(INPUT_CSV).unwrap().to_csv()
    };

    // 2. Emit C-Lite model + monitor + a driver wired to that monitor.
    let bundle = ol_clite_emit::emit_project(&project);
    let monitor = ol_clite_emit::monitor::emit_monitors(&project);
    let entry = project.find_node("FaultyLatch").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver_with_monitor(
        entry,
        Some("FaultyLatch_contract"),
    );

    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(tmp.join("openlustre_monitors.h"), &monitor.header).unwrap();
    std::fs::write(tmp.join("openlustre_monitors.c"), &monitor.source).unwrap();
    std::fs::write(tmp.join("driver.c"), &driver).unwrap();
    let exe = tmp.join("monitor_driver");

    let cc = Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Wno-unused-but-set-variable",
            "-Wno-unused-variable",
            "-Werror",
            "-o",
        ])
        .arg(&exe)
        .arg(tmp.join("openlustre_generated.c"))
        .arg(tmp.join("openlustre_monitors.c"))
        .arg(tmp.join("driver.c"))
        .arg(format!("-I{}", tmp.display()))
        .output()
        .expect("cc runs");
    if !cc.status.success() {
        let stderr = String::from_utf8_lossy(&cc.stderr).to_string();
        let _ = std::fs::remove_dir_all(&tmp);
        panic!(
            "cc failed:\n{stderr}\n--- monitor.c ---\n{}\n--- driver.c ---\n{}",
            monitor.source, driver
        );
    }

    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("driver runs");
    use std::io::Write as _;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(INPUT_CSV.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("driver finishes");
    let success = out.status.success();
    let c_csv = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(success, "driver crashed");
    if ir_csv != c_csv {
        panic!("trace mismatch\n--- IR ---\n{ir_csv}\n--- C ---\n{c_csv}");
    }
}

fn make_tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_mon_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Temporal contracts through both monitors. `Acc` accumulates `x` (reset to
/// 0 by `reset`); its contract uses ghost variables (`first`, `prev = 0 ->
/// pre y`), an assumption, a temporal guarantee, and a deliberately false
/// mode (`Lying`: y never changes). The expected verdicts are worked out by
/// hand per cycle — they need real `pre` / `->` state and ghost variables,
/// which the old stateless monitors could not evaluate — and the compiled C
/// monitor must report byte-for-byte the same.
fn build_accumulator_project() -> Project {
    let node = NodeDef {
        name: "Acc".into(),
        kind: NodeKind::Operator,
        inputs: vec![
            Port { name: "x".into(), ty: Type::Int32 },
            Port { name: "reset".into(), ty: Type::Bool },
        ],
        outputs: vec![Port { name: "y".into(), ty: Type::Int32 }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["y".into()],
            rhs: ol_stdlib::parse_expr("if reset then 0 else (0 -> pre y) + x").unwrap(),
        }],
        contract: Some("AccC".into()),
        diagram: Default::default(),
        probes: vec![],
    };
    let e = |s: &str| serde_json::to_value(ol_stdlib::parse_expr(s).unwrap()).unwrap();
    let contract = serde_json::json!({
        "name": "AccC",
        "inputs": [{"name":"x","ty":{"kind":"Int32"}}, {"name":"reset","ty":{"kind":"Bool"}}],
        "outputs": [{"name":"y","ty":{"kind":"Int32"}}],
        "ghost_vars": [
            {"name":"first","ty":{"kind":"Bool"},"definition": e("true -> false")},
            {"name":"prev","ty":{"kind":"Int32"},"definition": e("0 -> pre y")}
        ],
        "assumptions": [{"name":"nonneg","expr": e("x >= 0")}],
        "guarantees": [{"name":"grows","expr": e("not reset => y >= prev")}],
        "modes": [
            {"name":"Reset","requires":[e("reset")],"ensures":[e("y = 0")]},
            {"name":"First","requires":[e("first"), e("not reset")],"ensures":[e("y = x")]},
            {"name":"Lying","requires":[e("not reset"), e("not first")],"ensures":[e("y = prev")]}
        ],
        "imports": []
    });
    Project {
        name: "acc".into(),
        packages: vec![Package {
            name: "p".into(),
            nodes: vec![node],
            contracts: vec![contract],
            ..Default::default()
        }],
        main: Some("Acc".into()),
        ..Default::default()
    }
}

const ACC_CSV: &str = "x,reset\n1,false\n2,false\n-5,false\n0,true\n0,false\n";

#[test]
fn temporal_contract_with_ghosts_monitors_correctly_in_sim_and_c() {
    let project = build_accumulator_project();
    assert!(!ol_contract_check::check_project(&project).has_errors(), "contract is well-formed");

    let trace = Sim::new(&project, "Acc").unwrap().run_csv(ACC_CSV).unwrap();
    // cycle 0: y=1  prev=0  First active (y = x holds)          → clean
    // cycle 1: y=3  prev=1  Lying active (3 ≠ 1)                → Lying::ensure#0
    // cycle 2: y=-2 prev=3  x<0; -2 < 3; Lying (-2 ≠ 3)         → nonneg|grows|Lying::ensure#0
    // cycle 3: y=0  Reset active (y = 0 holds)                  → clean
    // cycle 4: y=0  prev=0  Lying active (0 = 0)                → clean
    let modes: Vec<String> = trace.active_modes.iter().map(|m| m.join("|")).collect();
    assert_eq!(modes, vec!["First", "Lying", "Lying", "Reset", "Lying"]);
    let by_cycle = |c: usize| -> Vec<&str> {
        trace.violations.iter().filter(|(_, k)| *k == c).map(|(l, _)| l.as_str()).collect()
    };
    assert!(by_cycle(0).is_empty() && by_cycle(3).is_empty() && by_cycle(4).is_empty(), "{:?}", trace.violations);
    assert_eq!(by_cycle(1), vec!["Lying::ensure#0"]);
    assert_eq!(by_cycle(2), vec!["nonneg", "grows", "Lying::ensure#0"]);

    if !cc_available() {
        eprintln!("skipping the C half: cc not available");
        return;
    }
    let ir_csv = trace.to_csv();
    let bundle = ol_clite_emit::emit_project(&project);
    let monitor = ol_clite_emit::monitor::emit_monitors(&project);
    let driver = ol_clite_emit::harness::emit_csv_driver_with_monitor(
        project.find_node("Acc").unwrap(),
        Some("AccC"),
    );
    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(tmp.join("openlustre_monitors.h"), &monitor.header).unwrap();
    std::fs::write(tmp.join("openlustre_monitors.c"), &monitor.source).unwrap();
    std::fs::write(tmp.join("driver.c"), &driver).unwrap();
    let exe = tmp.join("acc_driver");
    let cc = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Wno-unused-but-set-variable", "-Wno-unused-variable", "-Werror", "-o"])
        .arg(&exe)
        .arg(tmp.join("openlustre_generated.c"))
        .arg(tmp.join("openlustre_monitors.c"))
        .arg(tmp.join("driver.c"))
        .arg(format!("-I{}", tmp.display()))
        .output()
        .expect("cc runs");
    if !cc.status.success() {
        let stderr = String::from_utf8_lossy(&cc.stderr).to_string();
        let _ = std::fs::remove_dir_all(&tmp);
        panic!("cc failed:\n{stderr}\n--- monitors.c ---\n{}", monitor.source);
    }
    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("driver runs");
    use std::io::Write as _;
    child.stdin.as_mut().unwrap().write_all(ACC_CSV.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("driver finishes");
    let c_csv = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "driver crashed");
    assert_eq!(ir_csv, c_csv, "IR and compiled-C monitors must agree byte for byte");
}
