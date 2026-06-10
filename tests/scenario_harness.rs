//! The SCADE Test analog, end to end: record golden traces, verify both the
//! IR simulator and the compiled generated C against them, and prove that a
//! behavioral model change is caught with cycle-level, signal-level
//! diagnostics — via the CLI and via the Studio HTTP API.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Copy the ReleaseLogic example model + a scenario into a temp dir so tests
/// never mutate repository files.
fn setup_project(tmp: &PathBuf) -> PathBuf {
    let model = tmp.join("model.json");
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/release_logic/model/release_logic.json");
    std::fs::copy(&src, &model).unwrap();
    let scen_dir = tmp.join("scenarios");
    std::fs::create_dir_all(&scen_dir).unwrap();
    std::fs::write(
        scen_dir.join("nominal.csv"),
        "master_arm,station_selected,consent,fault_present,release_request\n\
         false,false,false,false,false\n\
         true,true,true,false,true\n\
         true,true,true,true,true\n",
    )
    .unwrap();
    model
}

fn openlustre(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--"])
        .args(args)
        .output()
        .expect("cargo run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn record_then_run_passes_on_both_backends() {
    let tmp = make_tempdir("scen");
    let model = setup_project(&tmp);
    let scen = tmp.join("scenarios");

    let (ok, out) = openlustre(&[
        "test",
        "record",
        model.to_str().unwrap(),
        "--scenarios",
        scen.to_str().unwrap(),
    ]);
    assert!(ok, "record failed: {out}");
    assert!(scen.join("nominal.golden.csv").exists(), "golden written");

    // The golden is the FULL trace: every input, output, and contract column.
    let golden = std::fs::read_to_string(scen.join("nominal.golden.csv")).unwrap();
    let header = golden.lines().next().unwrap();
    assert_eq!(
        header,
        "cycle,master_arm,station_selected,consent,fault_present,release_request,release_cmd,inhibit,active_mode,violations"
    );

    let (ok, out) = openlustre(&[
        "test",
        "run",
        model.to_str().unwrap(),
        "--scenarios",
        scen.to_str().unwrap(),
        "--backend",
        "both",
    ]);
    assert!(ok, "run failed: {out}");
    assert!(out.contains("[PASS] nominal (ir)"), "ir pass missing: {out}");
    assert!(out.contains("[PASS] nominal (c )"), "c pass missing: {out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn behavioral_model_change_is_caught_with_cycle_level_diffs() {
    let tmp = make_tempdir("scen_regress");
    let model = setup_project(&tmp);
    let scen = tmp.join("scenarios");

    let (ok, out) = openlustre(&[
        "test",
        "record",
        model.to_str().unwrap(),
        "--scenarios",
        scen.to_str().unwrap(),
    ]);
    assert!(ok, "record failed: {out}");

    // Break the model's behavior structurally (format-proof): rewrite the
    // release_cmd equation to `release_cmd = release_request`, dropping the
    // fault/arm/consent interlocks. The recorded golden has release_cmd =
    // false on the fault cycle; the mutated model produces true there.
    let text = std::fs::read_to_string(&model).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let eq0 = &mut v["packages"][0]["nodes"][0]["equations"][0];
    assert_eq!(eq0["lhs"][0], "release_cmd", "expected release_cmd equation");
    eq0["rhs"] = serde_json::json!({ "expr": "Var", "name": "release_request" });
    std::fs::write(&model, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    let (ok, out) = openlustre(&[
        "test",
        "run",
        model.to_str().unwrap(),
        "--scenarios",
        scen.to_str().unwrap(),
        "--backend",
        "both",
    ]);
    assert!(!ok, "run should fail after the behavioral change:\n{out}");
    assert!(out.contains("[FAIL] nominal (ir)"), "ir fail missing: {out}");
    assert!(out.contains("[FAIL] nominal (c )"), "c fail missing: {out}");
    // Cycle-level, signal-level diagnostic.
    assert!(
        out.contains("`release_cmd` expected") || out.contains("`inhibit` expected"),
        "expected a named-signal cell diff, got:\n{out}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn missing_golden_is_reported_not_failed() {
    let tmp = make_tempdir("scen_nogolden");
    let model = setup_project(&tmp);
    let scen = tmp.join("scenarios");

    // No record step: run must report NO-GOLDEN and exit successfully
    // (nothing failed; the suite just isn't recorded yet).
    let (ok, out) = openlustre(&[
        "test",
        "run",
        model.to_str().unwrap(),
        "--scenarios",
        scen.to_str().unwrap(),
        "--backend",
        "ir",
    ]);
    assert!(ok, "run with missing goldens should not fail: {out}");
    assert!(out.contains("[NO-GOLDEN] nominal"), "got: {out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

// --- Studio HTTP API surface ---

struct ServerGuard {
    child: Child,
    port: u16,
    tmp: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn start_server() -> ServerGuard {
    let tmp = make_tempdir("scen_http");
    let model = setup_project(&tmp);

    let mut child = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "studio", "serve"])
        .arg(&model)
        .arg("--port")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("studio serve");

    use std::io::BufRead;
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut port = None;
    for _ in 0..200 {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if let Some(rest) = line.split_once("http://127.0.0.1:") {
                    let p: String = rest.1.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(n) = p.parse::<u16>() {
                        port = Some(n);
                        break;
                    }
                }
            }
            Err(_) => sleep(Duration::from_millis(20)),
        }
    }
    let port = port.expect("server should print bound port");
    for _ in 0..50 {
        if request(port, "GET", "/api/health").is_some() {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    ServerGuard { child, port, tmp }
}

fn request(port: u16, method: &str, path: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).ok()?;
    stream.shutdown(Shutdown::Write).ok();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let (head, payload) = raw.split_once("\r\n\r\n")?;
    let status: u16 = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, payload.to_string()))
}

#[test]
fn studio_tests_endpoints_list_record_and_run() {
    let g = start_server();
    let port = g.port;

    // 1. List: one scenario, no golden yet, cc availability flagged.
    let (s, body) = request(port, "GET", "/api/tests").expect("list");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["scenarios"][0]["name"], "nominal");
    assert_eq!(v["scenarios"][0]["has_golden"], false);

    // 2. Record goldens through the API.
    let (s, body) = request(port, "POST", "/api/tests/record").expect("record");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["recorded"][0]["name"], "nominal");
    assert!(g.tmp.join("scenarios/nominal.golden.csv").exists());

    // 3. Run: everything green on both backends.
    let (s, body) = request(port, "POST", "/api/tests/run").expect("run");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["all_green"], true, "{body}");
    let results = v["results"].as_array().unwrap();
    assert!(results
        .iter()
        .any(|r| r["backend"] == "ir" && r["status"] == "pass"));
    // The C backend either passes (cc present) or is explicitly skipped.
    assert!(results
        .iter()
        .any(|r| r["backend"] == "c"
            && (r["status"] == "pass" || r["status"] == "skipped")));
}
