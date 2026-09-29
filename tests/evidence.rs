//! The evidence report (`openlustre evidence`): one document per operator
//! with static checks, contract, Kind 2 proof, tests and coverage, model ≡
//! generated code, and traceability — each section with a status, and an
//! overall verdict that fails the command when any section fails.

use std::path::{Path, PathBuf};
use std::process::Command;

fn example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/release_logic")
}

fn tempdir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_evidence_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn evidence(model: &Path, scenarios: &Path, out: &Path, extra: &[&str]) -> (bool, String, serde_json::Value) {
    let o = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "evidence"])
        .arg(model)
        .arg("--scenarios")
        .arg(scenarios)
        .arg("--out")
        .arg(out)
        .args(extra)
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let json = std::fs::read_to_string(out.join("evidence_ReleaseLogic.json"))
        .map(|t| serde_json::from_str(&t).unwrap())
        .unwrap_or(serde_json::Value::Null);
    (o.status.success(), text, json)
}

fn status(ev: &serde_json::Value, id: &str) -> String {
    ev["sections"].as_array().unwrap().iter().find(|s| s["id"] == id).unwrap()["status"].as_str().unwrap().to_string()
}

fn has_cc() -> bool {
    ["cc", "gcc", "clang"].iter().any(|c| Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
}

#[test]
fn a_clean_operator_passes_with_gaps_when_proof_is_not_run() {
    let out = tempdir("clean");
    let ex = example();
    let (ok, text, ev) = evidence(&ex.join("model/release_logic.json"), &ex.join("scenarios"), &out, &[]);
    assert!(ok, "{text}");
    assert_eq!(status(&ev, "checks"), "pass");
    assert_eq!(status(&ev, "contract"), "pass");
    assert_eq!(status(&ev, "proof"), "not_run", "not requested");
    assert_eq!(status(&ev, "tests"), "pass");
    // No `if` decisions, and every MC/DC condition shown: complete.
    assert_eq!(status(&ev, "coverage"), "pass", "{text}");
    assert_eq!(status(&ev, "traceability"), "pass");
    if has_cc() {
        assert_eq!(status(&ev, "equivalence"), "pass");
        assert!(ev["equivalence"]["cycles_compared"].as_u64().unwrap() > 0);
    }
    assert_eq!(ev["verdict"], "PASS WITH GAPS");
    // Identity: the model file's SHA-256 and the operator's fingerprint.
    let model_bytes = std::fs::read(ex.join("model/release_logic.json")).unwrap();
    assert_eq!(ev["identity"]["model_files"][0]["sha256"], ol_clite_emit::trace::sha256_hex(&model_bytes));
    assert_eq!(ev["identity"]["fingerprint"].as_str().unwrap().len(), 64);
    assert!(ev["generated_at"].as_str().unwrap().ends_with('Z'));
    // The HTML page carries the same.
    let html = std::fs::read_to_string(out.join("evidence_ReleaseLogic.html")).unwrap();
    assert!(html.contains("<title>Evidence — ReleaseLogic</title>"));
    assert!(html.contains("PASS WITH GAPS") && html.contains("contract ReleaseLogic_contract("));
    assert!(html.contains(ev["identity"]["fingerprint"].as_str().unwrap()));
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_test_that_disagrees_with_its_golden_fails_the_evidence() {
    let out = tempdir("fail");
    let ex = example();
    let scen = out.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    for f in std::fs::read_dir(ex.join("scenarios")).unwrap() {
        let f = f.unwrap().path();
        std::fs::copy(&f, scen.join(f.file_name().unwrap())).unwrap();
    }
    // Flip release_cmd on the first row of one golden trace.
    let g = scen.join("authorized_release.golden.csv");
    let text = std::fs::read_to_string(&g).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let col = lines[0].split(',').position(|h| h == "release_cmd").unwrap();
    let mut cells: Vec<String> = lines[1].split(',').map(str::to_string).collect();
    cells[col] = if cells[col] == "true" { "false".into() } else { "true".into() };
    lines[1] = cells.join(",");
    std::fs::write(&g, lines.join("\n") + "\n").unwrap();

    let (ok, text, ev) = evidence(&ex.join("model/release_logic.json"), &scen, &out, &[]);
    assert!(!ok, "a FAIL verdict fails the command: {text}");
    assert_eq!(ev["verdict"], "FAIL", "{text}");
    assert_eq!(status(&ev, "tests"), "fail");
    let _ = std::fs::remove_dir_all(&out);
}

#[cfg(unix)]
#[test]
fn proof_results_come_from_kind2() {
    use std::os::unix::fs::PermissionsExt;
    let out = tempdir("prove");
    let ex = example();
    let script = |name: &str, answer: &str| -> PathBuf {
        let p = out.join(name);
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\ncat <<'JSON'\n[{{\"objectType\": \"property\", \"name\": \"ReleaseLogic_contract.guarantee[a]\", \"answer\": {{\"value\": \"valid\"}}}},\n{{\"objectType\": \"property\", \"name\": \"ReleaseLogic_contract.guarantee[b]\", \"answer\": {{\"value\": \"{answer}\"}}}}]\nJSON\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let good = script("kind2_valid", "valid");
    let (_, text, ev) = evidence(&ex.join("model/release_logic.json"), &ex.join("scenarios"), &out, &["--prove", "--kind2", good.to_str().unwrap()]);
    assert_eq!(status(&ev, "proof"), "pass", "{text}");
    assert_eq!(ev["proof"]["properties"].as_array().unwrap().len(), 2);
    let bad = script("kind2_falsified", "falsifiable");
    let (ok, text, ev) = evidence(&ex.join("model/release_logic.json"), &ex.join("scenarios"), &out, &["--prove", "--kind2", bad.to_str().unwrap()]);
    assert!(!ok, "{text}");
    assert_eq!(status(&ev, "proof"), "fail", "{text}");
    // Kind 2 missing: not run, not a failure.
    let (_, text, ev) = evidence(&ex.join("model/release_logic.json"), &ex.join("scenarios"), &out, &["--prove", "--kind2", "/nonexistent/kind2"]);
    assert_eq!(status(&ev, "proof"), "not_run", "{text}");
    let _ = std::fs::remove_dir_all(&out);
}
