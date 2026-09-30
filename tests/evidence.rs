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
fn proof_property(name: &str, answer: &str) -> serde_json::Value {
    serde_json::json!({
        "objectType": "property",
        "name": format!("ReleaseLogic_contract.guarantee[{name}]"),
        "answer": { "value": answer },
    })
}

#[cfg(unix)]
fn realizability_records(node: &str, result: &str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({ "objectType": "analysisStart", "top": node, "context": "contract" }),
        serde_json::json!({ "objectType": "realizabilityCheck", "result": result }),
    ]
}

/// Exercise the real evidence command and Kind 2 process adapter, rather
/// than constructing an already-normalized evidence JSON document. Keep
/// copies of the exact source files supplied to the two prover processes
/// so the provenance assertions can independently recompute their hashes.
#[cfg(unix)]
fn fake_kind2(
    out: &Path,
    name: &str,
    properties: &[serde_json::Value],
    proof_exit: i32,
    realizability: &[serde_json::Value],
    realizability_exit: i32,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let shell_quote = |p: PathBuf| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let p = out.join(name);
    let proof_source = shell_quote(out.join(format!("{name}_proof_input.lus")));
    let contract_source = shell_quote(out.join(format!("{name}_contract_input.lus")));
    let mut proof_records = vec![serde_json::json!({ "objectType": "analysisStart", "top": "ReleaseLogic" })];
    proof_records.extend_from_slice(properties);
    let proof_json = serde_json::to_string_pretty(&proof_records).unwrap();
    let realizability_json = serde_json::to_string_pretty(realizability).unwrap();
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then\n\
               echo 'kind2 v2.2.0'\n\
               exit 0\n\
             fi\n\
             mode=proof\n\
             for arg in \"$@\"; do\n\
               if [ \"$arg\" = CONTRACTCK ]; then mode=contract; fi\n\
               source_file=$arg\n\
             done\n\
             if [ \"$mode\" = contract ]; then\n\
               cp \"$source_file\" {contract_source}\n\
               echo 'contract stderr retained' >&2\n\
               cat <<'REALIZABILITY_JSON'\n\
             {realizability_json}\n\
             REALIZABILITY_JSON\n\
               exit {realizability_exit}\n\
             fi\n\
             cp \"$source_file\" {proof_source}\n\
             echo 'proof stderr retained' >&2\n\
             cat <<'PROOF_JSON'\n\
             {proof_json}\n\
             PROOF_JSON\n\
             exit {proof_exit}\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[cfg(unix)]
fn replace_fake_proof_stdout(fake: &Path, stdout: &str) {
    let mut script = std::fs::read_to_string(fake).unwrap();
    let start = script.find("cat <<'PROOF_JSON'\n").unwrap() + "cat <<'PROOF_JSON'\n".len();
    let end = start + script[start..].find("\nPROOF_JSON\n").unwrap();
    script.replace_range(start..end, stdout);
    std::fs::write(fake, script).unwrap();
}

#[cfg(unix)]
fn proof_with_fake(out: &Path, fake: &Path) -> (bool, String, serde_json::Value) {
    let ex = example();
    evidence(
        &ex.join("model/release_logic.json"),
        &ex.join("scenarios"),
        out,
        &["--prove", "--kind2", fake.to_str().unwrap()],
    )
}

#[cfg(unix)]
fn prove_with_fake(out: &Path, fake: &Path, mode: &str) -> (bool, String) {
    let o = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "prove"])
        .arg(example().join("model/release_logic.json"))
        .args(["--node", "ReleaseLogic", "--mode", mode, "--kind2"])
        .arg(fake)
        .arg("--workdir")
        .arg(out.join("direct_prove"))
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    (o.status.success(), text)
}

#[cfg(unix)]
fn assert_raw_run<'a>(ev: &'a serde_json::Value, out: &Path, fake: &Path, mode: &str) -> &'a serde_json::Value {
    let runs = ev["proof"]["runs"].as_array().expect("raw prover runs must survive evidence production");
    let run = runs.iter().find(|r| r["mode"] == mode).unwrap_or_else(|| panic!("missing {mode} run: {runs:?}"));
    assert_eq!(run["requested_root"], "ReleaseLogic");
    let source_suffix = if mode == "Realizability" { "contract" } else { "proof" };
    let source = out.join(format!("{}_{source_suffix}_input.lus", fake.file_name().unwrap().to_str().unwrap()));
    let bytes = std::fs::read(source).unwrap();
    assert_eq!(run["source_sha256"], ol_clite_emit::trace::sha256_hex(&bytes));
    let result = &run["result"];
    assert!(result.is_object(), "raw process result missing: {run}");
    let invocation = result["invocation"].as_array().unwrap();
    assert_eq!(invocation.first().unwrap(), fake.to_str().unwrap());
    let main = invocation.iter().position(|v| v == "--lus_main").expect("root must be passed to prover");
    assert_eq!(invocation.get(main + 1).unwrap(), "ReleaseLogic");
    assert!(result["errors"].is_array());
    assert!(result["exit_code"].is_i64());
    assert!(result["timed_out"].is_boolean());
    assert!(result["stdout"].is_string());
    assert_eq!(
        result["stderr"],
        if mode == "Realizability" { "contract stderr retained\n" } else { "proof stderr retained\n" }
    );
    result
}

#[cfg(unix)]
fn assert_incomplete_proof(ev: &serde_json::Value, text: &str) {
    assert_ne!(status(ev, "proof"), "pass", "raw prover failure must not become completed proof: {text}");
    assert_ne!(ev["verdict"], "PASS", "{text}");
}

#[cfg(unix)]
#[test]
fn proof_results_come_from_kind2() {
    let out = tempdir("prove");
    let ex = example();
    let script = |name: &str, answer: &str| -> PathBuf {
        fake_kind2(
            &out,
            name,
            &[proof_property("a", "valid"), proof_property("b", answer)],
            0,
            &realizability_records("ReleaseLogic", "realizable"),
            0,
        )
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

#[cfg(unix)]
#[test]
fn completed_rows_do_not_hide_a_nonzero_prover_exit() {
    let out = tempdir("exit_failure");
    let fake = fake_kind2(&out, "kind2_exit_42", &[proof_property("a", "valid")], 42, &realizability_records("ReleaseLogic", "realizable"), 0);
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    assert_eq!(raw["exit_code"], 42);
    assert_eq!(raw["properties"][0]["status"], "valid", "retain useful partial results");
    assert!(raw["stdout"].as_str().unwrap().contains("valid"));
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn completed_rows_do_not_hide_a_fatal_wallclock_timeout() {
    let out = tempdir("timeout");
    let fake = fake_kind2(
        &out, "kind2_timeout", &[
            proof_property("a", "valid"),
            serde_json::json!({ "objectType": "log", "level": "fatal", "value": "Wallclock timeout after completed rows" }),
        ], 0, &realizability_records("ReleaseLogic", "realizable"), 0,
    );
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    assert_eq!(raw["timed_out"], true);
    assert!(raw["stdout"].as_str().unwrap().contains("Wallclock timeout"));
    assert!(ev["proof"]["note"].as_str().unwrap().to_ascii_lowercase().contains("timeout"), "timeout must remain visible even when every row holds: {ev}");
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn a_later_opposite_conclusive_result_is_not_discarded() {
    let out = tempdir("contradiction");
    let fake = fake_kind2(&out, "kind2_contradiction", &[proof_property("a", "valid"), proof_property("a", "falsifiable")], 0, &realizability_records("ReleaseLogic", "realizable"), 0);
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    let stdout = raw["stdout"].as_str().unwrap();
    assert!(stdout.contains("valid") && stdout.contains("falsifiable"));
    assert!(raw["errors"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("ReleaseLogic_contract.guarantee[a]")), "the contradictory property must be identified: {raw}");
    assert!(ev["proof"]["properties"].as_array().unwrap().iter().all(|p| p["holds"] != true), "a contradictory result cannot retain a proved row: {ev}");
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn a_helpers_realizability_does_not_prove_the_requested_root() {
    let out = tempdir("wrong_root");
    let mut realizability = realizability_records("UnrelatedHelper", "realizable");
    realizability.extend(realizability_records("ReleaseLogic", "unknown"));
    let fake = fake_kind2(&out, "kind2_wrong_root", &[proof_property("a", "valid")], 0, &realizability, 0);
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    assert_eq!(ev["proof"]["realizability"], "unknown");
    let raw = assert_raw_run(&ev, &out, &fake, "Realizability");
    assert_eq!(raw["realizability"].as_array().unwrap().len(), 2);
    assert_eq!(raw["realizability"][0]["node"], "UnrelatedHelper");
    assert_eq!(raw["realizability"][1]["node"], "ReleaseLogic");
    assert_eq!(raw["realizability"][1]["result"], "unknown");
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn a_fatal_realizability_error_does_not_hide_behind_a_realizable_result() {
    let out = tempdir("realizability_error");
    let mut realizability = realizability_records("ReleaseLogic", "realizable");
    realizability.push(serde_json::json!({ "objectType": "log", "level": "fatal", "value": "injected fatal realizability failure" }));
    let fake = fake_kind2(&out, "kind2_realizability_error", &[proof_property("a", "valid")], 0, &realizability, 0);
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "Realizability");
    assert!(raw["errors"].as_array().unwrap().iter().any(|e| e == "injected fatal realizability failure"));
    assert!(raw["stdout"].as_str().unwrap().contains("realizable"));
    assert!(ev["proof"]["note"].as_str().unwrap().contains("injected fatal realizability failure"));
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn a_nonzero_realizability_exit_does_not_hide_behind_a_realizable_result() {
    let out = tempdir("realizability_exit");
    let fake = fake_kind2(&out, "kind2_realizability_exit", &[proof_property("a", "valid")], 0, &realizability_records("ReleaseLogic", "realizable"), 42);
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "Realizability");
    assert_eq!(raw["exit_code"], 42);
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn inconclusive_progress_updates_do_not_invalidate_a_valid_property() {
    let out = tempdir("progress");
    let fake = fake_kind2(&out, "kind2_progress", &[proof_property("a", "unknown"), proof_property("a", "valid"), proof_property("a", "unknown")], 0, &realizability_records("ReleaseLogic", "realizable"), 0);
    let (ok, text, ev) = proof_with_fake(&out, &fake);
    assert!(ok, "{text}");
    assert_eq!(status(&ev, "proof"), "pass", "{text}");
    assert_eq!(ev["proof"]["properties"].as_array().unwrap().len(), 1);
    assert_eq!(ev["proof"]["properties"][0]["holds"], true);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    assert!(raw["errors"].as_array().unwrap().is_empty());
    assert_eq!(raw["properties"][0]["status"], "valid");
    assert_eq!(raw["stdout"].as_str().unwrap().matches("unknown").count(), 2, "keep ordinary progress in raw output");
    assert_raw_run(&ev, &out, &fake, "Realizability");
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn a_root_realizable_result_is_found_after_an_unrelated_helpers_result() {
    let out = tempdir("root_after_helper");
    let mut realizability = realizability_records("UnrelatedHelper", "unrealizable");
    realizability.extend(realizability_records("ReleaseLogic", "realizable"));
    let fake = fake_kind2(&out, "kind2_root_after_helper", &[proof_property("a", "valid")], 0, &realizability, 0);
    let (ok, text, ev) = proof_with_fake(&out, &fake);
    assert!(ok, "{text}");
    assert_eq!(status(&ev, "proof"), "pass", "{text}");
    assert_eq!(ev["proof"]["realizability"], "realizable");
    let raw = assert_raw_run(&ev, &out, &fake, "Realizability");
    assert_eq!(raw["realizability"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn opposite_conclusive_results_remain_conflicting_after_later_unknown_progress() {
    let out = tempdir("reverse_contradiction");
    let fake = fake_kind2(
        &out, "kind2_reverse_contradiction",
        &[proof_property("a", "falsifiable"), proof_property("a", "valid"), proof_property("a", "unknown")],
        0, &realizability_records("ReleaseLogic", "realizable"), 0,
    );
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    assert_eq!(raw["properties"][0]["status"], "conflicting");
    assert_eq!(raw["properties"][0]["reported_statuses"], serde_json::json!(["falsifiable", "valid", "unknown"]));
    assert!(raw["errors"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("ReleaseLogic_contract.guarantee[a]")));
    assert!(ev["proof"]["properties"].as_array().unwrap().iter().all(|p| p["holds"] != true));
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn repeated_valid_results_are_legitimate_progress() {
    let out = tempdir("duplicate_valid");
    let fake = fake_kind2(
        &out, "kind2_duplicate_valid", &[proof_property("a", "valid"), proof_property("a", "valid")],
        0, &realizability_records("ReleaseLogic", "realizable"), 0,
    );
    let (ok, text, ev) = proof_with_fake(&out, &fake);
    assert!(ok, "{text}");
    assert_eq!(status(&ev, "proof"), "pass", "{text}");
    assert_eq!(ev["proof"]["properties"].as_array().unwrap().len(), 1);
    assert_eq!(ev["proof"]["properties"][0]["holds"], true);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    assert!(raw["errors"].as_array().unwrap().is_empty());
    assert_eq!(raw["properties"][0]["reported_statuses"], serde_json::json!(["valid", "valid"]));
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn a_fatal_engine_runtime_failure_invalidates_completed_proof() {
    let out = tempdir("engine_runtime_failure");
    let failure = "Runtime failure in IC3: injected fatal prover failure";
    let fake = fake_kind2(
        &out, "kind2_runtime_failure", &[
            proof_property("a", "valid"),
            serde_json::json!({ "objectType": "log", "level": "fatal", "value": failure }),
        ], 0, &realizability_records("ReleaseLogic", "realizable"), 0,
    );
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    assert!(raw["errors"].as_array().unwrap().iter().any(|e| e == failure));
    assert_eq!(raw["properties"][0]["status"], "valid", "retain useful partial rows");
    assert!(ev["proof"]["note"].as_str().unwrap().contains(failure));
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn a_root_named_property_from_an_unrelated_analysis_does_not_prove_the_root() {
    let out = tempdir("property_wrong_root");
    let fake = fake_kind2(
        &out, "kind2_property_wrong_root", &[proof_property("a", "valid")],
        0, &realizability_records("ReleaseLogic", "realizable"), 0,
    );
    let stdout = serde_json::to_string(&vec![
        serde_json::json!({ "objectType": "analysisStart", "top": "UnrelatedHelper" }),
        proof_property("a", "valid"),
    ]).unwrap();
    replace_fake_proof_stdout(&fake, &stdout);
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    assert_eq!(raw["properties"][0]["reported_analysis_tops"], serde_json::json!(["UnrelatedHelper"]));
    assert!(raw["errors"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("ReleaseLogic_contract.guarantee[a]")), "root mismatch must identify the affected property: {raw}");
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn a_malformed_ndjson_trailer_does_not_hide_behind_a_valid_property() {
    let out = tempdir("malformed_ndjson");
    let fake = fake_kind2(
        &out, "kind2_malformed_ndjson", &[proof_property("a", "valid")],
        0, &realizability_records("ReleaseLogic", "realizable"), 0,
    );
    let stdout = format!(
        "{}\n{}\n{{\"objectType\":",
        serde_json::json!({ "objectType": "analysisStart", "top": "ReleaseLogic" }),
        proof_property("a", "valid"),
    );
    replace_fake_proof_stdout(&fake, &stdout);
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
    assert!(raw["stdout"].as_str().unwrap().ends_with("{\"objectType\":\n"));
    assert!(raw["errors"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("malformed")));
    assert_eq!(raw["properties"][0]["status"], "valid", "retain parsed partial rows beside the parse error");
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn inconclusive_realizability_updates_do_not_invalidate_a_realizable_root() {
    let out = tempdir("realizability_progress");
    let mut realizability = realizability_records("ReleaseLogic", "unknown");
    realizability.extend(realizability_records("ReleaseLogic", "realizable"));
    realizability.extend(realizability_records("ReleaseLogic", "unknown"));
    let fake = fake_kind2(
        &out, "kind2_realizability_progress", &[proof_property("a", "valid")],
        0, &realizability, 0,
    );
    let (ok, text, ev) = proof_with_fake(&out, &fake);
    assert!(ok, "{text}");
    assert_eq!(status(&ev, "proof"), "pass", "{text}");
    assert_eq!(ev["proof"]["realizability"], "realizable");
    let raw = assert_raw_run(&ev, &out, &fake, "Realizability");
    assert!(raw["errors"].as_array().unwrap().is_empty());
    let updates: Vec<_> = raw["realizability"].as_array().unwrap().iter().map(|r| r["result"].as_str().unwrap()).collect();
    assert_eq!(updates, vec!["unknown", "realizable", "unknown"]);
    assert!(raw["realizability"].as_array().unwrap().iter().all(|r| r["node"] == "ReleaseLogic" && r["context"] == "contract"));
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn malformed_json_array_members_do_not_hide_behind_valid_properties() {
    for (tag, member) in [("non_object", serde_json::json!(42)), ("object_without_type", serde_json::json!({ "injected": "malformed record" }))] {
        let out = tempdir(tag);
        let fake = fake_kind2(
            &out, "kind2_malformed_array", &[proof_property("a", "valid"), member],
            0, &realizability_records("ReleaseLogic", "realizable"), 0,
        );
        let (_, text, ev) = proof_with_fake(&out, &fake);
        assert_incomplete_proof(&ev, &text);
        let raw = assert_raw_run(&ev, &out, &fake, "BmcInd");
        assert!(raw["errors"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("malformed")), "reject corrupt {tag} member: {raw}");
        assert_eq!(raw["properties"][0]["status"], "valid", "retain parsed partial rows beside the corrupt member");
        let _ = std::fs::remove_dir_all(out);
    }
}

#[cfg(unix)]
#[test]
fn a_later_realizable_result_with_conflicting_clauses_invalidates_realizability() {
    let out = tempdir("realizability_conflicting_set");
    let mut realizability = realizability_records("ReleaseLogic", "realizable");
    let mut later = realizability_records("ReleaseLogic", "realizable");
    later.last_mut().unwrap()["conflictingSet"] = serde_json::json!({
        "nodes": [{ "name": "ReleaseLogic", "elements": [{ "category": "guarantee", "name": "injected_conflicting_clause" }] }],
    });
    realizability.extend(later);
    let fake = fake_kind2(
        &out, "kind2_realizability_conflicting_set", &[proof_property("a", "valid")],
        0, &realizability, 0,
    );
    let (_, text, ev) = proof_with_fake(&out, &fake);
    assert_incomplete_proof(&ev, &text);
    let raw = assert_raw_run(&ev, &out, &fake, "Realizability");
    assert_eq!(raw["realizability"].as_array().unwrap().len(), 2);
    assert_eq!(raw["realizability"][0]["conflicting"], serde_json::json!([]));
    assert_eq!(raw["realizability"][1]["result"], "realizable");
    assert_eq!(raw["realizability"][1]["conflicting"], serde_json::json!(["guarantee injected_conflicting_clause"]));
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn direct_prove_rejects_a_nonzero_exit_after_valid_properties() {
    let out = tempdir("direct_exit_failure");
    let fake = fake_kind2(
        &out, "kind2_direct_exit_42", &[proof_property("a", "valid")],
        42, &realizability_records("ReleaseLogic", "realizable"), 0,
    );
    let (ok, text) = prove_with_fake(&out, &fake, "bmc-ind");
    assert!(!ok, "direct prove must reject raw exit failure: {text}");
    assert!(text.contains("exit code: 42"), "{text}");
    assert!(out.join("kind2_direct_exit_42_proof_input.lus").exists(), "the real prover process must run");
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn direct_prove_rejects_properties_from_an_unrelated_analysis_root() {
    let out = tempdir("direct_wrong_root");
    let fake = fake_kind2(
        &out, "kind2_direct_wrong_root", &[proof_property("a", "valid")],
        0, &realizability_records("ReleaseLogic", "realizable"), 0,
    );
    let stdout = serde_json::to_string(&vec![
        serde_json::json!({ "objectType": "analysisStart", "top": "UnrelatedHelper" }),
        proof_property("a", "valid"),
    ]).unwrap();
    replace_fake_proof_stdout(&fake, &stdout);
    let (ok, text) = prove_with_fake(&out, &fake, "bmc-ind");
    assert!(!ok, "direct prove must bind properties to its requested root: {text}");
    assert!(text.contains("ReleaseLogic"), "identify the requested root: {text}");
    assert!(out.join("kind2_direct_wrong_root_proof_input.lus").exists());
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn direct_prove_realizability_requires_a_root_realizability_result() {
    let out = tempdir("direct_missing_realizability");
    let fake = fake_kind2(
        &out, "kind2_direct_missing_realizability", &[proof_property("a", "valid")], 0,
        &[
            serde_json::json!({ "objectType": "analysisStart", "top": "ReleaseLogic", "context": "contract" }),
            proof_property("a", "valid"),
        ], 0,
    );
    let (ok, text) = prove_with_fake(&out, &fake, "realizability");
    assert!(!ok, "a valid property row cannot replace the requested root realizability result: {text}");
    assert!(text.contains("ReleaseLogic"), "identify the requested root: {text}");
    assert!(out.join("kind2_direct_missing_realizability_contract_input.lus").exists(), "CONTRACTCK must run");
    let _ = std::fs::remove_dir_all(out);
}

#[cfg(unix)]
#[test]
fn direct_prove_realizability_accepts_inconclusive_progress_around_a_realizable_root() {
    let out = tempdir("direct_realizability_progress");
    let mut realizability = realizability_records("ReleaseLogic", "unknown");
    realizability.extend(realizability_records("ReleaseLogic", "realizable"));
    realizability.extend(realizability_records("ReleaseLogic", "unknown"));
    let fake = fake_kind2(
        &out, "kind2_direct_realizability_progress", &[], 0, &realizability, 0,
    );
    let (ok, text) = prove_with_fake(&out, &fake, "realizability");
    assert!(ok, "ordinary root realizability progress must remain accepted: {text}");
    assert!(text.contains("realizability of ReleaseLogic (contract): realizable"), "{text}");
    assert!(out.join("kind2_direct_realizability_progress_contract_input.lus").exists());
    let _ = std::fs::remove_dir_all(out);
}
