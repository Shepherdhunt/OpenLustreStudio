//! The Studio's quality-of-life surface, end to end through the HTTP API:
//! FUZZ simulation modes (full-range generation and bad-data injection),
//! TestScenarios saved from a session, and persisted wire shapes with
//! per-wire data types for the View ▸ link data types labels.
//!
//! The server is spawned against a COPY of the example model so edits never
//! touch the repository file.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

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

fn start_server_on_copy() -> ServerGuard {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_qol_{stamp}"));
    std::fs::create_dir_all(&tmp).unwrap();
    let model = tmp.join("model.json");
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/release_logic/model/release_logic.json");
    std::fs::copy(&src, &model).unwrap();
    let libraries = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries");

    let mut child = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "studio", "serve"])
        .arg(&model)
        .arg("--with-stdlib")
        .arg(&libraries)
        .arg("--port")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cargo run studio serve");

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
    stream.shutdown(Shutdown::Write).ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let (head, payload) = raw.split_once("\r\n\r\n")?;
    let status: u16 = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, payload.to_string()))
}

fn json(port: u16, method: &str, path: &str, body: &str) -> (u16, serde_json::Value) {
    let (s, b) = request(port, method, path, body).unwrap_or_else(|| panic!("{method} {path}"));
    let v = serde_json::from_str(&b).unwrap_or(serde_json::Value::Null);
    (s, v)
}

/// A clean operator to fuzz: `ok = RangeCheck(a, 0, 255)` — a directly
/// called stdlib block, so its one pin names the type the call expects.
fn build_fuzz_target(port: u16) {
    let (_, _) = json(port, "POST", "/api/edit/add_node", r#"{"name":"FuzzMe","kind":"operator"}"#);
    for body in [
        r#"{"node":"FuzzMe","side":"input","name":"a","type":"int32"}"#,
        r#"{"node":"FuzzMe","side":"output","name":"ok","type":"bool"}"#,
    ] {
        let (s, body) = request(port, "POST", "/api/edit/add_port", body).expect("add_port");
        assert_eq!(s, 200, "add_port failed: {body}");
    }
    let (s, body) = request(
        port,
        "POST",
        "/api/edit/add_equation",
        r#"{"node":"FuzzMe","lhs":"ok","body":"RangeCheck(a, 0, 255)"}"#,
    )
    .expect("add_equation");
    assert_eq!(s, 200, "add_equation failed: {body}");
    let (s, body) = request(port, "POST", "/api/edit/set_main", r#"{"main":"FuzzMe"}"#).expect("set_main");
    assert_eq!(s, 200, "set_main failed: {body}");
}

#[test]
fn fuzz_modes_testscenarios_and_wire_shapes() {
    let g = start_server_on_copy();
    let port = g.port;
    build_fuzz_target(port);

    // --- FUZZ range: every input generated across its full type span --------
    let (s, d) = json(port, "POST", "/api/sim/start", r#"{"node":"FuzzMe"}"#);
    assert_eq!(s, 200, "sim/start failed: {d}");
    let (s, d) = json(
        port,
        "POST",
        "/api/sim/step",
        r#"{"fuzz":{"mode":"range","seed":7},"count":8}"#,
    );
    assert_eq!(s, 200, "fuzz range step failed: {d}");
    let rows = d["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 8);
    assert_eq!(d["cycle"], 8, "session advanced one cycle per row");
    assert_eq!(d["fuzz"]["mode"], "range");
    for r in rows {
        let fed = r["fuzz_inputs"].as_object().expect("fuzz_inputs on every row");
        assert!(fed.contains_key("a"));
        // int32 full span: 8 seeded cycles must exercise more than one value.
        let a: i64 = fed["a"].as_str().unwrap().parse().expect("a is an integer");
        assert!((-2147483648..=2147483647).contains(&a), "a within int32");
    }
    let a_values: std::collections::HashSet<&str> = rows
        .iter()
        .map(|r| r["fuzz_inputs"]["a"].as_str().unwrap())
        .collect();
    assert!(a_values.len() > 1, "the generator varies across cycles");

    // Same seed, fresh session: the same values (a saved scenario replays it).
    let (s, _) = json(port, "POST", "/api/sim/start", r#"{"node":"FuzzMe"}"#);
    assert_eq!(s, 200);
    let (s, d2) = json(
        port,
        "POST",
        "/api/sim/step",
        r#"{"fuzz":{"mode":"range","seed":7},"count":8}"#,
    );
    assert_eq!(s, 200);
    assert_eq!(d["rows"], d2["rows"], "a seed replays its run exactly");

    // --- FUZZ bad data: injections refused, model never steps ---------------
    let (s, d) = json(
        port,
        "POST",
        "/api/sim/step",
        r#"{"fuzz":{"mode":"bad","seed":3},"count":6}"#,
    );
    assert_eq!(s, 200, "fuzz bad step failed: {d}");
    assert_eq!(d["fuzz"]["mode"], "bad");
    assert_eq!(d["fuzz"]["refused"], 6, "every malformed injection refused");
    assert_eq!(d["cycle"], 8, "refused attempts do not advance the session");
    for r in d["rows"].as_array().unwrap() {
        assert_eq!(r["fuzz_bad"]["outcome"], "refused");
        assert!(r["fuzz_bad"]["error"].as_str().unwrap().len() > 3, "the refusal names its error");
        assert!(r["values"].is_null(), "no model values on a refused cycle");
    }
    // The same session still steps normally afterwards.
    let (s, d) = json(port, "POST", "/api/sim/step", r#"{"inputs":{"a":"1"},"count":1}"#);
    assert_eq!(s, 200, "manual step after fuzz: {d}");
    assert_eq!(d["cycle"], 9);

    // --- TestScenarios: save from a session, list, read, delete -------------
    let (s, d) = json(
        port,
        "POST",
        "/api/tests/save",
        r#"{"name":"fuzz_cap","csv":"a\n5\n7\n"}"#,
    );
    assert_eq!(s, 200, "tests/save failed: {d}");
    assert_eq!(d["cycles"], 2);
    let (s, d) = json(port, "GET", "/api/tests", "");
    assert_eq!(s, 200);
    assert!(
        d["scenarios"].as_array().unwrap().iter().any(|x| x["name"] == "fuzz_cap"),
        "the saved scenario is listed"
    );
    // No overwrite without asking.
    let (s, d) = json(
        port,
        "POST",
        "/api/tests/save",
        r#"{"name":"fuzz_cap","csv":"a\n9\n"}"#,
    );
    assert_eq!(s, 400, "duplicate refused: {d}");
    let (s, d) = json(
        port,
        "POST",
        "/api/tests/save",
        r#"{"name":"fuzz_cap","csv":"a\n9\n","overwrite":true}"#,
    );
    assert_eq!(s, 200, "overwrite saves: {d}");
    // Path safety.
    let (s, _) = json(port, "POST", "/api/tests/save", r#"{"name":"../escape","csv":"a\n1\n"}"#);
    assert_eq!(s, 400, "path-unsafe names refused");
    // One scenario's vector, as the Contracts ▸ TestScenarios replay reads it.
    let (s, d) = json(port, "GET", "/api/tests/one?name=fuzz_cap", "");
    assert_eq!(s, 200, "tests/one failed: {d}");
    assert_eq!(d["header"], serde_json::json!(["a"]));
    assert_eq!(d["rows"], serde_json::json!([["9"]]));
    // Delete removes it.
    let (s, _) = json(port, "POST", "/api/tests/delete", r#"{"name":"fuzz_cap"}"#);
    assert_eq!(s, 200);
    let (_, d) = json(port, "GET", "/api/tests", "");
    assert!(
        !d["scenarios"].as_array().unwrap().iter().any(|x| x["name"] == "fuzz_cap"),
        "deleted"
    );

    // --- Wire shapes: waypoints persist; call pins carry expected types ------
    let (s, body) = request(
        port,
        "POST",
        "/api/edit/set_layout",
        r#"{"node":"FuzzMe","positions":{},"grid":8,"wires":{"a|eq0|0":{"pts":[[300,60],[300,120]]}}}"#,
    )
    .expect("set_layout");
    assert_eq!(s, 200, "set_layout wires failed: {body}");
    let (s, d) = json(port, "GET", "/api/diagram?node=FuzzMe", "");
    assert_eq!(s, 200);
    let routes = d["wire_routes"].as_object().expect("wire_routes");
    assert_eq!(
        serde_json::json!(routes["a|eq0|0"]["pts"]),
        serde_json::json!([[300.0, 60.0], [300.0, 120.0]]),
        "the shaped route round-trips through the model file"
    );
    // The pin fed by `a` (int32) names the type RangeCheck expects there.
    let wire = d["wires"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["from"] == "a" && w["to"] == "eq0")
        .expect("a→eq0 wire");
    assert_eq!(wire["dst_type"], "int32", "call pins carry the expected type");

    // A uint8 feeding the same int32 pin: `uint8 | int32` — the pair the
    // View ▸ link data types label shows in red.
    let _ = json(port, "POST", "/api/edit/add_node", r#"{"name":"Guard","kind":"operator"}"#);
    for body in [
        r#"{"node":"Guard","side":"input","name":"x","type":"uint8"}"#,
        r#"{"node":"Guard","side":"output","name":"ok","type":"bool"}"#,
    ] {
        let (s, b) = request(port, "POST", "/api/edit/add_port", body).expect("add_port Guard");
        assert_eq!(s, 200, "Guard add_port failed: {b}");
    }
    let (s, b) = request(
        port,
        "POST",
        "/api/edit/add_equation",
        r#"{"node":"Guard","lhs":"ok","body":"RangeCheck(x, 0, 255)"}"#,
    )
    .expect("Guard add_equation");
    assert_eq!(s, 200, "Guard add_equation failed: {b}");
    let (s, d) = json(port, "GET", "/api/diagram?node=Guard", "");
    assert_eq!(s, 200);
    let gwire = d["wires"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["from"] == "x" && w["to"] == "eq0")
        .expect("x→eq0 wire");
    assert_eq!(gwire["dst_type"], "int32", "the callee's expected type at the pin");
    assert_eq!(d["inputs"].as_array().unwrap().iter().find(|p| p["name"] == "x").unwrap()["type"]["kind"], "Uint8");

    // An empty pts clears the shape.
    let (s, _) = request(
        port,
        "POST",
        "/api/edit/set_layout",
        r#"{"node":"FuzzMe","positions":{},"wires":{"a|eq0|0":{"pts":[]}}}"#,
    )
    .expect("clear layout");
    assert_eq!(s, 200);
    let (_, d) = json(port, "GET", "/api/diagram?node=FuzzMe", "");
    assert!(d["wire_routes"].as_object().unwrap().is_empty(), "cleared");
}

#[test]
fn embedded_ui_ships_the_new_controls() {
    let ui = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/ol_cli/src/studio_ui.html"),
    )
    .expect("studio_ui.html");
    // View ▸ toggles (overlap guard on by default, link types off).
    assert!(ui.contains("id=\"block-overlaps\" checked"));
    assert!(ui.contains("id=\"wire-types\""));
    assert!(ui.contains("prevent overlapping boxes"));
    // FUZZ radios + the scenario saver in the Simulation dock.
    assert!(ui.contains("name=\"sim-fuzz\""));
    assert!(ui.contains("FUZZ range"));
    assert!(ui.contains("FUZZ bad data"));
    assert!(ui.contains("simSaveScenario"));
    // Contracts ▸ TestScenarios tab.
    assert!(ui.contains("data-ct=\"scenarios\""));
    // View options persist across opens.
    assert!(ui.contains("ols.viewOpts"));
}
