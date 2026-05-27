//! Loads the real `libraries/` tree shipped in the repository and asserts that
//! every standard-library block lowers to IR and passes type + contract checks.
//! This is the regression guard for the standard library: adding a malformed
//! block, or breaking the textual expression parser, fails this test.

use std::path::PathBuf;

use ol_ir::Severity;

fn libraries_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries")
}

#[test]
fn standard_library_loads_and_checks_clean() {
    let dir = libraries_dir();
    let lib = ol_stdlib::load_dir(&dir).expect("library loads");

    assert!(
        lib.entries.len() >= 20,
        "expected the expanded block library, found only {} blocks",
        lib.entries.len()
    );

    let errors: Vec<String> = lib
        .check()
        .into_iter()
        .filter(|d| matches!(d.severity, Severity::Error))
        .map(|d| d.render())
        .collect();
    assert!(
        errors.is_empty(),
        "standard library has check errors:\n{}",
        errors.join("\n")
    );
}

#[test]
fn expected_blocks_are_present() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("library loads");
    let names: Vec<&str> = lib.nodes().map(|n| n.name.as_str()).collect();
    for expected in [
        "And", "Or", "Not", "Xor", "Mux", "Switch", "Add", "Subtract", "Multiply", "Divide",
        "Min", "Max", "Clamp", "Saturate", "Equal", "Less", "RisingEdge", "FallingEdge", "Latch",
        "Delay", "Counter",
    ] {
        assert!(names.contains(&expected), "missing block `{expected}`");
    }
}

#[test]
fn every_node_with_a_contract_links_to_a_real_contract() {
    let lib = ol_stdlib::load_dir(&libraries_dir()).expect("library loads");
    let contract_names: Vec<&str> = lib.contracts().map(|c| c.name.as_str()).collect();
    for node in lib.nodes() {
        if let Some(c) = &node.contract {
            assert!(
                contract_names.contains(&c.as_str()),
                "node `{}` references contract `{}` which was not loaded",
                node.name,
                c
            );
        }
    }
}
