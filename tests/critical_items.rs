//! The five SCADE-parity items, end to end: free-form canvas layout
//! persistence, hierarchical diagram dive data, the Kind 2 Verify endpoint,
//! scenario decision coverage, and the FSM editor endpoints.

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

fn start_server_on_copy(tag: &str) -> ServerGuard {
    let tmp = make_tempdir(tag);
    let model = tmp.join("model.json");
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/release_logic/model/release_logic.json");
    std::fs::copy(&src, &model).unwrap();

    let mut child = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "studio", "serve"])
        .arg(&model)
        .args(["--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("studio serve");

    use std::io::BufRead;
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut port = None;
    for _ in 0..400 {
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
        if request(port, "GET", "/api/health", "").is_some() {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    ServerGuard { child, port, tmp }
}

fn request(port: u16, method: &str, path: &str, body: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    stream.write_all(req.as_bytes()).ok()?;
    stream.shutdown(Shutdown::Write).ok();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let (head, payload) = raw.split_once("\r\n\r\n")?;
    let status: u16 = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, payload.to_string()))
}

// --- 1. Free-form canvas: layout persistence -------------------------------

#[test]
fn layout_positions_persist_to_the_model_file_and_round_trip() {
    let g = start_server_on_copy("layout");
    let port = g.port;

    let payload = r#"{"node":"ReleaseLogic","positions":{
        "master_arm":{"x":12.0,"y":34.0},
        "eq0":{"x":300.0,"y":80.0},
        "release_cmd":{"x":640.0,"y":40.0}
    }}"#;
    let (s, body) = request(port, "POST", "/api/edit/set_layout", payload).expect("set_layout");
    assert_eq!(s, 200, "set_layout failed: {body}");

    // Round-trips through the diagram endpoint...
    let (s, body) = request(port, "GET", "/api/diagram?node=ReleaseLogic", "").expect("diagram");
    assert_eq!(s, 200);
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["positions"]["eq0"]["x"], 300.0);
    assert_eq!(d["positions"]["master_arm"]["y"], 34.0);

    // ...and is durable in the model file on disk.
    let on_disk = std::fs::read_to_string(g.tmp.join("model.json")).unwrap();
    assert!(on_disk.contains("positions"), "layout not saved to disk");
    assert!(on_disk.contains("eq0"));
}

// --- 2. Hierarchical navigation: dive data in the diagram ------------------

#[test]
fn diagram_exposes_callee_names_for_dive_navigation() {
    let g = start_server_on_copy("dive");
    let port = g.port;

    // Author an operator that calls a stdlib block, then inspect its diagram:
    // the equation must advertise its callee so the UI can dive into it, and
    // the callee's own diagram must be servable (the dive target).
    for (path, body) in [
        ("/api/edit/add_node", r#"{"name":"Outer","kind":"operator"}"#),
        ("/api/edit/add_port", r#"{"node":"Outer","side":"input","name":"x","type":"bool"}"#),
        ("/api/edit/add_port", r#"{"node":"Outer","side":"output","name":"y","type":"bool"}"#),
        ("/api/edit/add_equation", r#"{"node":"Outer","lhs":"y","body":"RisingEdge(x)"}"#),
    ] {
        let (s, b) = request(port, "POST", path, body).expect(path);
        assert_eq!(s, 200, "{path} failed: {b}");
    }

    let (s, body) = request(port, "GET", "/api/diagram?node=Outer", "").expect("diagram");
    assert_eq!(s, 200);
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["equations"][0]["calls"][0], "RisingEdge");

    let (s, body) = request(port, "GET", "/api/diagram?node=RisingEdge", "").expect("dive target");
    assert_eq!(s, 200, "{body}");
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["node"], "RisingEdge");

    // The SPA carries the new tabs and the dive/breadcrumb machinery.
    let (_, html) = request(port, "GET", "/", "").expect("spa");
    assert!(html.contains("data-tab=\"verify\""));
    assert!(html.contains("data-tab=\"fsm\""));
    assert!(html.contains("diagram-crumbs"));
}

// --- 3. Verify (Kind 2) endpoint -------------------------------------------

#[test]
fn prove_endpoint_degrades_gracefully_without_kind2() {
    let g = start_server_on_copy("prove");
    let (s, body) = request(g.port, "POST", "/api/prove?timeout=5", "").expect("prove");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    // This environment has no kind2 binary: the endpoint must say so with a
    // hint rather than erroring, and still record the attempted invocation.
    assert_eq!(v["kind2_found"], false, "{body}");
    assert!(v["hint"].as_str().unwrap_or("").contains("kind2"));
    let invocation = v["invocation"].as_array().unwrap();
    assert!(invocation.iter().any(|a| a.as_str() == Some("--timeout_wall")));
    assert!(invocation.iter().any(|a| a.as_str() == Some("5")));
}

// --- 4. Decision coverage over the scenario suite --------------------------

#[test]
fn scenario_suite_reports_decision_coverage_and_uncovered_conditions() {
    let tmp = make_tempdir("cov");
    let model = tmp.join("model.json");
    // Gate(x) -> y: one decision, `x > 10`.
    let project = serde_json::json!({
        "name": "gate",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Gate",
                "kind": "Function",
                "inputs": [{"name": "x", "ty": {"kind": "Int32"}}],
                "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
                "equations": [{
                    "lhs": ["y"],
                    "rhs": {"expr": "IfThenElse",
                        "cond": {"expr": "Binary", "op": "Gt",
                            "lhs": {"expr": "Var", "name": "x"},
                            "rhs": {"expr": "Const", "lit": {"lit": "Int", "value": 10}}},
                        "then_branch": {"expr": "Var", "name": "x"},
                        "else_branch": {"expr": "Const", "lit": {"lit": "Int", "value": 0}}}
                }]
            }]
        }],
        "main": "Gate"
    });
    std::fs::write(&model, serde_json::to_string_pretty(&project).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    // Only above-threshold inputs: the decision never goes false.
    std::fs::write(scen.join("high.csv"), "x\n11\n42\n").unwrap();

    let run = |args: &[&str]| -> (bool, String) {
        let out = Command::new(env!("CARGO"))
            .args(["run", "-q", "-p", "ol_cli", "--"])
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    let (ok, out) = run(&[
        "test", "record", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(),
    ]);
    assert!(ok, "record: {out}");

    let (ok, out) = run(&[
        "test", "run", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "ir",
    ]);
    assert!(ok, "run: {out}");
    assert!(out.contains("decision coverage: 0/1"), "got: {out}");
    assert!(out.contains("missing false"), "got: {out}");

    // Add a below-threshold scenario: the decision is now driven both ways.
    std::fs::write(scen.join("low.csv"), "x\n3\n").unwrap();
    let (ok, out) = run(&[
        "test", "record", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(),
    ]);
    assert!(ok, "re-record: {out}");
    let (ok, out) = run(&[
        "test", "run", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "ir",
    ]);
    assert!(ok, "run2: {out}");
    assert!(out.contains("decision coverage: 1/1"), "got: {out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

// --- 5. FSM editor endpoints ------------------------------------------------

#[test]
fn fsm_create_view_and_simulate_through_the_studio() {
    let g = start_server_on_copy("fsm");
    let port = g.port;

    // Create a two-state toggle machine through the editor payload.
    let payload = serde_json::json!({
        "name": "Toggle",
        "initial_state": "OFF",
        "inputs": [{"name": "pulse", "type": "bool"}],
        "outputs": [{"name": "light", "type": "bool"}],
        "states": [
            {"name": "OFF", "equations": [{"lhs": "light", "body": "false"}],
             "transitions": [{"guard": "pulse", "target": "ON"}]},
            {"name": "ON", "equations": [{"lhs": "light", "body": "true"}],
             "transitions": [{"guard": "pulse", "target": "OFF"}]}
        ]
    });
    let (s, body) = request(
        port, "POST", "/api/edit/add_state_machine", &payload.to_string(),
    ).expect("add fsm");
    assert_eq!(s, 200, "add_state_machine failed: {body}");

    // Listed and viewable.
    let (s, body) = request(port, "GET", "/api/fsm", "").expect("list");
    assert_eq!(s, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["machines"].as_array().unwrap().iter().any(|m| m == "Toggle"));

    let (s, body) = request(port, "GET", "/api/fsm?name=Toggle", "").expect("detail");
    assert_eq!(s, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["initial_state"], "OFF");
    assert_eq!(v["states"][0]["transitions"][0]["guard"], "pulse");

    // It lowers through the serve pipeline: set as main and step it.
    let (s, b) = request(port, "POST", "/api/edit/set_main", r#"{"main":"Toggle"}"#)
        .expect("set_main");
    assert_eq!(s, 200, "{b}");
    let csv = "pulse\nfalse\ntrue\ntrue\nfalse\n";
    let (s, trace) = request(port, "POST", "/api/simulate?full=1", csv).expect("simulate");
    assert_eq!(s, 200, "{trace}");
    let lines: Vec<&str> = trace.trim().lines().collect();
    // The full trace exposes the lowered machine's state locals too — the
    // SCADE watch view shows the FSM's current state every step.
    assert_eq!(lines[0], "cycle,pulse,__sm_state,__sm_next_state,light");
    // OFF (f) -> pulse arms ON for next cycle -> ON (t) -> pulse back to OFF.
    assert_eq!(lines[1], "0,false,OFF,OFF,false");
    assert_eq!(lines[2], "1,true,OFF,ON,false");
    assert_eq!(lines[3], "2,true,ON,OFF,true");
    assert_eq!(lines[4], "3,false,OFF,OFF,false");

    // A malformed machine (unknown initial state) is rejected and the file
    // stays untouched.
    let bad = serde_json::json!({
        "name": "Broken", "initial_state": "Nowhere",
        "inputs": [], "outputs": [{"name": "z", "type": "bool"}],
        "states": [{"name": "A", "equations": [{"lhs": "z", "body": "true"}],
                    "transitions": []}]
    });
    let (s, body) = request(
        port, "POST", "/api/edit/add_state_machine", &bad.to_string(),
    ).expect("bad fsm");
    assert_eq!(s, 400, "expected rejection, got {s}: {body}");
    let on_disk = std::fs::read_to_string(g.tmp.join("model.json")).unwrap();
    assert!(!on_disk.contains("Broken"));
}
