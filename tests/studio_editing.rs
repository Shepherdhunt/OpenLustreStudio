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

    // Bind it to `roll < 3.141`: the block keeps BOTH pins — the literal
    // operand renders as a value-carrying pin, so the full connection
    // contract stays visible and rewireable.
    post("/api/edit/update_equation",
        r#"{"node":"RangeCheck","index":0,"lhs":"less_than0","body":"roll < 3.141"}"#);
    let d = json(port, "/api/diagram?node=RangeCheck");
    let pins = d["equations"][0]["inputs"].as_array().unwrap();
    assert_eq!(pins.len(), 2, "both operand slots stay pins: {pins:?}");
    assert_eq!(pins[0]["name"], "roll");
    assert_eq!(pins[0]["bound"], true);
    assert_eq!(pins[1]["name"], "3.141");
    assert_eq!(pins[1]["kind"], "literal");
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
    // The operator got a blank .lus stub when it was created, but a failed
    // build must NOT fill it — only a clean build writes the operator's Lustre.
    let stub = g.tmp.join("Broken.lus");
    assert!(stub.exists(), "operator gets a blank stub on creation");
    let text = std::fs::read_to_string(&stub).unwrap();
    assert!(text.contains("has not been built yet"), "stub stays blank: {text}");
    assert!(!text.contains("node Broken"), "a failed build must not fill the stub: {text}");
}

/// Each operator has its own Lustre file: a blank stub the moment it is
/// created, filled only when *that* operator builds. The Build dock can target
/// any operator (not just the current root), and building it makes it the root
/// — so an unrelated clean operator can be built even while the example's own
/// root sits untouched.
#[test]
fn per_operator_lus_blank_on_create_filled_on_build() {
    let g = start_server_on_copy();
    let port = g.port;
    let post = |p: &str, b: &str| {
        let (s, m) = request(port, "POST", p, b).expect(p);
        assert_eq!(s, 200, "{p}: {m}");
    };

    post("/api/edit/add_node", r#"{"name":"Doubler2","kind":"operator"}"#);
    // The .lus exists immediately — blank, not yet the built Lustre.
    let lus = g.tmp.join("Doubler2.lus");
    assert!(lus.exists(), "a blank stub is written on create");
    let stub = std::fs::read_to_string(&lus).unwrap();
    assert!(stub.contains("has not been built yet"), "stub: {stub}");
    assert!(!stub.contains("node Doubler2"), "stub is not the built Lustre yet");

    post("/api/edit/add_port", r#"{"node":"Doubler2","side":"input","name":"x","type":"int32"}"#);
    post("/api/edit/add_port", r#"{"node":"Doubler2","side":"output","name":"y","type":"int32"}"#);
    post("/api/edit/add_equation", r#"{"node":"Doubler2","lhs":"y","body":"x + x"}"#);

    // The example's root is still ReleaseLogic — build Doubler2 by naming it.
    let (s, body) = request(port, "POST", "/api/build", r#"{"node":"Doubler2"}"#).expect("build");
    assert_eq!(s, 200);
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["ok"], true, "Doubler2 should build: {d}");
    assert_eq!(d["main"], "Doubler2", "the built operator becomes the root: {d}");

    // Its .lus is now the real Lustre, and the root switch persisted.
    let built = std::fs::read_to_string(&lus).unwrap();
    assert!(built.contains("node Doubler2"), "filled on build: {built}");
    assert!(built.contains("y = x + x"), "the equation is in the file: {built}");
    let (s, ins) = request(port, "GET", "/api/inspect", "").expect("inspect");
    assert_eq!(s, 200);
    let iv: serde_json::Value = serde_json::from_str(&ins).unwrap();
    assert_eq!(iv["project"]["main"], "Doubler2", "root persisted: {}", iv["project"]["main"]);
}

/// A dropped operation advertises its result as a "collapsible" output, so the
/// canvas draws the gate's own right pin as the result (red until consumed)
/// rather than a separate auto-local box. Wiring the result to a real output
/// makes it non-collapsible — the output keeps its box.
#[test]
fn dropped_operation_output_is_collapsible_until_wired() {
    let g = start_server_on_copy();
    let port = g.port;
    let post = |p: &str, b: &str| {
        let (s, m) = request(port, "POST", p, b).expect(p);
        assert_eq!(s, 200, "{p}: {m}");
    };
    let diagram = |node: &str| -> serde_json::Value {
        let (s, b) = request(port, "GET", &format!("/api/diagram?node={node}"), "").expect("diagram");
        assert_eq!(s, 200, "{b}");
        serde_json::from_str(&b).unwrap()
    };

    post("/api/edit/add_node", r#"{"name":"Summer","kind":"operator"}"#);
    post("/api/edit/add_port", r#"{"node":"Summer","side":"input","name":"a","type":"int32"}"#);
    post("/api/edit/add_port", r#"{"node":"Summer","side":"input","name":"b","type":"int32"}"#);
    post("/api/edit/add_port", r#"{"node":"Summer","side":"output","name":"s","type":"int32"}"#);

    // Drop a plus gate: its result is a fresh auto-local — collapsible and
    // unconsumed, so the canvas paints the gate's right pin red.
    post("/api/edit/add_operation", r#"{"node":"Summer","op":"plus","x":240.0,"y":40.0}"#);
    let d = diagram("Summer");
    let out = &d["equations"][0]["output"];
    assert_eq!(out["collapsible"], true, "auto-local result is collapsible: {out}");
    assert_eq!(out["bound"], true, "the auto-local is a declared local: {out}");
    let res = out["name"].as_str().unwrap().to_string();
    assert!(
        !d["wires"].as_array().unwrap().iter().any(|w| w["from"] == res.as_str()),
        "the result is unconsumed on drop (right pin renders red)"
    );

    // Route the result to the real output `s`.
    post("/api/edit/update_equation", r#"{"node":"Summer","index":0,"lhs":"s","body":"a + b"}"#);
    let d = diagram("Summer");
    let out = &d["equations"][0]["output"];
    assert_eq!(out["name"], "s");
    assert_eq!(out["bound"], true);
    assert_eq!(out["collapsible"], false, "an output keeps its own box, not collapsed: {out}");
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

/// SCADE-style copy/paste: duplicating a wired sub-diagram keeps its internal
/// wiring (renamed onto fresh `_copy` locals), keeps external reads pointing
/// at the originals, offsets the pasted boxes, and undoes as ONE edit.
#[test]
fn duplicate_equations_pastes_a_rewired_sub_diagram() {
    let g = start_server_on_copy();
    let port = g.port;
    let post = |p: &str, b: &str| {
        let (s, m) = request(port, "POST", p, b).expect(p);
        assert_eq!(s, 200, "{p}: {m}");
    };
    let diagram = |node: &str| -> serde_json::Value {
        let (s, b) = request(port, "GET", &format!("/api/diagram?node={node}"), "").expect("diagram");
        assert_eq!(s, 200, "{b}");
        serde_json::from_str(&b).unwrap()
    };

    post("/api/edit/add_node", r#"{"name":"Chain","kind":"operator"}"#);
    post("/api/edit/add_port", r#"{"node":"Chain","side":"input","name":"a","type":"int32"}"#);
    post("/api/edit/add_port", r#"{"node":"Chain","side":"input","name":"b","type":"int32"}"#);
    // eq0: sum = a + b (a local), eq1: twice = sum * 2 — a two-gate chain.
    post("/api/edit/add_local", r#"{"node":"Chain","name":"sum","type":"int32"}"#);
    post("/api/edit/add_local", r#"{"node":"Chain","name":"twice","type":"int32"}"#);
    post("/api/edit/add_equation", r#"{"node":"Chain","lhs":"sum","body":"a + b"}"#);
    post("/api/edit/add_equation", r#"{"node":"Chain","lhs":"twice","body":"sum * 2"}"#);
    post("/api/edit/set_layout",
        r#"{"node":"Chain","positions":{"eq0":{"x":100,"y":40},"eq1":{"x":300,"y":40}},"grid":8}"#);

    // Paste both.
    post("/api/edit/duplicate_equations", r#"{"node":"Chain","indices":[0,1],"dx":16,"dy":48}"#);
    let d = diagram("Chain");
    let eqs = d["equations"].as_array().unwrap();
    assert_eq!(eqs.len(), 4, "{eqs:?}");
    // The pasted pair is internally rewired: the copy of eq1 reads the COPY
    // of sum; the reads of a/b (outside the copied set) stay on the originals.
    assert_eq!(eqs[2]["lhs"][0], "sum_copy");
    assert_eq!(eqs[2]["body"], "a + b");
    assert_eq!(eqs[3]["lhs"][0], "twice_copy");
    assert_eq!(eqs[3]["body"], "sum_copy * 2");
    // The fresh locals exist and are typed like their sources.
    let locals = d["locals"].as_array().unwrap();
    for name in ["sum_copy", "twice_copy"] {
        let l = locals.iter().find(|l| l["name"] == name).expect(name);
        assert_eq!(l["type"]["kind"], "Int32", "{l}");
    }
    // Pasted boxes land offset from their sources.
    assert_eq!(d["positions"]["eq2"]["x"], 116.0);
    assert_eq!(d["positions"]["eq2"]["y"], 88.0);
    assert_eq!(d["positions"]["eq3"]["x"], 316.0);

    // Pasting again suffixes past the first copies.
    post("/api/edit/duplicate_equations", r#"{"node":"Chain","indices":[0],"dx":16,"dy":16}"#);
    let d = diagram("Chain");
    assert_eq!(d["equations"][4]["lhs"][0], "sum_copy2");

    // The whole paste is ONE journal entry: a single undo removes the pair.
    post("/api/edit/undo", "{}");
    post("/api/edit/undo", "{}");
    let d = diagram("Chain");
    assert_eq!(d["equations"].as_array().unwrap().len(), 2, "both pastes undone");
    assert!(!d["locals"].as_array().unwrap().iter().any(|l| l["name"] == "sum_copy"));

    // Out-of-range and empty index sets are loud 400s.
    let (s, m) = request(port, "POST", "/api/edit/duplicate_equations",
        r#"{"node":"Chain","indices":[9]}"#).unwrap();
    assert_eq!(s, 400, "{m}");
    let (s, _) = request(port, "POST", "/api/edit/duplicate_equations",
        r#"{"node":"Chain","indices":[]}"#).unwrap();
    assert_eq!(s, 400);
}

/// Requirements traceability: annotate operators with requirement IDs in the
/// Studio, read them back from the inspect, and compile the matrix with
/// `openlustre trace` (untraced operators reported; --strict turns them into
/// a failure).
#[test]
fn requirements_annotations_round_trip_and_trace_emits_the_matrix() {
    let g = start_server_on_copy();
    let port = g.port;
    let post = |p: &str, b: &str| {
        let (s, m) = request(port, "POST", p, b).expect(p);
        assert_eq!(s, 200, "{p}: {m}");
    };

    post("/api/edit/add_node", r#"{"name":"Interlock","kind":"operator"}"#);
    post("/api/edit/set_requirements",
        r#"{"node":"Interlock","requirements":[" SRS-042 ","SRS-107","SRS-042"]}"#);

    // Trimmed, deduplicated, and visible in the inspect tree data.
    let (s, b) = request(port, "GET", "/api/inspect", "").unwrap();
    assert_eq!(s, 200);
    let ins: serde_json::Value = serde_json::from_str(&b).unwrap();
    let node = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().unwrap())
        .find(|n| n["name"] == "Interlock").expect("Interlock in inspect");
    assert_eq!(node["requirements"], serde_json::json!(["SRS-042", "SRS-107"]));

    // Empty IDs are loud; unknown nodes are loud.
    let (s, _) = request(port, "POST", "/api/edit/set_requirements",
        r#"{"node":"Interlock","requirements":["  "]}"#).unwrap();
    assert_eq!(s, 400);
    let (s, _) = request(port, "POST", "/api/edit/set_requirements",
        r#"{"node":"Nope","requirements":["SRS-1"]}"#).unwrap();
    assert_eq!(s, 400);

    // The annotation persists in the model file (serde default keeps old
    // models loading; empty lists don't serialize).
    let on_disk = std::fs::read_to_string(g.tmp.join("model.json")).unwrap();
    assert!(on_disk.contains("SRS-107"), "requirements not saved");

    // Clause-level links: requirement IDs on individual contract clauses
    // (the rung below the operator). Annotate a guarantee and a mode of the
    // release-logic contract directly in the model file.
    let mut model: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(g.tmp.join("model.json")).unwrap()).unwrap();
    let contract = model["packages"].as_array_mut().unwrap().iter_mut()
        .flat_map(|p| p["contracts"].as_array_mut().unwrap())
        .find(|c| c["name"] == "ReleaseLogic_contract").expect("contract");
    let guarantee = contract["guarantees"].as_array_mut().unwrap().iter_mut()
        .find(|c| c["name"] == "release_implies_arm").expect("guarantee");
    guarantee["requirements"] = serde_json::json!(["SRS-077"]);
    let mode = contract["modes"].as_array_mut().unwrap().iter_mut()
        .find(|m| m["name"] == "SafeInhibit").expect("mode");
    mode["requirements"] = serde_json::json!(["SRS-078"]);
    std::fs::write(g.tmp.join("model.json"), serde_json::to_string_pretty(&model).unwrap())
        .unwrap();

    // `openlustre trace` compiles the matrix and reports untraced operators.
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "trace"])
        .arg(g.tmp.join("model.json"))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("requirement,operator,element"), "{text}");
    assert!(text.contains("SRS-042,Interlock,operator"), "{text}");
    assert!(text.contains("SRS-107,Interlock,operator"), "{text}");
    // Clause-level rows name the clause they hang from.
    assert!(text.contains("SRS-077,ReleaseLogic,guarantee release_implies_arm"), "{text}");
    assert!(text.contains("SRS-078,ReleaseLogic,mode SafeInhibit"), "{text}");
    // The release-logic model's own operators carry no annotations yet.
    assert!(text.contains("untraced operator(s):"), "{text}");

    // The design document tags the annotated clauses.
    let (s, doc) = request(port, "GET", "/api/doc", "").unwrap();
    assert_eq!(s, 200);
    assert!(doc.contains("-- [SRS-077]"), "clause tag missing from doc");

    // --strict gates on full coverage.
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "trace", "--strict"])
        .arg(g.tmp.join("model.json"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "--strict must fail with untraced operators");
}

/// SysML 2.0 association groundwork: an operator can name the SysML model
/// (and element) it realizes. The Studio edits it, the inspect warns when the
/// file is missing (W0170), `trace` reports the associations, `diff` sees the
/// change, and an empty model clears it.
#[test]
fn sysml_association_round_trip_warns_traces_and_clears() {
    let g = start_server_on_copy();
    let port = g.port;
    let post = |p: &str, b: &str| {
        let (s, m) = request(port, "POST", p, b).expect(p);
        assert_eq!(s, 200, "{p}: {m}");
    };
    let inspect = || {
        let (s, b) = request(port, "GET", "/api/inspect", "").unwrap();
        assert_eq!(s, 200);
        serde_json::from_str::<serde_json::Value>(&b).unwrap()
    };
    let release_logic = |ins: &serde_json::Value| {
        ins["project"]["packages"].as_array().unwrap().iter()
            .flat_map(|p| p["nodes"].as_array().unwrap())
            .find(|n| n["name"] == "ReleaseLogic").expect("ReleaseLogic in inspect").clone()
    };

    post("/api/edit/set_sysml",
        r#"{"node":"ReleaseLogic","model":" models/sys.sysml ","element":"Pkg::Rel"}"#);

    // The association round-trips (trimmed), and the missing file is a loud
    // W0170 warning in the inspect.
    let ins = inspect();
    let node = release_logic(&ins);
    assert_eq!(node["sysml"]["model"], "models/sys.sysml");
    assert_eq!(node["sysml"]["element"], "Pkg::Rel");
    let w0170 = ins["diagnostics"].as_array().unwrap().iter()
        .find(|d| d["code"] == "W0170");
    let w = w0170.expect("missing sysml file must warn");
    assert_eq!(w["source"], "sysml");
    assert!(w["message"].as_str().unwrap().contains("models/sys.sysml"), "{w}");

    // `openlustre diff` against the pristine example reports the association.
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/release_logic/model/release_logic.json");
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "diff"])
        .arg(&src)
        .arg(g.tmp.join("model.json"))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "models differ, diff must exit nonzero");
    assert!(
        text.contains("sysml (none) -> models/sys.sysml::Pkg::Rel"),
        "diff must report the sysml change: {text}"
    );

    // Creating the file clears the warning.
    std::fs::create_dir_all(g.tmp.join("models")).unwrap();
    std::fs::write(g.tmp.join("models/sys.sysml"), "package Pkg { part def Rel; }\n").unwrap();
    let ins = inspect();
    assert!(
        !ins["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "W0170"),
        "warning must clear once the file exists"
    );

    // `trace` reports the association alongside the requirements matrix, and
    // the design document labels the operator with it.
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "trace"])
        .arg(g.tmp.join("model.json"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("-- sysml association(s): ReleaseLogic -> models/sys.sysml::Pkg::Rel"),
        "{text}"
    );
    let (s, doc) = request(port, "GET", "/api/doc", "").unwrap();
    assert_eq!(s, 200);
    assert!(doc.contains("realizes SysML"), "doc label missing");
    assert!(doc.contains("models/sys.sysml::Pkg::Rel"), "doc association missing");

    // An empty model clears the association; unknown nodes are loud.
    post("/api/edit/set_sysml", r#"{"node":"ReleaseLogic","model":""}"#);
    let ins = inspect();
    assert!(release_logic(&ins).get("sysml").map_or(true, |s| s.is_null()), "cleared");
    let (s, _) = request(port, "POST", "/api/edit/set_sysml",
        r#"{"node":"Nope","model":"m.sysml"}"#).unwrap();
    assert_eq!(s, 400);

    // The whole thing is journaled: undo restores the association.
    post("/api/edit/undo", "{}");
    let ins = inspect();
    assert_eq!(release_logic(&ins)["sysml"]["model"], "models/sys.sysml");
}

/// SysML 2.0 requirements lifting: the associated `.sysml` file's
/// requirement definitions and `satisfy` relationships feed the trace matrix;
/// annotated IDs the model does not declare are warned about (W0171 in the
/// inspect, a report line + --strict failure in `openlustre trace`); the
/// design document shows the satisfied requirements with their doc text.
#[test]
fn sysml_requirements_lift_into_the_trace_matrix() {
    let g = start_server_on_copy();
    let port = g.port;
    let post = |p: &str, b: &str| {
        let (s, m) = request(port, "POST", p, b).expect(p);
        assert_eq!(s, 200, "{p}: {m}");
    };

    std::fs::create_dir_all(g.tmp.join("models")).unwrap();
    std::fs::write(
        g.tmp.join("models/flight.sysml"),
        r#"
        package Flight {
            requirement def <'SRS-201'> ReleaseReq {
                doc /* The system shall release only when armed. */
            }
            requirement def <'SRS-202'> StationReq;
            part def Rel;
            satisfy ReleaseReq by Rel;
            satisfy StationReq by Elsewhere;
        }
        "#,
    )
    .unwrap();
    post("/api/edit/set_sysml",
        r#"{"node":"ReleaseLogic","model":"models/flight.sysml","element":"Flight::Rel"}"#);
    // One ID the model declares, one it does not.
    post("/api/edit/set_requirements",
        r#"{"node":"ReleaseLogic","requirements":["SRS-201","SRS-999"]}"#);

    // The inspect warns (W0171) about the undeclared ID only.
    let (s, b) = request(port, "GET", "/api/inspect", "").unwrap();
    assert_eq!(s, 200);
    let ins: serde_json::Value = serde_json::from_str(&b).unwrap();
    let w0171: Vec<&str> = ins["diagnostics"].as_array().unwrap().iter()
        .filter(|d| d["code"] == "W0171")
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    assert_eq!(w0171.len(), 1, "exactly the undeclared ID warns: {w0171:?}");
    assert!(w0171[0].contains("SRS-999"), "{w0171:?}");

    // `trace`: the satisfy row rides in the matrix; the summary names the
    // model; the undeclared annotation is reported.
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "trace"])
        .arg(g.tmp.join("model.json"))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("SRS-201,ReleaseLogic,operator"), "{text}");
    assert!(text.contains("SRS-201,ReleaseLogic,sysml satisfy"), "{text}");
    assert!(!text.contains("SRS-202,ReleaseLogic"), "satisfy of another element must not leak: {text}");
    assert!(
        text.contains("-- sysml model(s): models/flight.sysml: 2 requirement(s), 2 satisfy link(s)"),
        "{text}"
    );
    assert!(
        text.contains("-- requirement(s) not declared in the associated sysml model: SRS-999 (ReleaseLogic -> models/flight.sysml)"),
        "{text}"
    );

    // --strict gates on the undeclared annotation.
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "trace", "--strict"])
        .arg(g.tmp.join("model.json"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "--strict must fail on an undeclared requirement");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("missing from the associated SysML model"), "{text}");

    // The design document lists the satisfied requirement with its doc text.
    let (s, doc) = request(port, "GET", "/api/doc", "").unwrap();
    assert_eq!(s, 200);
    assert!(doc.contains("SysML requirement"), "doc table missing");
    assert!(doc.contains("SRS-201"), "doc id missing");
    assert!(doc.contains("The system shall release only when armed."), "doc text missing");
}

/// Operand-slot pins: an operation block always exposes one connectable pin
/// per operand — a variable pin wires to its source, a literal or
/// sub-expression pin shows its value, a repeated variable gets one pin (and
/// wire) per slot — and `/api/edit/rewire_operand` replaces exactly the slot
/// a wire is dropped on. Growing a variadic operation's inputs grows the pin
/// list.
#[test]
fn operand_slot_pins_expose_every_input_and_rewire_by_port() {
    let g = start_server_on_copy();
    let port = g.port;
    let post = |p: &str, b: &str| {
        let (s, m) = request(port, "POST", p, b).expect(p);
        assert_eq!(s, 200, "{p}: {m}");
    };
    let diagram = || {
        let (s, b) = request(port, "GET", "/api/diagram?node=Pins", "").unwrap();
        assert_eq!(s, 200, "{b}");
        serde_json::from_str::<serde_json::Value>(&b).unwrap()
    };
    let pins = |d: &serde_json::Value| -> Vec<(String, String, bool)> {
        d["equations"][0]["inputs"].as_array().unwrap().iter()
            .map(|p| (
                p["name"].as_str().unwrap().to_string(),
                p["kind"].as_str().unwrap_or("").to_string(),
                p["bound"].as_bool().unwrap(),
            ))
            .collect()
    };
    let wire_ports = |d: &serde_json::Value| -> Vec<(String, i64)> {
        let mut v: Vec<(String, i64)> = d["wires"].as_array().unwrap().iter()
            .filter(|w| w["to"] == "eq0" && w["to_port"].is_i64() || w["to"] == "eq0" && w["to_port"].is_u64())
            .map(|w| (w["from"].as_str().unwrap().to_string(), w["to_port"].as_i64().unwrap()))
            .collect();
        v.sort();
        v
    };

    post("/api/edit/add_node", r#"{"name":"Pins","kind":"operator"}"#);
    for p in [r#"{"node":"Pins","side":"input","name":"x","type":"int32"}"#,
              r#"{"node":"Pins","side":"input","name":"y","type":"int32"}"#,
              r#"{"node":"Pins","side":"input","name":"c","type":"bool"}"#,
              r#"{"node":"Pins","side":"output","name":"z","type":"int32"}"#] {
        post("/api/edit/add_port", p);
    }
    post("/api/edit/add_expression", r#"{"node":"Pins","body":"x + 1"}"#);

    // A literal operand is still a pin: the block shows its full contract.
    let d = diagram();
    assert_eq!(
        pins(&d),
        vec![("x".into(), "var".into(), true), ("1".into(), "literal".into(), true)],
        "{d}"
    );
    assert_eq!(wire_ports(&d), vec![("x".to_string(), 0)]);
    let lhs = d["equations"][0]["lhs"][0].as_str().unwrap().to_string();

    // Dropping a wire on the literal pin replaces that operand exactly.
    post("/api/edit/rewire_operand", r#"{"node":"Pins","index":0,"port":1,"source":"y"}"#);
    let d = diagram();
    assert_eq!(d["equations"][0]["body"], "x + y", "{d}");
    assert_eq!(wire_ports(&d), vec![("x".to_string(), 0), ("y".to_string(), 1)]);

    // A repeated variable keeps one pin (and one wire) per slot.
    post("/api/edit/update_equation",
        &format!(r#"{{"node":"Pins","index":0,"lhs":"{lhs}","body":"x + x"}}"#));
    let d = diagram();
    assert_eq!(
        pins(&d),
        vec![("x".into(), "var".into(), true), ("x".into(), "var".into(), true)],
        "{d}"
    );
    assert_eq!(wire_ports(&d), vec![("x".to_string(), 0), ("x".to_string(), 1)]);
    // …and rewiring port 1 touches only that slot.
    post("/api/edit/rewire_operand", r#"{"node":"Pins","index":0,"port":1,"source":"y"}"#);
    assert_eq!(diagram()["equations"][0]["body"], "x + y");

    // A sub-expression operand is one pin; its variables wire into that port.
    post("/api/edit/update_equation",
        &format!(r#"{{"node":"Pins","index":0,"lhs":"{lhs}","body":"x + y * 2"}}"#));
    let d = diagram();
    assert_eq!(
        pins(&d),
        vec![("x".into(), "var".into(), true), ("y * 2".into(), "expr".into(), true)],
        "{d}"
    );
    assert_eq!(wire_ports(&d), vec![("x".to_string(), 0), ("y".to_string(), 1)]);

    // Growing the variadic operation grows the pins; shrinking drops them.
    post("/api/edit/update_equation",
        &format!(r#"{{"node":"Pins","index":0,"lhs":"{lhs}","body":"x + y"}}"#));
    post("/api/edit/set_operation_inputs", r#"{"node":"Pins","index":0,"inputs":5}"#);
    let d = diagram();
    let ps = pins(&d);
    assert_eq!(ps.len(), 5, "{d}");
    assert!(ps[2..].iter().all(|(_, k, bound)| k == "var" && !bound), "fresh pins unbound: {ps:?}");
    post("/api/edit/set_operation_inputs", r#"{"node":"Pins","index":0,"inputs":2}"#);
    assert_eq!(pins(&diagram()).len(), 2);

    // Fixed-arity shapes expose their slots too: if_then_else has 3 pins.
    post("/api/edit/update_equation",
        &format!(r#"{{"node":"Pins","index":0,"lhs":"{lhs}","body":"if c then x else 1"}}"#));
    let d = diagram();
    assert_eq!(
        pins(&d),
        vec![
            ("c".into(), "var".into(), true),
            ("x".into(), "var".into(), true),
            ("1".into(), "literal".into(), true),
        ],
        "{d}"
    );

    // Clock positions are variable names in the IR but pins all the same:
    // `x when c` shows the clock as its second pin, wired from `c`.
    post("/api/edit/update_equation",
        &format!(r#"{{"node":"Pins","index":0,"lhs":"{lhs}","body":"x when c"}}"#));
    let d = diagram();
    assert_eq!(
        pins(&d),
        vec![("x".into(), "var".into(), true), ("c".into(), "var".into(), true)],
        "{d}"
    );
    assert_eq!(wire_ports(&d), vec![("c".to_string(), 1), ("x".to_string(), 0)]);

    // Loud errors: unknown source, out-of-range port, a slotless block.
    let (s, m) = request(port, "POST", "/api/edit/rewire_operand",
        r#"{"node":"Pins","index":0,"port":0,"source":"nope"}"#).unwrap();
    assert_eq!(s, 400, "{m}");
    let (s, m) = request(port, "POST", "/api/edit/rewire_operand",
        r#"{"node":"Pins","index":0,"port":9,"source":"y"}"#).unwrap();
    assert_eq!(s, 400, "{m}");
    post("/api/edit/update_equation",
        &format!(r#"{{"node":"Pins","index":0,"lhs":"{lhs}","body":"x"}}"#));
    let (s, m) = request(port, "POST", "/api/edit/rewire_operand",
        r#"{"node":"Pins","index":0,"port":0,"source":"y"}"#).unwrap();
    assert_eq!(s, 400, "a pass-through box has no operand pins: {m}");
}

/// Project ▸ Design Document serves the report from the running Studio.
#[test]
fn design_document_endpoint_serves_the_report() {
    let g = start_server_on_copy();
    let (s, body) = request(g.port, "GET", "/api/doc", "").expect("doc");
    assert_eq!(s, 200, "{body}");
    assert!(body.contains("design document"), "{}", &body[..200.min(body.len())]);
    assert!(body.contains("<h2 id=\"op-ReleaseLogic\">"), "operator section missing");
    assert!(body.contains("Contract: ReleaseLogic_contract"), "contract section missing");
}

/// Cross/custom compiler: /api/clite/compile accepts an arbitrary GCC-style
/// driver command (the arm-none-eabi-gcc path), invoked by full path here;
/// a non-runnable command is a loud error, not a silent fallback.
#[test]
fn compile_accepts_a_custom_compiler_command() {
    // The host cc by absolute path exercises the custom-driver code path
    // (only bare "cc"/"gcc"/"clang"/"msvc" take the named paths).
    let cc = ["/usr/bin/cc", "/usr/bin/gcc", "/usr/bin/clang"]
        .iter()
        .find(|p| std::path::Path::new(p).exists());
    let Some(cc) = cc else { return }; // no host compiler: nothing to verify
    let g = start_server_on_copy();
    let out_dir = g.tmp.join("xbuild");
    let (s, body) = request(
        g.port,
        "POST",
        "/api/clite/compile",
        &format!(
            r#"{{"compiler":"{cc}","out_dir":"{}"}}"#,
            out_dir.display().to_string().replace('\\', "/")
        ),
    )
    .expect("compile");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["compiled"], true, "{body}");
    assert!(
        v["log"].as_str().unwrap_or("").contains(cc),
        "log should name the custom driver: {body}"
    );

    // A bogus command is rejected loudly.
    let (_, body) = request(
        g.port,
        "POST",
        "/api/clite/compile",
        r#"{"compiler":"no-such-compiler-xyz","out_dir":""}"#,
    )
    .expect("bad compile");
    assert!(body.contains("not runnable"), "{body}");
}
