//! A proof that runs out of time reads as *unknown*, never as fewer checks
//! or a broken run. Kind 2 reports only the properties it reached before its
//! wall-clock timeout — on a large model none at all — so the tools put back
//! every runtime-error check it did not report and name the contracts it
//! never reached (`crates/ol_cli/src/proof.rs`). A stand-in `kind2` replays
//! what Kind 2 printed when it ran out of time on a 200-operator model, so
//! the case is exercised without waiting for it. (The stand-in is a shell
//! script, so these run on Linux and macOS.)
#![cfg(unix)]

use std::io::{BufRead, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

/// The Studio's access token for these tests (`OPENLUSTRE_STUDIO_TOKEN`).
const TEST_TOKEN: &str = "openlustre-test-token";

/// What Kind 2 v2.2.0 printed when the wall-clock timeout struck before it
/// had settled, or even listed, any property.
const TIMED_OUT_BEFORE_ANYTHING: &str = r#"[
{"objectType" : "log", "level" : "info", "source" : "parse", "value" : "kind2 v2.2.0"}
,
{"objectType" : "log", "level" : "note", "source" : "parse", "value" : "Incomplete analysis result: Not all properties could be proven invariant"}
,
{"objectType" : "log", "level" : "fatal", "source" : "parse", "value" : "Wallclock timeout."}
]"#;

fn pms() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/pms/pms.wksc")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ol_prove_timeout_{tag}_{}", openlustre_integration_tests::unique_stamp()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A `kind2` that answers `--version` and otherwise runs out of time.
fn kind2_that_times_out(dir: &Path) -> PathBuf {
    let path = dir.join("kind2");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in --version) echo 'kind2 v2.2.0'; exit 0;; esac\ncat <<'EOF'\n{TIMED_OUT_BEFORE_ANYTHING}\nEOF\nexit 30\n"
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn prove(kind2: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "prove"])
        .arg(pms())
        .arg("--kind2")
        .arg(kind2)
        .args(["--timeout", "5"])
        .args(extra)
        .output()
        .expect("cargo run openlustre prove")
}

#[test]
fn a_proof_stopped_before_any_result_lists_every_check_as_unknown() {
    let dir = scratch("cli");
    let o = prove(&kind2_that_times_out(&dir), &[]);
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "a proof that settled nothing must not pass");
    assert!(!out.contains("reported no properties") && !err.contains("reported no properties"), "{out}\n{err}");
    // All 60 of the PMS's runtime-error checks, unknown — not "0 of 0".
    assert!(out.contains("prove: 0 of 60 hold, 0 failed, 60 unknown"), "{out}");
    assert!(out.contains("runtime errors — 0 of 60 checks hold"), "{out}");
    assert_eq!(out.lines().filter(|l| l.starts_with("  rte") && l.contains(": unknown")).count(), 60, "{out}");
    assert!(out.contains("Kind 2 stopped at the 5s timeout") && out.contains("(60 runtime-error checks never reached)"), "{out}");
    // The contracts it never reached are named, since their properties cannot be listed.
    assert!(out.contains("not listed: the properties of") && out.contains("PMS_contract") && out.contains("Balance_contract"), "{out}");
    assert!(out.contains("raise the timeout"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_contract_proof_stopped_before_any_result_says_so() {
    let dir = scratch("contracts");
    let o = prove(&kind2_that_times_out(&dir), &["--no-runtime-errors"]);
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success());
    assert!(out.contains("Kind 2 stopped at the 5s timeout before settling any property — nothing was proved or refuted"), "{out}");
    assert!(out.contains("not reached: the contracts of") && out.contains("PMS_contract"), "{out}");
    assert!(err.contains("stopped at the 5s timeout before settling any property"), "{err}");
    assert!(!out.contains("raw stdout follows"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn post(port: u16, path: &str) -> serde_json::Value {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nX-OpenLustre-Token: {TEST_TOKEN}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).ok();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (_, body) = raw.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

/// The Verify dock gets the same: every check unknown, the unreached
/// contracts named, and the timeout said in so many words.
#[test]
fn the_verify_dock_reports_a_timeout_before_any_result_as_unknown() {
    let dir = scratch("studio");
    let kind2 = kind2_that_times_out(&dir);
    for f in ["pms.wksc", "types.json"] {
        std::fs::copy(pms().with_file_name(f), dir.join(f)).unwrap();
    }
    let mut child = Command::new(env!("CARGO"))
        .env("OPENLUSTRE_STUDIO_TOKEN", TEST_TOKEN)
        .env("OPENLUSTRE_KIND2", &kind2)
        .args(["run", "-q", "-p", "ol_cli", "--", "studio", "serve"])
        .arg(dir.join("pms.wksc"))
        .args(["--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    let _guard = ServerGuard(child);
    let mut line = String::new();
    let mut port = None;
    while port.is_none() && reader.read_line(&mut line).unwrap_or(0) > 0 {
        port = line.split_once("http://127.0.0.1:").and_then(|(_, r)| r.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u16>().ok());
        line.clear();
    }
    let port = port.expect("the Studio prints its port");

    let v = post(port, "/api/prove?timeout=5&mode=prove");
    assert_eq!(v["timed_out"], true, "{v}");
    assert_eq!(v["not_reached"], 60, "{v}");
    let props = v["properties"].as_array().unwrap();
    assert_eq!(props.len(), 60);
    assert!(props.iter().all(|p| p["outcome"] == "Unknown" && p["rte"].is_object()), "{v}");
    let contracts: Vec<&str> = v["unreached_contracts"].as_array().unwrap().iter().filter_map(|c| c.as_str()).collect();
    assert!(contracts.contains(&"PMS_contract"), "{contracts:?}");

    // Contracts only: nothing to list, but the timeout and the contracts are said.
    let v = post(port, "/api/prove?timeout=5&mode=prove&rte=0");
    assert_eq!(v["timed_out"], true, "{v}");
    assert_eq!(v["properties"].as_array().unwrap().len(), 0);
    assert!(v["unreached_contracts"].as_array().unwrap().len() >= 8, "{v}");
    let _ = std::fs::remove_dir_all(&dir);
}
