//! What a Kind 2 run left unsettled.
//!
//! Kind 2 reports only the properties it reached. Stopped at the wall-clock
//! timeout, it leaves the others out of its report — some of them, or on a
//! large model all of them — and read as is, that looked like fewer checks
//! ("0 of 0 runtime-error checks hold") or like a broken run ("Kind 2
//! reported no properties"). [`complete`] puts back what was asked and not
//! answered, so a timeout always reads as *unknown*.

use std::collections::BTreeSet;

use ol_cocospec_emit::kind2::Kind2Input;
use ol_kind2::{Kind2Result, PropertyResult};

/// What Kind 2 had not reached when it stopped.
#[derive(Debug, Default)]
pub struct Unreached {
    /// Runtime-error checks it never reported: added to the result as
    /// `unknown`.
    pub checks: usize,
    /// Contracts none of whose properties it reported. Kind 2 names a
    /// contract's properties per call instance, so they cannot be listed one
    /// by one before it does; the contract is named instead.
    pub contracts: Vec<String>,
}

/// After Kind 2 ran `input` — on `project`, restricted to the properties
/// named in `requested` when that is not empty — and stopped at the
/// timeout: list every runtime-error check it did not report as `unknown`,
/// and name the contracts of which it reported nothing. A run that finished
/// reported everything and is left as it is.
pub fn complete(result: &mut Kind2Result, input: &Kind2Input, project: &ol_ir::Project, requested: &[String]) -> Unreached {
    let mut out = Unreached::default();
    if !result.timed_out {
        return out;
    }
    let reported: BTreeSet<String> = result.properties.iter().map(|p| p.name.clone()).collect();
    for check in &input.checks {
        let asked = requested.is_empty() || requested.iter().any(|r| r == &check.name);
        if asked && !reported.contains(&check.name) {
            result.properties.push(PropertyResult {
                name: check.name.clone(),
                status: "unknown".into(),
                scope: None,
                source: Some("PropAnnot".into()),
                counterexample: None,
                witness: None,
                line: None,
                label: check.name.clone(),
            });
            out.checks += 1;
        }
    }
    if requested.is_empty() {
        let contracts: BTreeSet<&str> = project
            .packages
            .iter()
            .flat_map(|p| &p.nodes)
            .filter_map(|n| n.contract.as_deref())
            .collect();
        for contract in contracts {
            // `PMS_contract.REL_1`, `Balance.Abs.Abs_contract.magnitude #2`.
            let mentions = |p: &PropertyResult| p.display_name().split('.').any(|part| part.split(' ').next() == Some(contract));
            if !result.properties.iter().any(mentions) {
                out.contracts.push(contract.to_string());
            }
        }
    }
    out
}

/// What to try when Kind 2 runs out of time, in a sentence — for a proof
/// with or without the runtime-error checks.
pub fn timeout_advice(runtime_errors: bool) -> &'static str {
    if runtime_errors {
        "raise the timeout, prove one operator at a time, or prove the contracts without the runtime-error checks"
    } else {
        "raise the timeout, or prove one operator at a time"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn property(name: &str, status: &str) -> PropertyResult {
        PropertyResult {
            name: name.into(),
            status: status.into(),
            scope: None,
            source: None,
            counterexample: None,
            witness: None,
            line: None,
            label: ol_kind2::display_name(name),
        }
    }

    fn project() -> ol_ir::Project {
        serde_json::from_value(serde_json::json!({
            "name": "p",
            "packages": [{"name": "user", "nodes": [
                {"name": "Top", "kind": "Operator", "inputs": [], "outputs": [], "contract": "Top_contract"},
                {"name": "Abs", "kind": "Function", "inputs": [], "outputs": [], "contract": "Abs_contract"},
                {"name": "Plain", "kind": "Function", "inputs": [], "outputs": []}
            ]}]
        }))
        .unwrap()
    }

    fn input(checks: &[&str]) -> Kind2Input {
        let mut input = ol_cocospec_emit::kind2::Kind2Input::default();
        for name in checks {
            input.checks.push(ol_cocospec_emit::rte::RteCheck {
                name: (*name).into(),
                kind: ol_cocospec_emit::rte::RteKind::Overflow,
                node: "Top".into(),
                path: String::new(),
                what: "x + 1 fits int32".into(),
            });
        }
        input
    }

    #[test]
    fn a_timeout_lists_the_unreported_checks_as_unknown_and_names_silent_contracts() {
        let mut result = Kind2Result {
            timed_out: true,
            properties: vec![property("rte1", "valid"), property("Top.Abs.Abs_contract[l3c1].magnitude[l4c3] #1", "unknown")],
            ..Default::default()
        };
        let u = complete(&mut result, &input(&["rte1", "rte2", "rte3"]), &project(), &[]);
        assert_eq!(u.checks, 2);
        assert_eq!(u.contracts, vec!["Top_contract".to_string()]);
        let names: Vec<_> = result.properties.iter().map(|p| (p.name.as_str(), p.outcome())).collect();
        assert!(names.contains(&("rte2", ol_kind2::Outcome::Unknown)) && names.contains(&("rte3", ol_kind2::Outcome::Unknown)));
        assert_eq!(result.properties.len(), 4, "rte1 is not listed twice");
    }

    #[test]
    fn nothing_reported_at_all_becomes_every_check_unknown() {
        let mut result = Kind2Result { timed_out: true, ..Default::default() };
        let u = complete(&mut result, &input(&["rte1", "rte2"]), &project(), &[]);
        assert_eq!((u.checks, result.properties.len()), (2, 2));
        assert_eq!(u.contracts, vec!["Abs_contract".to_string(), "Top_contract".to_string()]);
    }

    #[test]
    fn a_finished_run_and_unrequested_checks_are_left_alone() {
        let mut finished = Kind2Result { properties: vec![property("rte1", "valid")], ..Default::default() };
        let u = complete(&mut finished, &input(&["rte1", "rte2"]), &project(), &[]);
        assert_eq!((u.checks, finished.properties.len()), (0, 1));
        // `--property rte1`: rte2 was not asked for.
        let mut only = Kind2Result { timed_out: true, ..Default::default() };
        let u = complete(&mut only, &input(&["rte1", "rte2"]), &project(), &["rte1".into()]);
        assert_eq!((u.checks, u.contracts.len()), (1, 0));
        assert_eq!(only.properties[0].name, "rte1");
    }
}
