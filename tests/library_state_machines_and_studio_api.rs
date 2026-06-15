//! Closeout coverage for Phase 9 (library state-machine blocks) and the
//! Phase 8 scaffold (`openlustre studio inspect` JSON IPC the future GUI
//! consumes).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use ol_ir::{Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};
use ol_sim::{Sim, Value};

fn libraries_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries")
}

fn build_with_stdlib(node: NodeDef, main: &str) -> Project {
    let mut p = Project {
        name: "sm_lib_test".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![node],
            ..Default::default()
        }],
        main: Some(main.into()),
        ..Default::default()
    };
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("library loads");
    lib.merge_into(&mut p, "stdlib");
    p
}

#[test]
fn srflipflop_library_block_loads_lowers_and_simulates() {
    // node Latched(set, reset: bool) returns (q: bool)
    //   q = SRFlipFlop(set, reset);
    let node = NodeDef {
        name: "Latched".into(),
        kind: NodeKind::Operator,
        inputs: vec![
            Port { name: "set".into(), ty: Type::Bool },
            Port { name: "reset".into(), ty: Type::Bool },
        ],
        outputs: vec![Port { name: "q".into(), ty: Type::Bool }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["q".into()],
            rhs: Expr::call("SRFlipFlop", vec![Expr::var("set"), Expr::var("reset")]),
        }],
        contract: None,
        diagram: Default::default(),
            probes: vec![],
    };
    let project = build_with_stdlib(node, "Latched");
    let report = ol_typecheck::check_project(&project);
    assert!(
        !report.has_errors(),
        "errors: {:?}",
        report.errors().map(|d| d.render()).collect::<Vec<_>>()
    );

    let mut sim = Sim::new(&project, "Latched").unwrap();
    // The SRFF starts in Reset (q = false). On cycle 0 we read q = false
    // (the FSM's "current state output"), then transitions advance state for
    // the next cycle. So the canonical pattern is q lagging set/reset by one
    // cycle.
    // set:    F T F F T F F
    // reset:  F F F T F F F
    // q:      F F T T T T F  (state: Reset Reset Set Reset Reset Set Set)
    // Wait — by the SRFF lowering: state = Reset -> pre next_state, and the
    // body of state `Reset` outputs q=false; body of `Set` outputs q=true.
    // So q is whatever state we are *in* on this cycle.
    let inputs: Vec<(bool, bool)> = vec![
        (false, false), // c0: Reset, q=F. next=Reset.
        (true, false),  // c1: Reset, q=F. next=Set.
        (false, false), // c2: Set, q=T. next=Set (not set).
        (false, true),  // c3: Set, q=T. next=Reset.
        (true, false),  // c4: Reset, q=F. next=Set.
        (false, false), // c5: Set, q=T. next=Set.
    ];
    let expected_q = [false, false, true, true, false, true];
    for (i, (s, r)) in inputs.into_iter().enumerate() {
        let mut input = BTreeMap::new();
        input.insert("set".into(), Value::Bool(s));
        input.insert("reset".into(), Value::Bool(r));
        let out = sim.step(&input).unwrap();
        assert_eq!(
            out["q"].as_bool(),
            Some(expected_q[i]),
            "cycle {i}: set={s}, reset={r}"
        );
    }
}

#[test]
fn srflipflop_appears_in_library_with_state_machine_category() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).unwrap();
    let entry = lib
        .entries
        .iter()
        .find(|e| e.block.node.name == "SRFlipFlop")
        .expect("SRFlipFlop block exists");
    assert_eq!(entry.block.category.as_deref(), Some("state_machine"));
    // The lowering must surface the auto-generated state enum so the type
    // appears in the merged project.
    let has_state_enum = entry
        .block
        .extra_types
        .iter()
        .any(|t| t.name() == "SRFlipFlop_StateEnum");
    assert!(has_state_enum, "expected auto-generated SRFlipFlop_StateEnum");
}

#[test]
fn studio_inspect_emits_the_documented_schema() {
    // Run the CLI as a child process so the test verifies the full IPC the
    // GUI will rely on, not just an internal helper.
    let model = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/release_logic/model/release_logic.json");
    let out = Command::new(env!("CARGO"))
        .args([
            "run",
            "-q",
            "-p",
            "ol_cli",
            "--",
            "studio",
            "inspect",
        ])
        .arg(&model)
        .output()
        .expect("cargo run");
    assert!(out.status.success(), "studio inspect failed");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("studio inspect produces JSON");

    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["project"]["name"], "release_authorization");
    assert!(v["project"]["packages"].as_array().unwrap().len() >= 1);
    let pkg = &v["project"]["packages"][0];
    let nodes = pkg["nodes"].as_array().unwrap();
    assert!(nodes
        .iter()
        .any(|n| n["name"] == "ReleaseLogic" && n["kind"] == "Operator"));
    // The contract summary must include the three modes the example declares.
    let contracts = pkg["contracts"].as_array().unwrap();
    let c = contracts
        .iter()
        .find(|c| c["name"] == "ReleaseLogic_contract")
        .unwrap();
    assert_eq!(c["mode_count"], 3);
    let modes: Vec<&str> = c["modes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert!(modes.contains(&"SafeInhibit"));
    assert!(modes.contains(&"AuthorizedRelease"));
    assert!(modes.contains(&"Idle"));
}

#[test]
fn library_now_advertises_41_blocks_with_state_machine_category() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).unwrap();
    let names: Vec<&str> = lib.nodes().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"SRFlipFlop"));
    assert!(lib.entries.len() >= 41);
}
