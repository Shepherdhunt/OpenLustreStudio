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
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
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

/// The verification spine, with temporal state inside branches: a resettable
/// up-counter written as a decision tree. IR simulation and the compiled
/// generated C must agree byte-for-byte.
#[test]
fn activation_ir_sim_and_generated_c_agree_byte_for_byte() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }

    let held = Expr::arrow(Expr::int_lit(0), Expr::pre(Expr::var("n")));
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
                equations: vec![eq("n", Expr::bin(BinOp::Add, held.clone(), Expr::int_lit(1)))],
            },
        ],
        else_equations: vec![eq("n", held)],
        owner: "Counter2".into(),
    };
    let mut project = Project {
        name: "c2".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![operator(
                "Counter2",
                vec![
                    Port { name: "up".into(), ty: Type::Bool },
                    Port { name: "reset".into(), ty: Type::Bool },
                ],
                vec![Port { name: "n".into(), ty: Type::Int32 }],
            )],
            activations: vec![act],
            ..Default::default()
        }],
        main: Some("Counter2".into()),
        ..Default::default()
    };
    project.lower_activations().expect("lowers");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    const INPUT_CSV: &str = "\
up,reset
true,false
true,false
false,false
true,false
false,true
true,false
true,false
";

    let ir_csv = {
        let mut sim = ol_sim::Sim::new(&project, "Counter2").unwrap();
        sim.run_csv(INPUT_CSV).unwrap().to_csv()
    };

    let bundle = ol_clite_emit::emit_project(&project);
    let entry = project.find_node("Counter2").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver(entry);

    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(tmp.join("driver.c"), &driver).unwrap();
    let exe = tmp.join("act_driver");

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
    child.stdin.as_mut().unwrap().write_all(INPUT_CSV.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("driver finishes");
    let success = out.status.success();
    let c_csv = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(success, "driver crashed");
    if ir_csv != c_csv {
        panic!("trace mismatch\n--- IR ---\n{ir_csv}\n--- C ---\n{c_csv}");
    }

    // Sanity on the counter itself: count 1,2, hold, 3, reset, 1, 2.
    let rows: Vec<&str> = ir_csv.lines().collect();
    let n_col = rows[0].split(',').position(|c| c == "n").unwrap();
    let ns: Vec<String> =
        (1..rows.len()).map(|r| rows[r].split(',').nth(n_col).unwrap().to_string()).collect();
    assert_eq!(ns, vec!["1", "2", "2", "3", "0", "1", "2"]);
}
