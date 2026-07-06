//! `openlustre doc`: the design-document generator (the SCADE Report
//! Generator role) — content, determinism, and the traceability section.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn doc_renders_the_release_logic_design_document() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let tmp = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_doc_{stamp}"));
    std::fs::create_dir_all(&tmp).unwrap();
    let model = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/release_logic/model/release_logic.json");

    let run = |out: &std::path::Path| {
        let o = Command::new(env!("CARGO"))
            .args(["run", "-q", "-p", "ol_cli", "--", "doc"])
            .arg(&model)
            .args(["-o", out.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        std::fs::read_to_string(out).unwrap()
    };
    let a = run(&tmp.join("a.html"));
    let b = run(&tmp.join("b.html"));
    assert_eq!(a, b, "the document must be deterministic");

    // The report carries every design view of the operator.
    for needle in [
        "release_authorization — design document",
        "<h2 id=\"op-ReleaseLogic\">",          // per-operator section
        "master_arm",                            // interface table
        "<svg",                                  // schematic
        "inhibit = release_request and not release_cmd;", // behavior as Lustre
        "Contract: ReleaseLogic_contract",       // CoCoSpec section
        "mode SafeInhibit (",
        "Requirements traceability",
    ] {
        assert!(a.contains(needle), "missing {needle}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
