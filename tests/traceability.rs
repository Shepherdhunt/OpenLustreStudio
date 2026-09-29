//! Model-to-code traceability of the generated C: every equation of every
//! generated operator carries a `@trace` comment naming its diagram element
//! (and, when lowered from an owned construct, the construct and branch);
//! the trace matrix gives each one's line range; the generation report
//! fingerprints every file and counts the coverage.

use ol_ir::{ActivationBranch, ActivationDef, BinOp, Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};

fn eq(lhs: &str, rhs: Expr) -> Equation {
    Equation { lhs: vec![lhs.into()], rhs }
}

fn project() -> Project {
    let owner = NodeDef {
        name: "Guard".into(),
        kind: NodeKind::Operator,
        inputs: vec![
            Port { name: "arm".into(), ty: Type::Bool },
            Port { name: "fault".into(), ty: Type::Bool },
            Port { name: "x".into(), ty: Type::Int32 },
        ],
        outputs: vec![
            Port { name: "cmd".into(), ty: Type::Int32 },
            Port { name: "twice".into(), ty: Type::Int32 },
        ],
        locals: vec![],
        equations: vec![eq("twice", Expr::bin(BinOp::Mul, Expr::var("x"), Expr::int_lit(2)))],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
    };
    let act = ActivationDef {
        name: "Select".into(),
        outputs: vec![Port { name: "cmd".into(), ty: Type::Int32 }],
        branches: vec![
            ActivationBranch { name: "Fault".into(), condition: Expr::var("fault"), equations: vec![eq("cmd", Expr::int_lit(0))] },
            ActivationBranch {
                name: "Armed".into(),
                condition: Expr::var("arm"),
                equations: vec![eq("cmd", Expr::bin(BinOp::Add, Expr::var("x"), Expr::int_lit(1)))],
            },
        ],
        else_equations: vec![eq("cmd", Expr::Call { node: "last".into(), args: vec![Expr::var("cmd")] })],
        owner: "Guard".into(),
    };
    let mut p = Project {
        name: "trace".into(),
        packages: vec![Package { name: "user".into(), nodes: vec![owner], activations: vec![act], ..Default::default() }],
        main: Some("Guard".into()),
        ..Default::default()
    };
    p.lower_activations().expect("lowers");
    p
}

#[test]
fn every_equation_is_traced_to_its_diagram_element_and_lines() {
    let p = project();
    let bundle = ol_clite_emit::emit_project(&p);
    let node = p.find_node("Guard").unwrap();
    let lines: Vec<&str> = bundle.source.lines().collect();

    assert_eq!(bundle.trace.len(), node.equations.len(), "one entry per equation");
    let mut seen: Vec<usize> = bundle.trace.iter().map(|t| t.equation).collect();
    seen.sort();
    assert_eq!(seen, (0..node.equations.len()).collect::<Vec<_>>());

    for t in &bundle.trace {
        assert!(t.first_line >= 1 && t.first_line < t.last_line && t.last_line <= lines.len(), "{t:?}");
        // The range opens on the equation's @trace comment…
        let head = lines[t.first_line - 1];
        assert!(head.contains(&format!("@trace Guard {}", t.element)), "{head}");
        // …and holds the statement assigning its lhs.
        let body = lines[t.first_line..t.last_line].join("\n");
        assert!(body.contains(&t.lhs[0]), "{t:?}\n{body}");
    }

    // The operator's own equation keeps its diagram id.
    let own = bundle.trace.iter().find(|t| t.lhs == ["twice"]).unwrap();
    assert_eq!(own.element, "eq0");
    assert_eq!(own.origin, None);
    assert_eq!(own.source, "twice = x * 2");
    // Lowered equations point at the construct's block, with the branch.
    let armed = bundle.trace.iter().find(|t| t.lhs == ["__act_Select_x2_cmd"]).unwrap();
    assert_eq!(armed.element, "act:Select");
    assert_eq!(armed.origin.as_deref(), Some("activation Select, branch Armed computes cmd"));
    let merged = bundle.trace.iter().find(|t| t.lhs == ["cmd"]).unwrap();
    assert_eq!(merged.origin.as_deref(), Some("activation Select, merges cmd"));
    let hold = bundle.trace.iter().find(|t| t.lhs == ["__act_Select_last_cmd"]).unwrap();
    assert_eq!(hold.origin.as_deref(), Some("activation Select, last(cmd)"));
    // Comments are plain ASCII.
    assert!(bundle.source.lines().filter(|l| l.contains("@trace")).all(|l| l.is_ascii()));
}

#[test]
fn generation_report_fingerprints_files_and_counts_coverage() {
    let p = project();
    let bundle = ol_clite_emit::emit_project(&p);
    let files = [("openlustre_generated.h", bundle.header.as_str()), ("openlustre_generated.c", bundle.source.as_str())];
    let report = ol_clite_emit::trace::report(&p, Some("Guard"), &files, &bundle.trace);
    assert_eq!(report.traced, report.equations);
    assert_eq!(report.files[1].sha256, ol_clite_emit::trace::sha256_hex(bundle.source.as_bytes()));
    assert_eq!(report.files[1].lines, bundle.source.lines().count());
    let op = &report.operators[0];
    assert_eq!(op.name, "Guard");
    assert_eq!(op.step_function, "Guard_step");
    assert_eq!(op.constructs, ["act:Select"]);
    assert!(op.state_fields > 0, "pre state and held clocked values");
    let md = report.to_markdown();
    assert!(md.contains("| `openlustre_generated.c` |") && md.contains("Guard_step"), "{md}");
}

#[test]
fn emit_clite_writes_the_trace_matrix_and_report() {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tmp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_traceability_{stamp}"));
    std::fs::create_dir_all(&tmp).unwrap();
    let model = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/release_logic/model/release_logic.json");
    let out = std::process::Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "emit-clite"])
        .arg(&model)
        .arg("--out")
        .arg(&tmp)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let trace: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(tmp.join("trace.json")).unwrap()).unwrap();
    assert!(!trace.as_array().unwrap().is_empty());
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.join("generation_report.json")).unwrap()).unwrap();
    assert_eq!(report["traced"], report["equations"], "{report}");
    let source = std::fs::read_to_string(tmp.join("clite/openlustre_generated.c")).unwrap();
    let c = report["files"].as_array().unwrap().iter().find(|f| f["name"] == "clite/openlustre_generated.c").unwrap();
    assert_eq!(c["sha256"], ol_clite_emit::trace::sha256_hex(source.as_bytes()));
    assert!(std::fs::read_to_string(tmp.join("generation_report.md")).unwrap().contains("# C-Lite generation report"));
    let _ = std::fs::remove_dir_all(&tmp);
}
