//! The SCADE-style authoring loop, end to end through the Studio HTTP API:
//! create an operator, add ports, write an equation that calls a library
//! block, set it as the project's main, view its dataflow diagram, step it
//! with deterministic values for every item, and generate its C-Lite.
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
    let tmp = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_edit_{stamp}"));
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
    stream.shutdown(Shutdown::Write).ok();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let (head, payload) = raw.split_once("\r\n\r\n")?;
    let status: u16 = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, payload.to_string()))
}

#[test]
fn draw_an_operator_step_it_and_generate_its_c() {
    let g = start_server_on_copy();
    let port = g.port;

    // 1. Create a new operator in the project — the "drawing" act.
    let (s, body) = request(
        port,
        "POST",
        "/api/edit/add_node",
        r#"{"name":"EdgeDemo","kind":"operator"}"#,
    )
    .expect("add_node");
    assert_eq!(s, 200, "add_node failed: {body}");

    // 2. Give it a boolean input and a boolean output.
    let (s, body) = request(
        port,
        "POST",
        "/api/edit/add_port",
        r#"{"node":"EdgeDemo","side":"input","name":"button","type":"bool"}"#,
    )
    .expect("add input");
    assert_eq!(s, 200, "add input failed: {body}");
    let (s, body) = request(
        port,
        "POST",
        "/api/edit/add_port",
        r#"{"node":"EdgeDemo","side":"output","name":"edge","type":"bool"}"#,
    )
    .expect("add output");
    assert_eq!(s, 200, "add output failed: {body}");

    // 3. Wire the behavior: one equation calling the RisingEdge library
    //    block. This is the math/if/temporal surface — the textual body goes
    //    through the same parser the library uses.
    let (s, body) = request(
        port,
        "POST",
        "/api/edit/add_equation",
        r#"{"node":"EdgeDemo","lhs":"edge","body":"RisingEdge(button)"}"#,
    )
    .expect("add equation");
    assert_eq!(s, 200, "add equation failed: {body}");

    // 4. Designate it as the project's main — the entry point of the
    //    eventual standalone executable.
    let (s, body) = request(
        port,
        "POST",
        "/api/edit/set_main",
        r#"{"main":"EdgeDemo"}"#,
    )
    .expect("set main");
    assert_eq!(s, 200, "set_main failed: {body}");

    // The edit persisted to the model FILE, not just memory.
    let on_disk = std::fs::read_to_string(g.tmp.join("model.json")).unwrap();
    assert!(on_disk.contains("EdgeDemo"), "edit not saved to disk");
    assert!(on_disk.contains("\"main\": \"EdgeDemo\""), "main not saved");

    // 5. The diagram endpoint renders its dataflow: button -> eq0 -> edge.
    let (s, body) = request(port, "GET", "/api/diagram?node=EdgeDemo", "").expect("diagram");
    assert_eq!(s, 200, "diagram failed: {body}");
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["node"], "EdgeDemo");
    let wires = d["wires"].as_array().unwrap();
    assert!(wires.iter().any(|w| w["from"] == "button" && w["to"] == "eq0"));
    assert!(wires.iter().any(|w| w["from"] == "eq0" && w["to"] == "edge"));
    assert_eq!(d["equations"][0]["calls"][0], "RisingEdge");

    // 6. Step it deterministically — full trace gives EVERY item a value
    //    each cycle (cycle, button, edge).
    let csv = "button\nfalse\ntrue\ntrue\nfalse\ntrue\n";
    let (s, trace) = request(port, "POST", "/api/simulate?full=1", csv).expect("simulate");
    assert_eq!(s, 200, "simulate failed: {trace}");
    let lines: Vec<&str> = trace.trim().lines().collect();
    assert_eq!(lines[0], "cycle,button,edge");
    // RisingEdge fires exactly on false->true transitions: cycles 1 and 4.
    assert_eq!(lines[1], "0,false,false");
    assert_eq!(lines[2], "1,true,true");
    assert_eq!(lines[3], "2,true,false");
    assert_eq!(lines[4], "3,false,false");
    assert_eq!(lines[5], "4,true,true");

    // 7. The auto-code generator produces its C — model behavior to C-Lite.
    let (s, src) = request(port, "GET", "/api/clite/source", "").expect("clite");
    assert_eq!(s, 200);
    assert!(src.contains("void EdgeDemo_step"), "generated C missing EdgeDemo_step");
    let (s, mk) = request(port, "GET", "/api/clite/makefile", "").expect("makefile");
    assert_eq!(s, 200);
    assert!(mk.contains("TARGET ?= EdgeDemo"), "makefile should target the new main");
}

/// The SCADE sensor-validation pattern, authored entirely through the API:
/// two comparison gates feed an AND gate that drives a boolean output. This
/// exercises the whole loop the user asked for — create an operator, drop
/// operations, wire their operands, and turn it into Lustre and C-Lite — and
/// asserts the SCADE-style input-pin structure the diagram now exposes.
#[test]
fn author_comparison_and_gate_logic_then_generate_code() {
    let g = start_server_on_copy();
    let port = g.port;
    let json = |p: u16, path: &str| -> serde_json::Value {
        let (s, b) = request(p, "GET", path, "").expect(path);
        assert_eq!(s, 200, "{path}: {b}");
        serde_json::from_str(&b).unwrap()
    };
    let post = |path: &str, body: &str| {
        let (s, b) = request(port, "POST", path, body).expect(path);
        assert_eq!(s, 200, "{path} failed: {b}");
    };

    // Operator with one float input and one bool "valid" output.
    post("/api/edit/add_node", r#"{"name":"RangeCheck","kind":"operator"}"#);
    post("/api/edit/add_port", r#"{"node":"RangeCheck","side":"input","name":"roll","type":"float64"}"#);
    post("/api/edit/add_port", r#"{"node":"RangeCheck","side":"output","name":"roll_ok","type":"bool"}"#);

    // Drop a less-than gate: it lands with two RED input pins on its left edge
    // (the SCADE default) — exactly the contract the user described.
    post("/api/edit/add_operation", r#"{"node":"RangeCheck","op":"less_than","x":240.0,"y":40.0}"#);
    let d = json(port, "/api/diagram?node=RangeCheck");
    let pins = d["equations"][0]["inputs"].as_array().unwrap();
    assert_eq!(pins.len(), 2, "less-than has two input pins");
    assert!(pins.iter().all(|p| p["bound"] == false), "both unbound on drop: {pins:?}");

    // Bind it to `roll < 3.141` (a constant is inlined, not a pin).
    post("/api/edit/update_equation",
        r#"{"node":"RangeCheck","index":0,"lhs":"less_than0","body":"roll < 3.141"}"#);
    let d = json(port, "/api/diagram?node=RangeCheck");
    let pins = d["equations"][0]["inputs"].as_array().unwrap();
    assert_eq!(pins.len(), 1, "only `roll` is a pin now: {pins:?}");
    assert_eq!(pins[0]["name"], "roll");
    assert_eq!(pins[0]["bound"], true);
    // …and that bound pin's wire is tagged with its port index.
    let wires = d["wires"].as_array().unwrap();
    assert!(wires.iter().any(|w| w["from"] == "roll" && w["to"] == "eq0" && w["to_port"] == 0));

    // A greater-than gate for the lower bound.
    post("/api/edit/add_operation", r#"{"node":"RangeCheck","op":"greater_than","x":240.0,"y":120.0}"#);
    post("/api/edit/update_equation",
        r#"{"node":"RangeCheck","index":1,"lhs":"greater_than1","body":"roll > -3.141"}"#);

    // An AND gate: minimum two inputs on the left, output on the right.
    post("/api/edit/add_operation", r#"{"node":"RangeCheck","op":"and","x":420.0,"y":80.0}"#);
    let d = json(port, "/api/diagram?node=RangeCheck");
    let and_pins = d["equations"][2]["inputs"].as_array().unwrap();
    assert_eq!(and_pins.len(), 2, "AND drops with two input pins");
    assert_eq!(d["equations"][2]["symbol"]["text"], "AND");

    // Wire the two comparisons into the AND and route its output to roll_ok,
    // then sweep the orphaned auto-local (what the canvas does on a rewire).
    post("/api/edit/update_equation",
        r#"{"node":"RangeCheck","index":2,"lhs":"roll_ok","body":"less_than0 and greater_than1"}"#);
    post("/api/edit/remove_port", r#"{"node":"RangeCheck","name":"and2"}"#);

    // The AND gate now has two BOUND input pins, wired by port, output to roll_ok.
    let d = json(port, "/api/diagram?node=RangeCheck");
    let and_pins = d["equations"][2]["inputs"].as_array().unwrap();
    assert_eq!(and_pins[0]["name"], "less_than0");
    assert_eq!(and_pins[1]["name"], "greater_than1");
    assert!(and_pins.iter().all(|p| p["bound"] == true), "both wired: {and_pins:?}");
    let wires = d["wires"].as_array().unwrap();
    assert!(wires.iter().any(|w| w["from"] == "less_than0" && w["to"] == "eq2" && w["to_port"] == 0));
    assert!(wires.iter().any(|w| w["from"] == "greater_than1" && w["to"] == "eq2" && w["to_port"] == 1));
    assert!(wires.iter().any(|w| w["from"] == "eq2" && w["to"] == "roll_ok"));
    // No leftover problems — the model is well-typed.
    assert!(d["problems"].as_array().unwrap().is_empty(), "clean model: {:?}", d["problems"]);

    post("/api/edit/set_main", r#"{"main":"RangeCheck"}"#);

    // The model became Lustre that functionally represents the logic…
    let (s, lus) = request(port, "GET", "/api/lustre", "").expect("lustre");
    assert_eq!(s, 200);
    assert!(lus.contains("node RangeCheck"), "lustre: {lus}");
    assert!(lus.contains("less_than0 = roll < 3.141"), "comparison in lustre: {lus}");
    assert!(lus.contains("roll_ok = less_than0 and greater_than1"), "AND in lustre: {lus}");

    // …and that Lustre became C-Lite that computes it.
    let (s, csrc) = request(port, "GET", "/api/clite/source", "").expect("clite source");
    assert_eq!(s, 200);
    assert!(csrc.contains("void RangeCheck_step"), "C step fn: {csrc}");
    assert!(csrc.contains("&&"), "AND lowered to C: {csrc}");
    assert!(csrc.contains("in->roll"), "reads the input: {csrc}");
}

/// The SCADE build pipeline: building the model checks validity and writes
/// `<main>.lus` to the project folder, and a log message (debug probe)
/// round-trips and lands in the generated C under `#ifdef OL_DEBUG`.
#[test]
fn build_writes_lus_and_log_messages_reach_generated_c() {
    let g = start_server_on_copy();
    let port = g.port;
    let json = |path: &str, body: &str| -> (u16, serde_json::Value) {
        let (s, b) = request(port, "POST", path, body).expect(path);
        let v = serde_json::from_str(&b).unwrap_or(serde_json::Value::Null);
        (s, v)
    };

    // The example's main is ReleaseLogic — a clean model. Build it.
    let (s, d) = json("/api/build", "");
    assert_eq!(s, 200, "build: {d}");
    assert_eq!(d["ok"], true, "model should build: {d}");
    assert!(d["lustre"].as_str().unwrap().contains("node ReleaseLogic"), "lustre: {d}");
    // …and the operator's Lustre is now a file in the project folder.
    let lus = g.tmp.join("ReleaseLogic.lus");
    assert!(lus.exists(), "expected {} to be written", lus.display());
    assert!(std::fs::read_to_string(&lus).unwrap().contains("node ReleaseLogic"));

    // Add a log message for an output, SCADE's "log message" probe.
    let (s, _) = request(port, "POST", "/api/edit/add_probe",
        r#"{"node":"ReleaseLogic","var":"release_cmd","label":"cmd"}"#).expect("add_probe");
    assert_eq!(s, 200);
    let (s, b) = request(port, "GET", "/api/diagram?node=ReleaseLogic", "").unwrap();
    assert_eq!(s, 200);
    let dg: serde_json::Value = serde_json::from_str(&b).unwrap();
    let probes = dg["probes"].as_array().unwrap();
    assert!(probes.iter().any(|p| p["var"] == "release_cmd" && p["label"] == "cmd"), "{probes:?}");

    // The generated C carries the probe, guarded so production builds skip it.
    let (s, csrc) = request(port, "GET", "/api/clite/source", "").unwrap();
    assert_eq!(s, 200);
    assert!(csrc.contains("#ifdef OL_DEBUG"), "debug guard: {csrc}");
    assert!(csrc.contains("\"cmd: %s\\n\""), "probe printf: {csrc}");

    // A probe on an unknown variable is rejected.
    let (s, _) = request(port, "POST", "/api/edit/add_probe",
        r#"{"node":"ReleaseLogic","var":"nope","label":"x"}"#).unwrap();
    assert_eq!(s, 400, "unknown probe var must be rejected");
}

/// A model with an error does not build, is not written, and stays gated.
#[test]
fn an_invalid_model_does_not_build() {
    let g = start_server_on_copy();
    let port = g.port;
    for (p, b) in [
        ("/api/edit/add_node", r#"{"name":"Broken","kind":"operator"}"#),
        ("/api/edit/add_port", r#"{"node":"Broken","side":"output","name":"y","type":"bool"}"#),
        ("/api/edit/add_equation", r#"{"node":"Broken","lhs":"y","body":"y and missing"}"#),
        ("/api/edit/set_main", r#"{"main":"Broken"}"#),
    ] {
        let (s, m) = request(port, "POST", p, b).expect(p);
        assert_eq!(s, 200, "{p}: {m}");
    }
    let (s, body) = request(port, "POST", "/api/build", "").expect("build");
    assert_eq!(s, 200);
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["ok"], false, "broken model must not build: {d}");
    assert!(d["errors"].as_u64().unwrap() >= 1);
    assert!(!g.tmp.join("Broken.lus").exists(), "no .lus for a model that didn't build");
}

#[test]
fn bad_equation_body_is_rejected_with_400() {
    let g = start_server_on_copy();
    let (s, body) = request(
        g.port,
        "POST",
        "/api/edit/add_equation",
        r#"{"node":"ReleaseLogic","lhs":"release_cmd","body":"if then else"}"#,
    )
    .expect("bad equation");
    assert_eq!(s, 400, "expected 400, got {s}: {body}");
    // And the model file was left untouched by the failed edit.
    let on_disk = std::fs::read_to_string(g.tmp.join("model.json")).unwrap();
    assert!(!on_disk.contains("if then else"));
}
