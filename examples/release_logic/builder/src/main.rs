//! Construct the ReleaseLogic MVP project programmatically and serialize it
//! to JSON. This avoids the noise of hand-writing IR trees in YAML.

use ol_ir::{BinOp, Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};

fn bool_port(name: &str) -> Port {
    Port {
        name: name.into(),
        ty: Type::Bool,
    }
}

fn v(name: &str) -> Expr {
    Expr::var(name)
}
fn and(a: Expr, b: Expr) -> Expr {
    Expr::and(a, b)
}
fn not(a: Expr) -> Expr {
    Expr::not(a)
}
fn impl_(a: Expr, b: Expr) -> Expr {
    Expr::implies(a, b)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let inputs = vec![
        bool_port("master_arm"),
        bool_port("station_selected"),
        bool_port("consent"),
        bool_port("fault_present"),
        bool_port("release_request"),
    ];
    let outputs = vec![bool_port("release_cmd"), bool_port("inhibit")];

    // release_cmd = master_arm and station_selected and consent
    //               and not fault_present and release_request
    let release_cmd_rhs = and(
        and(
            and(and(v("master_arm"), v("station_selected")), v("consent")),
            not(v("fault_present")),
        ),
        v("release_request"),
    );
    // inhibit = release_request and not release_cmd
    let inhibit_rhs = and(v("release_request"), not(v("release_cmd")));

    let release_logic = NodeDef {
        name: "ReleaseLogic".into(),
        kind: NodeKind::Operator,
        inputs: inputs.clone(),
        outputs: outputs.clone(),
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["release_cmd".into()],
                rhs: release_cmd_rhs,
            },
            Equation {
                lhs: vec!["inhibit".into()],
                rhs: inhibit_rhs,
            },
        ],
        contract: Some("ReleaseLogic_contract".into()),
        diagram: Default::default(),
    };

    // Pure helper to show off the function/operator distinction.
    let armed_and_consented = NodeDef {
        name: "ArmedAndConsented".into(),
        kind: NodeKind::Function,
        inputs: vec![bool_port("master_arm"), bool_port("consent")],
        outputs: vec![bool_port("armed_consented")],
        locals: vec![],
        equations: vec![Equation {
            lhs: vec!["armed_consented".into()],
            rhs: Expr::bin(BinOp::And, v("master_arm"), v("consent")),
        }],
        contract: None,
        diagram: Default::default(),
    };

    let contract = serde_json::json!({
        "name": "ReleaseLogic_contract",
        "inputs": inputs,
        "outputs": outputs,
        "ghost_vars": [],
        "assumptions": [],
        "guarantees": [
            { "name": "release_implies_arm",
              "expr": impl_(v("release_cmd"), v("master_arm")) },
            { "name": "release_implies_station",
              "expr": impl_(v("release_cmd"), v("station_selected")) },
            { "name": "release_implies_consent",
              "expr": impl_(v("release_cmd"), v("consent")) },
            { "name": "release_excludes_fault",
              "expr": impl_(v("release_cmd"), not(v("fault_present"))) },
        ],
        "modes": [
            {
                "name": "SafeInhibit",
                "requires": [ v("release_request"), v("fault_present") ],
                "ensures":  [ not(v("release_cmd")), v("inhibit") ]
            },
            {
                "name": "AuthorizedRelease",
                "requires": [
                    v("release_request"), v("master_arm"),
                    v("station_selected"), v("consent"),
                    not(v("fault_present"))
                ],
                "ensures":  [ v("release_cmd"), not(v("inhibit")) ]
            },
            {
                "name": "Idle",
                "requires": [ not(v("release_request")) ],
                "ensures":  [ not(v("release_cmd")) ]
            }
        ],
        "imports": []
    });

    let pkg = Package {
        name: "release".into(),
        nodes: vec![release_logic, armed_and_consented],
        contracts: vec![contract],
        ..Default::default()
    };

    let project = Project {
        name: "release_authorization".into(),
        packages: vec![pkg],
        main: Some("ReleaseLogic".into()),
    };

    let json = serde_json::to_string_pretty(&project)?;
    let model_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../model");
    std::fs::create_dir_all(&model_dir)?;
    let model_path = model_dir.join("release_logic.json");
    std::fs::write(&model_path, json)?;
    println!("wrote {}", model_path.display());

    // CSV input vector exercising every mode at least once.
    let csv = "master_arm,station_selected,consent,fault_present,release_request\n\
               false,false,false,false,false\n\
               true,true,true,false,false\n\
               true,true,true,false,true\n\
               true,true,true,true,true\n\
               false,true,true,false,true\n";
    let inputs_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../inputs");
    std::fs::create_dir_all(&inputs_dir)?;
    let inputs_path = inputs_dir.join("nominal.csv");
    std::fs::write(&inputs_path, csv)?;
    println!("wrote {}", inputs_path.display());

    Ok(())
}
