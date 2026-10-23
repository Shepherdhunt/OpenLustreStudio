//! Conditional activations ("activate if") end to end: prioritized branch
//! selection through the IR simulator, SCADE exhaustiveness enforcement,
//! owner integration rules, slice survival, and the load-bearing invariant —
//! an activation with temporal state produces byte-identical traces from the
//! IR simulator and the compiled generated C.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_ir::{
    ActivationBranch, ActivationDef, BinOp, Equation, Expr, NodeDef, NodeKind, Package, Port,
    Project, Type,
};

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn make_tempdir() -> PathBuf {
    let stamp = openlustre_integration_tests::unique_stamp();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_act_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn operator(name: &str, inputs: Vec<Port>, outputs: Vec<Port>) -> NodeDef {
    NodeDef {
        name: name.into(),
        kind: NodeKind::Operator,
        inputs,
        outputs,
        locals: vec![],
        equations: vec![],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
    }
}

fn eq(lhs: &str, rhs: Expr) -> Equation {
    Equation { lhs: vec![lhs.into()], rhs }
}

/// Guard(arm, fault, x) returns (cmd, safe) via a three-way decision tree:
/// `if fault` wins over `elsif arm`, and the else branch passes x through.
fn guard_project() -> Project {
    let act = ActivationDef {
        name: "Select".into(),
        outputs: vec![
            Port { name: "cmd".into(), ty: Type::Int32 },
            Port { name: "safe".into(), ty: Type::Bool },
        ],
        branches: vec![
            ActivationBranch {
                name: "Fault".into(),
                condition: Expr::var("fault"),
                equations: vec![eq("cmd", Expr::int_lit(0)), eq("safe", Expr::bool_lit(true))],
            },
            ActivationBranch {
                name: "Engaged".into(),
                condition: Expr::var("arm"),
                equations: vec![
                    eq("cmd", Expr::bin(BinOp::Add, Expr::var("x"), Expr::int_lit(1))),
                    eq("safe", Expr::bool_lit(false)),
                ],
            },
        ],
        else_equations: vec![eq("cmd", Expr::var("x")), eq("safe", Expr::bool_lit(true))],
        owner: "Guard".into(),
    };
    Project {
        name: "guard".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![operator(
                "Guard",
                vec![
                    Port { name: "arm".into(), ty: Type::Bool },
                    Port { name: "fault".into(), ty: Type::Bool },
                    Port { name: "x".into(), ty: Type::Int32 },
                ],
                vec![
                    Port { name: "cmd".into(), ty: Type::Int32 },
                    Port { name: "safe".into(), ty: Type::Bool },
                ],
            )],
            activations: vec![act],
            ..Default::default()
        }],
        main: Some("Guard".into()),
        ..Default::default()
    }
}

#[test]
fn activation_selects_by_priority_and_falls_back_to_else() {
    let mut project = guard_project();
    project.lower_activations().expect("lowers");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    // The branch-selected flags are real, inspectable locals on the owner.
    let node = project.find_node("Guard").unwrap();
    assert!(node.locals.iter().any(|l| l.name == "__act_Select_b1"));
    assert!(node.locals.iter().any(|l| l.name == "__act_Select_b2"));

    let mut sim = ol_sim::Sim::new(&project, "Guard").unwrap();
    let csv = sim
        .run_csv(
            "arm,fault,x\n\
             false,false,10\n\
             true,false,10\n\
             true,true,10\n\
             false,true,10\n\
             false,false,7\n",
        )
        .unwrap()
        .to_csv();
    let rows: Vec<&str> = csv.lines().collect();
    let header = rows[0];
    let cmd_col = header.split(',').position(|c| c == "cmd").expect("cmd column");
    let safe_col = header.split(',').position(|c| c == "safe").expect("safe column");
    let cell = |row: usize, col: usize| rows[row].split(',').nth(col).unwrap().to_string();
    // else → arm branch → fault wins over arm → fault alone → else again.
    assert_eq!(cell(1, cmd_col), "10");
    assert_eq!(cell(1, safe_col), "true");
    assert_eq!(cell(2, cmd_col), "11");
    assert_eq!(cell(2, safe_col), "false");
    assert_eq!(cell(3, cmd_col), "0", "fault branch has priority over arm");
    assert_eq!(cell(3, safe_col), "true");
    assert_eq!(cell(4, cmd_col), "0");
    assert_eq!(cell(5, cmd_col), "7");
}

#[test]
fn activation_exhaustiveness_and_owner_rules_are_enforced() {
    // A branch that misses one output is rejected with the branch named.
    let mut project = guard_project();
    project.packages[0].activations[0].branches[1].equations.pop(); // drop `safe` in Engaged
    let errs = project.lower_activations().unwrap_err();
    assert!(
        errs.iter().any(|e| {
            matches!(e, ol_ir::ActLowerError::OutputUnassigned(a, o, b)
                if a == "Select" && o == "safe" && b == "Engaged")
        }),
        "expected OutputUnassigned for Engaged/safe, got {errs:?}"
    );

    // The else branch must assign every output too.
    let mut project = guard_project();
    project.packages[0].activations[0].else_equations.pop();
    let errs = project.lower_activations().unwrap_err();
    assert!(errs.iter().any(|e| {
        matches!(e, ol_ir::ActLowerError::OutputUnassigned(_, o, b) if o == "safe" && b == "else")
    }));

    // The owner must exist …
    let mut project = guard_project();
    project.packages[0].activations[0].owner = "Ghost".into();
    let errs = project.lower_activations().unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, ol_ir::ActLowerError::UnknownOwner(_, o) if o == "Ghost")));

    // A driven variable the owner no longer declares (e.g. the port was
    // deleted) does NOT fail the load: lowering merges and the type checker
    // pins E0020 on the equation — visible on the canvas, never a crash.
    let mut project = guard_project();
    project.packages[0].nodes[0].outputs.retain(|p| p.name != "safe");
    project.lower_activations().expect("lowering tolerates a missing driven var");
    let report = ol_typecheck::check_project(&project);
    assert!(
        report.errors().any(|d| d.code == "E0020" && d.message.contains("`safe`")),
        "missing driven variable is reported: {:?}",
        report.diagnostics
    );

    // Driving an INPUT (e.g. the port's role changed) is E0022.
    let mut project = guard_project();
    let safe = project.packages[0].nodes[0].outputs.pop().unwrap();
    project.packages[0].nodes[0].inputs.push(safe);
    project.lower_activations().expect("lowers");
    let report = ol_typecheck::check_project(&project);
    assert!(
        report.errors().any(|d| d.code == "E0022" && d.message.contains("`safe`")),
        "assigning an input is reported: {:?}",
        report.diagnostics
    );

    // An activation with no branches is rejected.
    let mut project = guard_project();
    project.packages[0].activations[0].branches.clear();
    let errs = project.lower_activations().unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, ol_ir::ActLowerError::NoBranches(_))));
}

#[test]
fn activation_rides_with_its_owner_through_the_slicer() {
    let mut project = guard_project();
    // A second, unrelated operator that slicing away must not disturb.
    project.packages[0]
        .nodes
        .push(operator("Other", vec![], vec![Port { name: "o".into(), ty: Type::Bool }]));
    project.packages[0].nodes[1].equations.push(eq("o", Expr::bool_lit(false)));

    let sliced = project.slice_for_root("Guard").expect("slices");
    assert_eq!(sliced.packages[0].activations.len(), 1, "activation kept with its owner");
    assert!(sliced.find_node("Other").is_none(), "unrelated operator sliced away");

    let sliced_away = project.slice_for_root("Other").expect("slices");
    assert!(
        sliced_away.packages[0].activations.is_empty(),
        "activation dropped when its owner is not in the slice"
    );
}

/// Simulate `node` on `input` with the IR simulator and with the compiled
/// generated C; assert the traces are byte-identical and return the IR one.
fn ir_and_c_agree(project: &Project, node: &str, input: &str) -> String {
    let ir_csv = {
        let mut sim = ol_sim::Sim::new(project, node).unwrap();
        sim.run_csv(input).unwrap().to_csv()
    };
    if !cc_available() {
        eprintln!("cc not available: IR trace only");
        return ir_csv;
    }
    let bundle = ol_clite_emit::emit_project(project);
    let entry = project.find_node(node).unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver(entry);

    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(tmp.join("driver.c"), &driver).unwrap();
    let exe = tmp.join("act_driver");
    let cc = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Wno-unused-but-set-variable", "-Wno-unused-variable", "-Werror", "-o"])
        .arg(&exe)
        .arg(tmp.join("openlustre_generated.c"))
        .arg(tmp.join("driver.c"))
        .arg(format!("-I{}", tmp.display()))
        .output()
        .expect("cc runs");
    if !cc.status.success() {
        let stderr = String::from_utf8_lossy(&cc.stderr).to_string();
        let _ = std::fs::remove_dir_all(&tmp);
        panic!("cc failed:\n{stderr}\n--- generated.c ---\n{}", bundle.source);
    }
    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("driver runs");
    use std::io::Write as _;
    child.stdin.as_mut().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("driver finishes");
    let c_csv = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "driver crashed");
    if ir_csv != c_csv {
        panic!("trace mismatch\n--- IR ---\n{ir_csv}\n--- C ---\n{c_csv}");
    }
    ir_csv
}

/// One output column of a trace.
fn column(csv: &str, name: &str) -> Vec<String> {
    let rows: Vec<&str> = csv.lines().collect();
    let col = rows[0].split(',').position(|c| c == name).unwrap();
    rows[1..].iter().map(|r| r.split(',').nth(col).unwrap().to_string()).collect()
}

fn last(v: &str) -> Expr {
    Expr::Call { node: "last".into(), args: vec![Expr::var(v)] }
}

fn single_operator_project(owner: NodeDef, extra: Vec<NodeDef>, act: ActivationDef) -> Project {
    let mut nodes = vec![owner];
    nodes.extend(extra);
    let main = nodes[0].name.clone();
    Project {
        name: "act".into(),
        packages: vec![Package { name: "user".into(), nodes, activations: vec![act], ..Default::default() }],
        main: Some(main),
        ..Default::default()
    }
}

/// The verification spine, with the SCADE "hold" pattern: a resettable
/// up-counter as a decision tree, holding with `last(n)` (SCADE's `last 'n`
/// — the previous cycle's value, whichever branch set it). IR simulation and
/// the compiled generated C must agree byte for byte.
#[test]
fn activation_ir_sim_and_generated_c_agree_byte_for_byte() {
    let act = ActivationDef {
        name: "Count".into(),
        outputs: vec![Port { name: "n".into(), ty: Type::Int32 }],
        branches: vec![
            ActivationBranch {
                name: "Reset".into(),
                condition: Expr::var("reset"),
                equations: vec![eq("n", Expr::int_lit(0))],
            },
            ActivationBranch {
                name: "Up".into(),
                condition: Expr::var("up"),
                equations: vec![eq("n", Expr::bin(BinOp::Add, last("n"), Expr::int_lit(1)))],
            },
        ],
        else_equations: vec![eq("n", last("n"))],
        owner: "Counter2".into(),
    };
    let owner = operator(
        "Counter2",
        vec![Port { name: "up".into(), ty: Type::Bool }, Port { name: "reset".into(), ty: Type::Bool }],
        vec![Port { name: "n".into(), ty: Type::Int32 }],
    );
    let mut project = single_operator_project(owner, vec![], act);
    project.lower_activations().expect("lowers");
    let tc = ol_typecheck::check_project(&project);
    assert!(!tc.has_errors(), "{:?}", tc.errors().collect::<Vec<_>>());

    let csv = ir_and_c_agree(&project, "Counter2", "up,reset\ntrue,false\ntrue,false\nfalse,false\ntrue,false\nfalse,true\ntrue,false\ntrue,false\n");
    // Count 1, 2, hold, 3, reset, 1, 2.
    assert_eq!(column(&csv, "n"), ["1", "2", "2", "3", "0", "1", "2"]);
}

/// SCADE's clocked branches: each branch runs only when selected, and its
/// state is frozen otherwise — `pre` reads the branch's previous activation,
/// `->` initializes on its first activation, and a stateful call inside a
/// branch steps only while the branch runs. `last(h, init)` holds across
/// branches. Model and generated C agree byte for byte.
#[test]
fn activation_branches_are_clocked_and_freeze_when_inactive() {
    // Tick(inc) = running sum of inc (stateful).
    let mut tick = operator(
        "Tick",
        vec![Port { name: "inc".into(), ty: Type::Int32 }],
        vec![Port { name: "c".into(), ty: Type::Int32 }],
    );
    tick.equations.push(eq(
        "c",
        Expr::bin(BinOp::Add, Expr::arrow(Expr::int_lit(0), Expr::pre(Expr::var("c"))), Expr::var("inc")),
    ));
    let call_tick = Expr::Call { node: "Tick".into(), args: vec![Expr::int_lit(1)] };
    let act = ActivationDef {
        name: "Mode".into(),
        outputs: vec![
            Port { name: "k".into(), ty: Type::Int32 },
            Port { name: "t".into(), ty: Type::Int32 },
            Port { name: "h".into(), ty: Type::Int32 },
        ],
        branches: vec![ActivationBranch {
            name: "On".into(),
            condition: Expr::var("sel"),
            equations: vec![
                // Counts On activations: pre k is k at the previous On cycle.
                eq("k", Expr::bin(BinOp::Add, Expr::arrow(Expr::int_lit(0), Expr::pre(Expr::var("k"))), Expr::int_lit(1))),
                eq("t", call_tick.clone()),
                eq("h", Expr::var("x")),
            ],
        }],
        else_equations: vec![
            // Counts down from 99 per else activation, frozen while On runs.
            eq("k", Expr::bin(BinOp::Sub, Expr::arrow(Expr::int_lit(100), Expr::pre(Expr::var("k"))), Expr::int_lit(1))),
            eq("t", call_tick),
            eq("h", Expr::Call { node: "last".into(), args: vec![Expr::var("h"), Expr::int_lit(-1)] }),
        ],
        owner: "Frozen".into(),
    };
    let owner = operator(
        "Frozen",
        vec![Port { name: "sel".into(), ty: Type::Bool }, Port { name: "x".into(), ty: Type::Int32 }],
        vec![
            Port { name: "k".into(), ty: Type::Int32 },
            Port { name: "t".into(), ty: Type::Int32 },
            Port { name: "h".into(), ty: Type::Int32 },
        ],
    );
    let mut project = single_operator_project(owner, vec![tick], act);
    project.lower_activations().expect("lowers");
    let tc = ol_typecheck::check_project(&project);
    assert!(!tc.has_errors(), "{:?}", tc.errors().collect::<Vec<_>>());

    // The Lustre view (and so Kind 2) declares the branch locals on their
    // clocks and merges the outputs back.
    let lus = ol_lustre_emit::emit_project(&project);
    assert!(lus.contains("__act_Mode_s1_x: int when __act_Mode_b1"), "{lus}");
    assert!(lus.contains("__act_Mode_xe_k: int when not __act_Mode_b1"), "{lus}");
    assert!(lus.contains("merge"), "{lus}");

    let input = "sel,x\ntrue,5\ntrue,6\nfalse,7\nfalse,8\ntrue,9\nfalse,10\ntrue,11\ntrue,12\n";
    let csv = ir_and_c_agree(&project, "Frozen", input);
    // On: 1, 2, (frozen), 3, (frozen), 4, 5. Else: 99, 98, (frozen), 97.
    assert_eq!(column(&csv, "k"), ["1", "2", "99", "98", "3", "97", "4", "5"]);
    // Each branch's Tick instance advances only on its own cycles.
    assert_eq!(column(&csv, "t"), ["1", "2", "1", "2", "3", "3", "4", "5"]);
    // h follows x while On, holds the last value in else.
    assert_eq!(column(&csv, "h"), ["5", "6", "6", "6", "9", "9", "11", "12"]);
}

#[test]
fn last_needs_a_variable_and_an_init_for_aggregates() {
    let act = |rhs: Expr| ActivationDef {
        name: "L".into(),
        outputs: vec![Port { name: "o".into(), ty: Type::Int32 }],
        branches: vec![ActivationBranch { name: "B".into(), condition: Expr::var("c"), equations: vec![eq("o", rhs.clone())] }],
        else_equations: vec![eq("o", Expr::int_lit(0))],
        owner: "Own".into(),
    };
    let owner = || operator("Own", vec![Port { name: "c".into(), ty: Type::Bool }], vec![Port { name: "o".into(), ty: Type::Int32 }]);
    let bad = Expr::Call { node: "last".into(), args: vec![Expr::int_lit(3)] };
    let errs = single_operator_project(owner(), vec![], act(bad)).lower_activations().unwrap_err();
    assert!(matches!(errs[0], ol_ir::ActLowerError::LastArgs(_)), "{errs:?}");
    let errs = single_operator_project(owner(), vec![], act(last("nope"))).lower_activations().unwrap_err();
    assert!(matches!(errs[0], ol_ir::ActLowerError::LastUnknown(..)), "{errs:?}");
}
