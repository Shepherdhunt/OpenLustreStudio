//! State machines through the full back end: lower an FSM, run it through the
//! IR simulator, then emit + compile its C-Lite and confirm the runtime trace
//! is byte-identical. This guards the enum-`pre` and enum-equality lowering in
//! both the simulator and the C-Lite emitter, and the `_StateEnum` vs `_State`
//! naming that keeps the generated C free of typedef collisions.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_ir::{
    Equation, Expr, Package, Port, Project, Region, StateDef, StateMachineDef, Transition, Type,
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

fn traffic_light_project() -> Project {
    let make_state = |name: &str, go: bool, warn: bool, advance_to: &str| StateDef {
        name: name.into(),
        equations: vec![
            Equation { lhs: vec!["go".into()], rhs: Expr::bool_lit(go) },
            Equation { lhs: vec!["warn".into()], rhs: Expr::bool_lit(warn) },
        ],
        transitions: vec![
            Transition { guard: Expr::var("emergency"), target: "Red".into() },
            Transition { guard: Expr::var("tick"), target: advance_to.into() },
        ],
        regions: vec![],
        refines: None,
    };
    let sm = StateMachineDef {
        name: "TrafficLight".into(),
        inputs: vec![
            Port { name: "tick".into(), ty: Type::Bool },
            Port { name: "emergency".into(), ty: Type::Bool },
        ],
        outputs: vec![
            Port { name: "go".into(), ty: Type::Bool },
            Port { name: "warn".into(), ty: Type::Bool },
        ],
        locals: vec![],
        initial_state: "Red".into(),
        states: vec![
            make_state("Red", false, false, "Green"),
            make_state("Green", true, false, "Yellow"),
            make_state("Yellow", false, true, "Red"),
        ],
        contract: None,
        owner: None,
    };
    Project {
        name: "tl".into(),
        packages: vec![Package {
            name: "user".into(),
            state_machines: vec![sm],
            ..Default::default()
        }],
        main: Some("TrafficLight".into()),
        ..Default::default()
    }
}

const INPUT_CSV: &str = "\
tick,emergency
false,false
true,false
true,false
true,false
true,false
false,true
false,false
true,false
true,true
";

#[test]
fn state_machine_ir_sim_and_generated_c_agree_byte_for_byte() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }

    let mut project = traffic_light_project();
    project.lower_state_machines().expect("lowers");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    // 1. IR simulator trace.
    let ir_csv = {
        let mut sim = ol_sim::Sim::new(&project, "TrafficLight").unwrap();
        sim.run_csv(INPUT_CSV).unwrap().to_csv()
    };

    // 2. Emit + compile C-Lite with a driver.
    let bundle = ol_clite_emit::emit_project(&project);
    let entry = project.find_node("TrafficLight").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver(entry);

    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(tmp.join("driver.c"), &driver).unwrap();
    let exe = tmp.join("tl_driver");

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

#[test]
fn generated_c_has_no_state_typedef_collision() {
    let mut project = traffic_light_project();
    project.lower_state_machines().unwrap();
    let bundle = ol_clite_emit::emit_project(&project);
    // The FSM enum is `_StateEnum`; the node's state struct is `_State`. Both
    // must appear, and they must be distinct identifiers.
    assert!(bundle.header.contains("} TrafficLight_StateEnum;"));
    assert!(bundle.header.contains("} TrafficLight_State;"));
}

// --- Hierarchical (nested) automaton through the back end --------------------

/// `Mode`: top states Idle/Active; while Active a nested region toggles Lo<->Hi
/// (on `tick`) driving `level`. Exercises nested state enums, the per-region
/// activation local, and `pre` of generated vars in both backends.
fn mode_project() -> Project {
    let lo = StateDef {
        name: "Lo".into(),
        equations: vec![Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(1) }],
        transitions: vec![Transition { guard: Expr::var("tick"), target: "Hi".into() }],
        regions: vec![],
        refines: None,
    };
    let hi = StateDef {
        name: "Hi".into(),
        equations: vec![Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(2) }],
        transitions: vec![Transition { guard: Expr::var("tick"), target: "Lo".into() }],
        regions: vec![],
        refines: None,
    };
    let idle = StateDef {
        name: "Idle".into(),
        equations: vec![
            Equation { lhs: vec!["active".into()], rhs: Expr::bool_lit(false) },
            Equation { lhs: vec!["level".into()], rhs: Expr::int_lit(0) },
        ],
        transitions: vec![Transition { guard: Expr::var("go"), target: "Active".into() }],
        regions: vec![],
        refines: None,
    };
    let active = StateDef {
        name: "Active".into(),
        equations: vec![Equation { lhs: vec!["active".into()], rhs: Expr::bool_lit(true) }],
        transitions: vec![Transition { guard: Expr::var("stop"), target: "Idle".into() }],
        regions: vec![Region { initial_state: "Lo".into(), states: vec![lo, hi], history: false }],
        refines: None,
    };
    let sm = StateMachineDef {
        name: "Mode".into(),
        inputs: vec![
            Port { name: "go".into(), ty: Type::Bool },
            Port { name: "stop".into(), ty: Type::Bool },
            Port { name: "tick".into(), ty: Type::Bool },
        ],
        outputs: vec![
            Port { name: "active".into(), ty: Type::Bool },
            Port { name: "level".into(), ty: Type::Int32 },
        ],
        locals: vec![],
        initial_state: "Idle".into(),
        states: vec![idle, active],
        contract: None,
        owner: None,
    };
    Project {
        name: "mode".into(),
        packages: vec![Package { name: "user".into(), state_machines: vec![sm], ..Default::default() }],
        main: Some("Mode".into()),
        ..Default::default()
    }
}

const MODE_INPUT_CSV: &str = "\
go,stop,tick
false,false,false
true,false,false
false,false,true
false,false,true
false,true,false
true,false,false
false,false,false
";

#[test]
fn hierarchical_generated_c_has_both_region_enums() {
    let mut project = mode_project();
    project.lower_state_machines().unwrap();
    assert!(!ol_typecheck::check_project(&project).has_errors());
    let bundle = ol_clite_emit::emit_project(&project);
    // The C carries one enum per region — the top automaton and the nested one.
    assert!(bundle.header.contains("} Mode_StateEnum;"), "top enum:\n{}", bundle.header);
    assert!(bundle.header.contains("} Mode_r1_StateEnum;"), "nested enum:\n{}", bundle.header);
}

#[test]
fn hierarchical_ir_sim_and_generated_c_agree_byte_for_byte() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }
    let mut project = mode_project();
    project.lower_state_machines().expect("lowers");
    assert!(!ol_typecheck::check_project(&project).has_errors());

    let ir_csv = {
        let mut sim = ol_sim::Sim::new(&project, "Mode").unwrap();
        sim.run_csv(MODE_INPUT_CSV).unwrap().to_csv()
    };

    let bundle = ol_clite_emit::emit_project(&project);
    let entry = project.find_node("Mode").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver(entry);

    let tmp = make_tempdir();
    std::fs::write(tmp.join("openlustre_generated.h"), &bundle.header).unwrap();
    std::fs::write(tmp.join("openlustre_generated.c"), &bundle.source).unwrap();
    std::fs::write(tmp.join("driver.c"), &driver).unwrap();
    let exe = tmp.join("mode_driver");
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
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("driver runs");
    use std::io::Write as _;
    child.stdin.as_mut().unwrap().write_all(MODE_INPUT_CSV.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("driver finishes");
    let c_csv = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "driver crashed");
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
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_sm_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}
