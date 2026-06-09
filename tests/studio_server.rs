//! Phase 8 GUI back-end: drive the `openlustre studio serve` HTTP server
//! end-to-end. The test spawns the server on a random port, hits each
//! endpoint with raw TCP, and verifies the responses match the documented
//! schema (`apps/studio_ui/README.md`).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

struct ServerGuard {
    child: Child,
    port: u16,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_server() -> ServerGuard {
    let model = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/release_logic/model/release_logic.json");

    let mut child = Command::new(env!("CARGO"))
        .args([
            "run", "-q", "-p", "ol_cli", "--",
            "studio", "serve",
        ])
        .arg(&model)
        .arg("--port")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cargo run studio serve");

    // Read stdout until we see the printed `http://127.0.0.1:<port>` line.
    use std::io::BufRead;
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut port = None;
    let mut tries = 0;
    while tries < 200 {
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
        tries += 1;
    }
    let port = port.expect("server should print bound port");

    // Belt-and-braces: poll the health endpoint until it answers (the
    // listener can be ready slightly after the printed line).
    for _ in 0..50 {
        if http_get(port, "/api/health").is_some() {
            break;
        }
        sleep(Duration::from_millis(50));
    }

    ServerGuard { child, port }
}

fn http_get(port: u16, path: &str) -> Option<(u16, String, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).ok()?;
    stream.shutdown(Shutdown::Write).ok();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    parse_response(&buf)
}

fn http_post(port: u16, path: &str, body: &str) -> Option<(u16, String, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    stream.write_all(req.as_bytes()).ok()?;
    stream.shutdown(Shutdown::Write).ok();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    parse_response(&buf)
}

fn parse_response(raw: &str) -> Option<(u16, String, String)> {
    let (head, body) = raw.split_once("\r\n\r\n")?;
    let mut lines = head.lines();
    let status_line = lines.next()?;
    let status: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    let mut ctype = String::new();
    for h in lines {
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-type:") {
            ctype = v.trim().to_string();
        }
    }
    Some((status, ctype, body.to_string()))
}

#[test]
fn studio_server_health_root_inspect_lustre_clite_and_simulate() {
    let g = start_server();
    let port = g.port;

    // /api/health
    let (s, _, body) = http_get(port, "/api/health").expect("health");
    assert_eq!(s, 200);
    assert_eq!(body, "ok");

    // / serves the SPA HTML.
    let (s, ctype, body) = http_get(port, "/").expect("root");
    assert_eq!(s, 200);
    assert!(ctype.contains("text/html"));
    assert!(body.contains("OpenLustre Studio"));
    assert!(body.contains("/api/inspect"));

    // /api/inspect returns the documented schema.
    let (s, ctype, body) = http_get(port, "/api/inspect").expect("inspect");
    assert_eq!(s, 200);
    assert!(ctype.contains("application/json"));
    let v: serde_json::Value =
        serde_json::from_str(&body).expect("inspect returns valid JSON");
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["project"]["name"], "release_authorization");
    let nodes = v["project"]["packages"][0]["nodes"].as_array().unwrap();
    assert!(nodes.iter().any(|n| n["name"] == "ReleaseLogic"));

    // /api/lustre returns Lustre + CoCoSpec text.
    let (s, _, body) = http_get(port, "/api/lustre").expect("lustre");
    assert_eq!(s, 200);
    assert!(body.contains("node ReleaseLogic"));
    assert!(body.contains("contract ReleaseLogic_contract"));

    // /api/clite/header returns generated C header with typedefs.
    let (s, _, body) = http_get(port, "/api/clite/header").expect("clite header");
    assert_eq!(s, 200);
    assert!(body.contains("ReleaseLogic_Input"));
    assert!(body.contains("ReleaseLogic_Output"));

    // POST /api/simulate with a CSV row.
    let csv = "master_arm,station_selected,consent,fault_present,release_request\ntrue,true,true,false,true\n";
    let (s, ctype, body) = http_post(port, "/api/simulate", csv).expect("simulate");
    assert_eq!(s, 200);
    assert!(ctype.contains("text/csv"));
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines[0], "cycle,release_cmd,inhibit,active_mode,violations");
    // With master_arm + consent + station + request + no fault → release_cmd=true.
    assert!(lines[1].starts_with("0,true,false,"));

    // 404 on unknown paths.
    let (s, _, _) = http_get(port, "/does/not/exist").expect("404");
    assert_eq!(s, 404);

    // The Build tab needs a driver + Makefile so the user-defined main
    // operator becomes a standalone executable in one `make`.
    let (s, _, body) = http_get(port, "/api/clite/driver").expect("driver");
    assert_eq!(s, 200);
    assert!(body.contains("ReleaseLogic_step"), "driver text: {body}");
    assert!(body.contains("int main"));

    let (s, _, body) = http_get(port, "/api/clite/makefile").expect("makefile");
    assert_eq!(s, 200);
    assert!(body.contains("TARGET ?= ReleaseLogic"));
    assert!(body.contains("$(CC)"));
    assert!(body.contains("openlustre_generated.c driver.c"));

    // The SPA must include the new Step + Build tabs so the GUI panels we
    // built are actually reachable.
    let (_, _, html) = http_get(port, "/").expect("root");
    assert!(html.contains("data-tab=\"step\""), "Step tab missing");
    assert!(html.contains("data-tab=\"build\""), "Build tab missing");
}
