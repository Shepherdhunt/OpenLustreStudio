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

    // The SPA must expose stepping and code generation: the Simulation menu
    // (with the run-gated Step item), the Simulation dock, and the Compile
    // C-Lite menu entry.
    let (_, _, html) = http_get(port, "/").expect("root");
    assert!(html.contains("data-menu=\"simulation\""), "Simulation menu missing");
    assert!(html.contains("mi-sim-step"), "Step menu item missing");
    assert!(html.contains("data-dock=\"simulation\""), "Simulation dock missing");
    assert!(html.contains("mi-code-compile"), "Compile C-Lite menu item missing");
    assert!(html.contains("toolbox"), "Operations toolbox missing");
    assert!(html.contains("propsdock"), "Properties dock missing");
    assert!(html.contains("mi-undo"), "Edit > Undo missing");
    assert!(html.contains("data-dock=\"build\""), "Build dock tab missing");

    // The SCADE-style canvas tooling: the Diagram menu (align / distribute /
    // export), zoom (View menu items + the status-bar readout), the
    // orthogonal-wire toggle, and the operator glyph renderer.
    assert!(html.contains("data-menu=\"diagram\""), "Diagram menu missing");
    assert!(html.contains("alignSelected"), "align/distribute commands missing");
    assert!(html.contains("exportDiagram"), "diagram export missing");
    assert!(html.contains("status-zoom"), "status-bar zoom readout missing");
    assert!(html.contains("wire-orth"), "orthogonal-wires toggle missing");
    assert!(html.contains("function shapeSvg"), "operator glyph renderer missing");
    assert!(html.contains("function zoomFit"), "zoom-to-fit missing");
    assert!(html.contains("mm-show"), "minimap toggle missing");
    assert!(html.contains("function copySelection"), "diagram clipboard missing");
    assert!(html.contains("function fsmDraw"), "draggable state chart missing");
    assert!(html.contains("id=\"dlg-act\""), "activation editor dialog missing");
    assert!(html.contains("mi-insert-act"), "Insert > Activation missing");
    assert!(html.contains("function renderActTree"), "decision-tree chart missing");
    assert!(html.contains("function isConstructId"), "construct canvas blocks missing");
    // The contract editor: dialog, its three views, and the entry points.
    assert!(html.contains("id=\"dlg-contract\""), "contract editor dialog missing");
    assert!(html.contains("function ctRenderModes"), "mode table missing");
    assert!(html.contains("function ctFromText"), "CoCoSpec text view missing");
    assert!(html.contains("mi-insert-contract") && html.contains("mi-project-contracts"),
        "contract menu entries missing");
    // Live simulation: the session client and the canvas overlay.
    assert!(html.contains("/api/sim/step") && html.contains("function simAdvance"), "sim session client missing");
    assert!(html.contains("id=\"sim-break\"") && html.contains("id=\"sim-stop-viol\""), "run controls missing");
    assert!(html.contains("function liveValues"), "live diagram overlay missing");
    // The waveform viewer and its three uses: the session's cycles (with
    // cycle review on the diagram), scenario runs against their goldens, and
    // Kind 2 counterexamples — the last two replayable in the simulator.
    assert!(html.contains("function waveMount") && html.contains("function waveDraw"), "waveform viewer missing");
    assert!(html.contains("id=\"sim-wave\"") && html.contains("function simPick"), "simulation waveform missing");
    assert!(html.contains("function testsWave") && html.contains("id=\"tests-wave\""), "scenario waveform missing");
    assert!(html.contains("function verifyShowCex") && html.contains("id=\"verify-wave\""), "counterexample waveform missing");
    assert!(html.contains("function simReplay") && html.contains("sequence"), "replay in the simulator missing");
    assert!(html.contains("function dockToggleMax"), "resizable dock missing");
    // C in the loop: the toggle, the attach call, and the compared overlay.
    assert!(html.contains("id=\"sim-cil\"") && html.contains("/api/sim/c"), "C-in-the-loop toggle missing");
    assert!(html.contains("function simSetC") && html.contains("function simShownCDiff"), "C-in-the-loop client missing");
    assert!(html.contains("stop_on_divergence"), "stop on divergence missing");
}
