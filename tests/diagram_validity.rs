//! Diagram validity: invalid or problematic connections are color-coded by
//! the client because the server marks them — undeclared names become red
//! "ghost" boxes with red wires, typecheck errors land on the equation boxes
//! and defining wires they belong to, and the drawing grid persists in the
//! model file alongside the positions (the layout-metadata role SCADE plays
//! with its .etp/.xscade sidecars).

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

fn diagram(port: u16, node: &str) -> serde_json::Value {
    let (s, body) = request(port, "GET", &format!("/api/diagram?node={node}"), "")
        .expect("diagram request");
    assert_eq!(s, 200, "diagram failed: {body}");
    serde_json::from_str(&body).unwrap()
}

fn find_wire<'a>(
    d: &'a serde_json::Value,
    from: &str,
    to: &str,
) -> Option<&'a serde_json::Value> {
    d["wires"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["from"] == from && w["to"] == to)
}

// --- Undeclared names: ghost boxes + red wires ------------------------------

#[test]
fn undeclared_read_becomes_a_ghost_with_an_invalid_wire() {
    let g = start_server_on_copy("ghost");
    let port = g.port;

    for (path, body) in [
        ("/api/edit/add_node", r#"{"name":"Bad","kind":"operator"}"#),
        ("/api/edit/add_port", r#"{"node":"Bad","side":"input","name":"a","type":"bool"}"#),
        ("/api/edit/add_port", r#"{"node":"Bad","side":"output","name":"y","type":"bool"}"#),
        ("/api/edit/add_equation", r#"{"node":"Bad","lhs":"y","body":"a and zz"}"#),
    ] {
        let (s, b) = request(port, "POST", path, body).expect(path);
        assert_eq!(s, 200, "{path} failed: {b}");
    }

    let d = diagram(port, "Bad");
    // `zz` is not declared anywhere: it shows up as a ghost box...
    let ghosts = d["ghosts"].as_array().unwrap();
    assert!(
        ghosts.iter().any(|g| g["name"] == "zz"),
        "expected ghost for zz, got: {ghosts:?}"
    );
    // ...with a red (invalid) wire into the consuming equation.
    let w = find_wire(&d, "zz", "eq0").expect("wire zz -> eq0");
    assert_eq!(w["invalid"], true, "ghost wire must be invalid: {w}");
    assert!(w["reason"].as_str().unwrap().contains("zz"));
    // The legitimate read is untouched: a -> eq0 carries no invalid flag.
    let ok = find_wire(&d, "a", "eq0").expect("wire a -> eq0");
    assert!(ok.get("invalid").is_none(), "valid wire flagged: {ok}");
}

// --- Type mismatch: the equation box and its defining wire go red -----------

#[test]
fn type_mismatch_marks_the_equation_and_its_defining_wire_invalid() {
    let g = start_server_on_copy("mismatch");
    let port = g.port;

    for (path, body) in [
        ("/api/edit/add_node", r#"{"name":"Mix","kind":"operator"}"#),
        ("/api/edit/add_port", r#"{"node":"Mix","side":"input","name":"x","type":"int32"}"#),
        ("/api/edit/add_port", r#"{"node":"Mix","side":"output","name":"y","type":"bool"}"#),
        ("/api/edit/add_equation", r#"{"node":"Mix","lhs":"y","body":"x + 1"}"#),
    ] {
        let (s, b) = request(port, "POST", path, body).expect(path);
        assert_eq!(s, 200, "{path} failed: {b}");
    }

    let d = diagram(port, "Mix");
    assert_eq!(d["equations"][0]["invalid"], true, "{}", d["equations"][0]);
    assert!(
        d["equations"][0]["reason"].as_str().unwrap().contains("declared as"),
        "reason should carry the typecheck message: {}",
        d["equations"][0]["reason"]
    );
    let w = find_wire(&d, "eq0", "y").expect("wire eq0 -> y");
    assert_eq!(w["invalid"], true, "defining wire must be red: {w}");
}

// --- Unconnected output: the port box itself is the problem -----------------

#[test]
fn never_assigned_output_is_marked_invalid_on_the_box() {
    let g = start_server_on_copy("unassigned");
    let port = g.port;

    for (path, body) in [
        ("/api/edit/add_node", r#"{"name":"Hollow","kind":"operator"}"#),
        ("/api/edit/add_port", r#"{"node":"Hollow","side":"output","name":"out","type":"bool"}"#),
    ] {
        let (s, b) = request(port, "POST", path, body).expect(path);
        assert_eq!(s, 200, "{path} failed: {b}");
    }

    let d = diagram(port, "Hollow");
    let out = &d["outputs"][0];
    assert_eq!(out["invalid"], true, "unassigned output not flagged: {out}");
    assert!(out["reason"].as_str().unwrap().contains("never assigned"));
}

// --- A clean model carries no red anywhere ----------------------------------

#[test]
fn clean_model_has_no_invalid_wires_ghosts_or_problems() {
    let g = start_server_on_copy("clean");
    let d = diagram(g.port, "ReleaseLogic");
    assert!(d["ghosts"].as_array().unwrap().is_empty(), "{}", d["ghosts"]);
    assert!(d["problems"].as_array().unwrap().is_empty(), "{}", d["problems"]);
    for w in d["wires"].as_array().unwrap() {
        assert!(w.get("invalid").is_none(), "clean wire flagged: {w}");
    }
    for eq in d["equations"].as_array().unwrap() {
        assert_eq!(eq["invalid"], false, "clean equation flagged: {eq}");
    }
}

// --- Grid metadata: persists with the layout, validated on write ------------

#[test]
fn grid_pitch_persists_to_the_model_file_and_round_trips() {
    let g = start_server_on_copy("grid");
    let port = g.port;

    let payload = r#"{"node":"ReleaseLogic","grid":16,"positions":{
        "eq0":{"x":304.0,"y":80.0}
    }}"#;
    let (s, body) = request(port, "POST", "/api/edit/set_layout", payload).expect("set_layout");
    assert_eq!(s, 200, "set_layout failed: {body}");

    let d = diagram(port, "ReleaseLogic");
    assert_eq!(d["grid"], 16, "grid not served back: {}", d["grid"]);
    assert_eq!(d["positions"]["eq0"]["x"], 304.0);

    // Durable in the model file — the drawing metadata travels with the model.
    let on_disk = std::fs::read_to_string(g.tmp.join("model.json")).unwrap();
    assert!(on_disk.contains("\"grid\""), "grid not saved to disk");

    // A nonsense pitch is rejected and the stored one survives.
    let bad = r#"{"node":"ReleaseLogic","grid":0,"positions":{}}"#;
    let (s, _) = request(port, "POST", "/api/edit/set_layout", bad).expect("bad grid");
    assert_eq!(s, 400, "grid 0 must be rejected");
    let d = diagram(port, "ReleaseLogic");
    assert_eq!(d["grid"], 16, "stored grid lost after rejected write");

    // Layout saves without a grid leave the stored pitch untouched.
    let nogrid = r#"{"node":"ReleaseLogic","positions":{"eq0":{"x":312.0,"y":88.0}}}"#;
    let (s, _) = request(port, "POST", "/api/edit/set_layout", nogrid).expect("no grid");
    assert_eq!(s, 200);
    let d = diagram(port, "ReleaseLogic");
    assert_eq!(d["grid"], 16);
}
