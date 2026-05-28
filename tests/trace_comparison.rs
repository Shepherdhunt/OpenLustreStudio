//! Phase 6 trace comparison: emit C-Lite for a model that uses stateful
//! standard-library operators, build it with `cc`, run it on a CSV input
//! vector, and assert the output trace is byte-identical to what the IR
//! simulator produces from the same vector.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use ol_ir::{Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};

fn libraries_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries")
}

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Same shape as `tests/stdlib_subnode_calls.rs::build_edge_counter_project`
/// but kept local so the two tests stay independent.
fn build_edge_counter_project() -> Project {
    let node = NodeDef {
        name: "EdgeCounter".into(),
        kind: NodeKind::Operator,
        inputs: vec![
            Port { name: "button".into(), ty: Type::Bool },
            Port { name: "clear".into(), ty: Type::Bool },
        ],
        outputs: vec![
            Port { name: "edge".into(), ty: Type::Bool },
            Port { name: "count".into(), ty: Type::Int32 },
            Port { name: "armed".into(), ty: Type::Bool },
        ],
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["edge".into()],
                rhs: Expr::call("RisingEdge", vec![Expr::var("button")]),
            },
            Equation {
                lhs: vec!["count".into()],
                rhs: Expr::call("Counter", vec![Expr::var("clear"), Expr::var("edge")]),
            },
            Equation {
                lhs: vec!["armed".into()],
                rhs: Expr::call("Latch", vec![Expr::var("edge"), Expr::var("clear")]),
            },
        ],
        contract: None,
        diagram: Default::default(),
    };
    Project {
        name: "edge_counter".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some("EdgeCounter".into()),
    }
}

const INPUT_CSV: &str = "\
button,clear
false,false
true,false
true,false
false,false
true,false
true,false
false,true
true,false
true,false
false,false
";

#[test]
fn ir_simulator_and_generated_c_lite_agree_byte_for_byte() {
    if !cc_available() {
        eprintln!("skipping: cc not available");
        return;
    }

    let mut project = build_edge_counter_project();
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("stdlib loads");
    lib.merge_into(&mut project, "stdlib");

    // 1. Run the IR simulator and capture its CSV trace.
    let ir_trace = {
        let mut sim = ol_sim::Sim::new(&project, "EdgeCounter").unwrap();
        sim.run_csv(INPUT_CSV).unwrap().to_csv()
    };

    // 2. Emit C-Lite for the same project plus a CSV driver.
    let bundle = ol_clite_emit::emit_project(&project);
    let entry = project.find_node("EdgeCounter").unwrap();
    let driver = ol_clite_emit::harness::emit_csv_driver(entry);

    let tmp =
        tempdir_in(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))).expect("temp dir");
    let header_path = tmp.join("openlustre_generated.h");
    let source_path = tmp.join("openlustre_generated.c");
    let driver_path = tmp.join("driver.c");
    let exe_path = tmp.join("trace_driver");

    std::fs::write(&header_path, &bundle.header).unwrap();
    std::fs::write(&source_path, &bundle.source).unwrap();
    std::fs::write(&driver_path, &driver).unwrap();

    // 3. Compile with strict warnings as errors so any sloppy emission fails
    //    the test rather than silently miscompiling.
    let cc = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Wno-unused-but-set-variable",
               "-Wno-unused-variable", "-Werror", "-o"])
        .arg(&exe_path)
        .arg(&source_path)
        .arg(&driver_path)
        .arg(format!("-I{}", tmp.display()))
        .output()
        .expect("cc runs");
    if !cc.status.success() {
        panic!(
            "cc failed:\nstdout:\n{}\nstderr:\n{}\n--- header ---\n{}\n--- source ---\n{}\n--- driver ---\n{}",
            String::from_utf8_lossy(&cc.stdout),
            String::from_utf8_lossy(&cc.stderr),
            bundle.header,
            bundle.source,
            driver,
        );
    }

    // 4. Run the compiled driver against the same CSV.
    let mut child = Command::new(&exe_path)
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
    assert!(out.status.success(), "driver crashed: {:?}", out);
    let clite_trace = String::from_utf8(out.stdout).unwrap();

    // 5. The traces must match byte-for-byte.
    let traces_match = ir_trace == clite_trace;

    // Best-effort cleanup whether we matched or not.
    let _ = std::fs::remove_dir_all(&tmp);

    if !traces_match {
        panic!(
            "trace mismatch\n--- IR simulator ---\n{ir_trace}\n--- generated C-Lite ---\n{clite_trace}"
        );
    }
}

/// Minimal stand-in for `tempfile` so we don't pull in another dep just for
/// this test. Creates and returns a unique directory under `parent`; the
/// caller is responsible for treating it as ephemeral.
fn tempdir_in(parent: &std::path::Path) -> std::io::Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = parent.join(format!("__trace_tmp_{stamp}"));
    std::fs::create_dir_all(&p)?;
    Ok(p)
}
