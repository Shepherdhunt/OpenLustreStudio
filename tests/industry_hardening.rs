//! Industry-deployment hardening: generated C must compile for ANY legal
//! model name (C keywords included), and every editor action must be
//! undoable/redoable through the server's edit journal.

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

// --- C keyword names in the model must not break the generated code ---------

#[test]
fn c_keyword_variable_names_compile_and_match_the_ir() {
    let tmp = make_tempdir("keywords");
    let model = tmp.join("model.json");
    // input `unsigned`, local `for`, output `double` — all legal model names,
    // all C keywords.
    let project = serde_json::json!({
        "name": "hostile",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Hostile",
                "kind": "Function",
                "inputs": [{"name": "unsigned", "ty": {"kind": "Int32"}}],
                "outputs": [{"name": "double", "ty": {"kind": "Int32"}}],
                "locals": [{"name": "for", "ty": {"kind": "Int32"}}],
                "equations": [
                    {"lhs": ["for"], "rhs": {"expr": "Binary", "op": "Add",
                        "lhs": {"expr": "Var", "name": "unsigned"},
                        "rhs": {"expr": "Const", "lit": {"lit": "Int", "value": 1}}}},
                    {"lhs": ["double"], "rhs": {"expr": "Binary", "op": "Mul",
                        "lhs": {"expr": "Var", "name": "for"},
                        "rhs": {"expr": "Const", "lit": {"lit": "Int", "value": 2}}}}
                ]
            }]
        }],
        "main": "Hostile"
    });
    std::fs::write(&model, serde_json::to_string_pretty(&project).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    // The CSV header keeps the model's own name, keyword or not.
    std::fs::write(scen.join("vals.csv"), "unsigned\n0\n5\n-3\n").unwrap();

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
    let (ok, out) = run(&["test", "record", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap()]);
    assert!(ok, "record: {out}");
    let golden = std::fs::read_to_string(scen.join("vals.golden.csv")).unwrap();
    assert!(golden.contains("1,5,6,12"), "values must flow: {golden}");
    let (ok, out) = run(&["test", "run", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "both"]);
    assert!(ok, "run: {out}");
    assert!(out.contains("[PASS] vals (ir)"), "{out}");
    assert!(out.contains("[PASS] vals (c )"), "keyword names broke the C build: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

// --- Undo/redo through the server's edit journal -----------------------------

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

fn start_server_on_workspace(tag: &str) -> ServerGuard {
    let tmp = make_tempdir(tag);
    let ws = tmp.join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let mut child = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "studio", "serve"])
        .arg(&ws)
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

fn inspect(port: u16) -> serde_json::Value {
    let (s, body) = request(port, "GET", "/api/inspect", "").expect("inspect");
    assert_eq!(s, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

fn node_names(ins: &serde_json::Value) -> Vec<String> {
    ins["project"]["packages"].as_array().unwrap().iter()
        .filter(|p| p["name"] != "stdlib")
        .flat_map(|p| p["nodes"].as_array().unwrap())
        .map(|n| n["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_edit_is_undoable_and_redoable() {
    let g = start_server_on_workspace("undo");
    let port = g.port;

    // Two edits: a node, then a port on it.
    let (s, b) = request(port, "POST", "/api/edit/add_node",
        r#"{"name":"Scratch","kind":"operator"}"#).unwrap();
    assert_eq!(s, 200, "{b}");
    let (s, b) = request(port, "POST", "/api/edit/add_port",
        r#"{"node":"Scratch","side":"input","name":"x","type":"int32"}"#).unwrap();
    assert_eq!(s, 200, "{b}");

    let ins = inspect(port);
    assert!(node_names(&ins).contains(&"Scratch".to_string()));
    assert_eq!(ins["history"]["undo"], 2, "{}", ins["history"]);
    assert_eq!(ins["history"]["redo"], 0);

    // Undo the port, then the node.
    let (s, _) = request(port, "POST", "/api/edit/undo", "").unwrap();
    assert_eq!(s, 200);
    let (s, _) = request(port, "POST", "/api/edit/undo", "").unwrap();
    assert_eq!(s, 200);
    let ins = inspect(port);
    assert!(!node_names(&ins).contains(&"Scratch".to_string()), "undo must remove the node");
    assert_eq!(ins["history"]["redo"], 2);

    // Redo brings it back, port included.
    let (s, _) = request(port, "POST", "/api/edit/redo", "").unwrap();
    assert_eq!(s, 200);
    let (s, _) = request(port, "POST", "/api/edit/redo", "").unwrap();
    assert_eq!(s, 200);
    let ins = inspect(port);
    assert!(node_names(&ins).contains(&"Scratch".to_string()));
    let scratch = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().unwrap())
        .find(|n| n["name"] == "Scratch").unwrap();
    assert_eq!(scratch["inputs"][0]["name"], "x", "redo must restore the port too");

    // A fresh edit invalidates the redo branch; an empty stack is a 400.
    let (s, _) = request(port, "POST", "/api/edit/undo", "").unwrap();
    assert_eq!(s, 200);
    let (s, _) = request(port, "POST", "/api/edit/add_local",
        r#"{"node":"Scratch","name":"t","type":"bool"}"#).unwrap();
    assert_eq!(s, 200);
    let ins = inspect(port);
    assert_eq!(ins["history"]["redo"], 0, "new edit must clear redo");
    let (s, _) = request(port, "POST", "/api/edit/redo", "").unwrap();
    assert_eq!(s, 400, "empty redo must report, not corrupt");
}
