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
