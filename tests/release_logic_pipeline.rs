//! End-to-end pipeline test for the ReleaseLogic MVP: build the project,
//! type-check, contract-check, emit Lustre/CoCoSpec/C-Lite, and simulate.

use std::collections::BTreeMap;

use ol_clite_emit::monitor;
use ol_cocospec_emit::Target;
use ol_sim::Value;
use openlustre_integration_tests::build_release_logic_project;

#[test]
fn pipeline_succeeds_on_release_logic() {
    let project = build_release_logic_project();

    let tc = ol_typecheck::check_project(&project);
    let tc_errors: Vec<_> = tc.errors().collect();
    assert!(
        tc_errors.is_empty(),
        "type checker reported errors: {:#?}",
        tc_errors
    );

    let cc = ol_contract_check::check_project(&project);
    let cc_errors: Vec<_> = cc
        .diagnostics
        .iter()
        .filter(|d| matches!(d.severity, ol_ir::Severity::Error))
        .collect();
    assert!(
        cc_errors.is_empty(),
        "contract checker reported errors: {:#?}",
        cc_errors
    );

    let lustre = ol_lustre_emit::emit_project(&project);
    assert!(lustre.contains("node ReleaseLogic"));
    assert!(lustre.contains("release_cmd ="));

    let modern = ol_cocospec_emit::emit_project(&project, Target::Modern);
    assert!(modern.contains("contract ReleaseLogic_contract"));
    assert!(modern.contains("mode AuthorizedRelease"));

    let legacy = ol_cocospec_emit::emit_project(&project, Target::Legacy);
    assert!(legacy.contains("(*@contract"));

    let bundle = ol_clite_emit::emit_project(&project);
    assert!(bundle.header.contains("ReleaseLogic_Input"));
    assert!(bundle.source.contains("ReleaseLogic_step"));

    let monitors = monitor::emit_monitors(&project);
    assert!(monitors.source.contains("ReleaseLogic_contract_monitor_check"));
}

#[test]
fn simulator_matches_expected_outputs_and_modes() {
    let project = build_release_logic_project();
    let mut sim = ol_sim::Sim::new(&project, "ReleaseLogic").unwrap();

    let scenarios: Vec<(BTreeMap<&str, bool>, bool, bool)> = vec![
        // (inputs, expected release_cmd, expected inhibit)
        (BTreeMap::from([
            ("master_arm", false), ("station_selected", false),
            ("consent", false), ("fault_present", false), ("release_request", false),
        ]), false, false),
        (BTreeMap::from([
            ("master_arm", true), ("station_selected", true),
            ("consent", true), ("fault_present", false), ("release_request", true),
        ]), true, false),
        (BTreeMap::from([
            ("master_arm", true), ("station_selected", true),
            ("consent", true), ("fault_present", true), ("release_request", true),
        ]), false, true),
    ];

    for (i, (inputs, expected_cmd, expected_inhibit)) in scenarios.into_iter().enumerate() {
        let mut as_btree: BTreeMap<String, Value> = BTreeMap::new();
        for (k, v) in inputs {
            as_btree.insert(k.into(), Value::Bool(v));
        }
        let out = sim.step(&as_btree).unwrap();
        assert_eq!(
            out.get("release_cmd"),
            Some(&Value::Bool(expected_cmd)),
            "scenario {i}: release_cmd"
        );
        assert_eq!(
            out.get("inhibit"),
            Some(&Value::Bool(expected_inhibit)),
            "scenario {i}: inhibit"
        );
    }
}
