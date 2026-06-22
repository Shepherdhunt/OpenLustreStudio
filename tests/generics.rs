//! Generic (polymorphic) nodes: a template written once over a type parameter,
//! instantiated at concrete types and monomorphized to ordinary nodes before
//! any downstream tool runs — so typecheck, the simulator and the C-Lite
//! emitter need no awareness of genericity.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use ol_ir::{
    Equation, Expr, GenericInst, GenericNode, NodeDef, NodeKind, Package, Port, Project, Type,
    TypeArg,
};
use ol_sim::{Sim, Value};

/// `Pick<T>(c: bool, a: T, b: T) returns (o: T)` with `o = if c then a else b`.
/// The element type `T` is a `Named` placeholder; `generics` records it as a
/// parameter, and two instantiations specialise it to `int32` and `bool`.
fn pick_template() -> NodeDef {
    NodeDef {
        name: "Pick".into(),
        kind: NodeKind::Function,
        inputs: vec![
            Port { name: "c".into(), ty: Type::Bool },
            Port { name: "a".into(), ty: Type::named("T") },
            Port { name: "b".into(), ty: Type::named("T") },
        ],
        outputs: vec![Port { name: "o".into(), ty: Type::named("T") }],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["o".into()],
            rhs: Expr::if_then_else(Expr::var("c"), Expr::var("a"), Expr::var("b")),
        }],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
    }
}

/// `Choose` uses both instantiations: an `int32` pick and a `bool` pick.
fn choose_node() -> NodeDef {
    let call = |node: &str, args: [&str; 3]| Expr::Call {
        node: node.into(),
        args: args.iter().map(|a| Expr::var(*a)).collect(),
    };
    NodeDef {
        name: "Choose".into(),
        kind: NodeKind::Operator,
        inputs: vec![
            Port { name: "c".into(), ty: Type::Bool },
            Port { name: "x".into(), ty: Type::Int32 },
            Port { name: "y".into(), ty: Type::Int32 },
            Port { name: "bc".into(), ty: Type::Bool },
            Port { name: "p".into(), ty: Type::Bool },
            Port { name: "q".into(), ty: Type::Bool },
        ],
        outputs: vec![
            Port { name: "oi".into(), ty: Type::Int32 },
            Port { name: "ob".into(), ty: Type::Bool },
        ],
        locals: vec![],
        equations: vec![
            Equation { lhs: vec!["oi".into()], rhs: call("PickI", ["c", "x", "y"]) },
            Equation { lhs: vec!["ob".into()], rhs: call("PickB", ["bc", "p", "q"]) },
        ],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
    }
}

fn pick_project() -> Project {
    Project {
        name: "gen".into(),
        packages: vec![Package {
            name: "user".into(),
            nodes: vec![pick_template(), choose_node()],
            generics: vec![GenericNode { node: "Pick".into(), params: vec!["T".into()] }],
            instantiations: vec![
                GenericInst {
                    name: "PickI".into(),
                    generic: "Pick".into(),
                    args: vec![TypeArg { param: "T".into(), ty: Type::Int32 }],
                },
                GenericInst {
                    name: "PickB".into(),
                    generic: "Pick".into(),
                    args: vec![TypeArg { param: "T".into(), ty: Type::Bool }],
                },
            ],
            ..Default::default()
        }],
        main: Some("Choose".into()),
        ..Default::default()
    }
}

#[test]
fn monomorphize_expands_templates_and_typechecks() {
    let mut project = pick_project();
    project.monomorphize().expect("monomorphizes cleanly");

    let pkg = &project.packages[0];
    assert!(pkg.find_node("Pick").is_none(), "the generic template is dropped");
    assert!(pkg.generics.is_empty(), "generic declarations are consumed");
    assert!(pkg.instantiations.is_empty(), "instantiations are consumed");

    let pick_i = pkg.find_node("PickI").expect("int32 instance");
    let pick_b = pkg.find_node("PickB").expect("bool instance");
    // `T` is substituted in the ports of each concrete node.
    assert_eq!(pick_i.inputs[1].ty, Type::Int32);
    assert_eq!(pick_i.outputs[0].ty, Type::Int32);
    assert_eq!(pick_b.inputs[1].ty, Type::Bool);
    assert_eq!(pick_b.outputs[0].ty, Type::Bool);

    let report = ol_typecheck::check_project(&project);
    assert!(
        !report.has_errors(),
        "typecheck errors: {:?}",
        report.errors().map(|d| d.render()).collect::<Vec<_>>()
    );
}

#[test]
fn generic_pick_simulates_at_each_type() {
    let mut project = pick_project();
    project.monomorphize().unwrap();
    let mut sim = Sim::new(&project, "Choose").unwrap();

    // c selects a/b for the int pick; bc selects p/q for the bool pick.
    let cases = [
        // (c, x, y, bc, p, q) -> (oi, ob)
        (true, 10, 20, false, true, false, 10, false),
        (false, 3, 7, true, true, false, 7, true),
        (true, 0, 9, false, false, true, 0, true),
    ];
    for (c, x, y, bc, p, q, oi, ob) in cases {
        let mut inputs = BTreeMap::new();
        inputs.insert("c".into(), Value::Bool(c));
        inputs.insert("x".into(), Value::Int(x));
        inputs.insert("y".into(), Value::Int(y));
        inputs.insert("bc".into(), Value::Bool(bc));
        inputs.insert("p".into(), Value::Bool(p));
        inputs.insert("q".into(), Value::Bool(q));
        let out = sim.step(&inputs).unwrap();
        assert_eq!(out["oi"].as_int().unwrap(), oi, "int pick for c={c}");
        assert_eq!(out["ob"].as_bool().unwrap(), ob, "bool pick for bc={bc}");
    }
}

#[test]
fn monomorphize_rejects_unknown_generic() {
    let mut project = pick_project();
    project.packages[0].instantiations.push(GenericInst {
        name: "PickX".into(),
        generic: "Ghost".into(),
        args: vec![TypeArg { param: "T".into(), ty: Type::Int32 }],
    });
    let errs = project.monomorphize().unwrap_err();
    assert!(matches!(errs[0], ol_ir::MonoError::UnknownGeneric(_, _)), "{errs:?}");
}

#[test]
fn monomorphize_rejects_unbound_parameter() {
    let mut project = pick_project();
    // Drop the `T` binding from one instantiation.
    project.packages[0].instantiations[0].args.clear();
    let errs = project.monomorphize().unwrap_err();
    assert!(matches!(errs[0], ol_ir::MonoError::UnboundParam { .. }), "{errs:?}");
}

// --- Dual-backend equivalence (real toolchain) -------------------------------

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__gen_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Monomorphization yields plain nodes, so the generated C must reproduce the IR
/// trace cell-for-cell. Drive the un-monomorphized project (the CLI loader
/// expands the generics) through the scenario harness against *both* backends.
/// Uses the project's C toolchain (MSVC here, `cc` in CI).
#[test]
fn generic_ir_sim_and_generated_c_agree() {
    let tmp = make_tempdir("c");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&pick_project()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(
        scen.join("choose.csv"),
        "c,x,y,bc,p,q\ntrue,10,20,false,true,false\nfalse,3,7,true,true,false\ntrue,0,9,false,false,true\n",
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
    let (ok, out) =
        run(&["test", "record", model.to_str().unwrap(), "--scenarios", scen.to_str().unwrap()]);
    assert!(ok, "record: {out}");
    let (ok, out) = run(&[
        "test",
        "run",
        model.to_str().unwrap(),
        "--scenarios",
        scen.to_str().unwrap(),
        "--backend",
        "both",
    ]);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(ok, "run: {out}");
    assert!(out.contains("[PASS] choose (ir)"), "{out}");
    assert!(out.contains("[PASS] choose (c )"), "generic C backend diverged: {out}");
}
