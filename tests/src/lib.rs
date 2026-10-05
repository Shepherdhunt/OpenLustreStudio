//! Shared helpers for OpenLustre integration tests.
//!
//! These helpers build small projects programmatically — they are the same
//! pattern an IDE-driven model construction would use, so the tests double as
//! executable documentation of the IR construction API.

use ol_ir::{BinOp, Equation, Expr, NodeDef, NodeKind, Package, Port, Project, Type};

pub fn bool_port(name: &str) -> Port {
    Port {
        name: name.into(),
        ty: Type::Bool,
    }
}

pub fn v(name: &str) -> Expr {
    Expr::var(name)
}

pub fn build_release_logic_project() -> Project {
    let inputs = vec![
        bool_port("master_arm"),
        bool_port("station_selected"),
        bool_port("consent"),
        bool_port("fault_present"),
        bool_port("release_request"),
    ];
    let outputs = vec![bool_port("release_cmd"), bool_port("inhibit")];

    let release_cmd_rhs = Expr::and(
        Expr::and(
            Expr::and(Expr::and(v("master_arm"), v("station_selected")), v("consent")),
            Expr::not(v("fault_present")),
        ),
        v("release_request"),
    );
    let inhibit_rhs = Expr::and(v("release_request"), Expr::not(v("release_cmd")));

    let node = NodeDef {
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
        probes: vec![],
    };

    let helper = NodeDef {
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
        probes: vec![],
    };

    let contract = serde_json::json!({
        "name": "ReleaseLogic_contract",
        "inputs": inputs,
        "outputs": outputs,
        "ghost_vars": [],
        "assumptions": [],
        "guarantees": [
            { "name": "release_implies_arm",
              "expr": Expr::implies(v("release_cmd"), v("master_arm")) },
            { "name": "release_excludes_fault",
              "expr": Expr::implies(v("release_cmd"), Expr::not(v("fault_present"))) },
        ],
        "modes": [
            {
                "name": "SafeInhibit",
                "requires": [ v("release_request"), v("fault_present") ],
                "ensures":  [ Expr::not(v("release_cmd")), v("inhibit") ]
            },
            {
                "name": "AuthorizedRelease",
                "requires": [
                    v("release_request"), v("master_arm"),
                    v("station_selected"), v("consent"),
                    Expr::not(v("fault_present"))
                ],
                "ensures":  [ v("release_cmd"), Expr::not(v("inhibit")) ]
            },
            {
                "name": "Idle",
                "requires": [ Expr::not(v("release_request")) ],
                "ensures":  [ Expr::not(v("release_cmd")) ]
            }
        ],
        "imports": []
    });

    let pkg = Package {
        name: "release".into(),
        nodes: vec![node, helper],
        contracts: vec![contract],
        ..Default::default()
    };
    Project {
        name: "release_authorization_test".into(),
        packages: vec![pkg],
        main: Some("ReleaseLogic".into()),
        ..Default::default()
    }
}

/// A name part for a scratch folder that no other call returns, in this
/// test process or another running alongside: the process id, a counter,
/// and the clock. The clock alone is not enough — on macOS it advances in
/// whole microseconds, so two tests starting together got the same folder
/// and one deleted the other's files.
pub fn unique_stamp() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{}_{}_{nanos}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}
