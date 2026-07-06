//! The printout block: user-wired signals in, the special `terminal_out`
//! bool out. The IR simulator prints to stderr (the CSV trace on stdout is
//! untouched); generated C prints only under -DOL_DEBUG (production C-Lite
//! stays free of I/O); the Kind 2 view sees the constant `true`.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

fn make_tempdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_{tag}_{stamp}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn watcher_model() -> serde_json::Value {
    let eq = |lhs: &str, body: &str| {
        serde_json::json!({"lhs": [lhs], "rhs": ol_stdlib::parse_expr(body).unwrap()})
    };
    serde_json::json!({
        "name": "po",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Watcher",
                "kind": "Operator",
                "inputs": [
                    {"name": "speed", "ty": {"kind": "Int32"}},
                    {"name": "armed", "ty": {"kind": "Bool"}}
                ],
                "outputs": [
                    {"name": "twice", "ty": {"kind": "Int32"}},
                    {"name": "terminal_out", "ty": {"kind": "Bool"}}
                ],
                "equations": [
                    eq("twice", "speed * 2"),
                    eq("terminal_out", "printout(speed, armed)")
                ]
            }]
        }],
        "main": "Watcher"
    })
}

#[test]
fn printout_parses_typechecks_and_yields_true() {
    let e = ol_stdlib::parse_expr("printout(a, b)").expect("parse printout");
    assert!(matches!(&e, ol_ir::Expr::Printout { args } if args.len() == 2), "{e:?}");
    assert_eq!(ol_lustre_emit::format_expr(&e), "printout(a, b)");
    assert_eq!(e, ol_stdlib::parse_expr("printout(a, b)").unwrap());
    // The proof view sees only the value.
    assert_eq!(ol_lustre_emit::format_expr_lustre(&e), "true");
    // Inputs are declared variables, 1..=12 of them.
    assert!(ol_stdlib::parse_expr("printout()").is_err());
    assert!(ol_stdlib::parse_expr("printout(a + 1)").is_err());

    let p: ol_ir::Project = serde_json::from_value(watcher_model()).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);

    // An undeclared or non-scalar input is E0149.
    let mut bad = watcher_model();
    bad["packages"][0]["nodes"][0]["equations"][1]["rhs"] =
        serde_json::to_value(ol_stdlib::parse_expr("printout(ghost)").unwrap()).unwrap();
    let p: ol_ir::Project = serde_json::from_value(bad).unwrap();
    let r = ol_typecheck::check_project(&p);
    assert!(r.diagnostics.iter().any(|d| d.code == "E0149"), "{:?}", r.diagnostics);
}

#[test]
fn printout_simulates_clean_traces_and_debug_only_c() {
    let tmp = make_tempdir("printout");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&watcher_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();

    // The stdout CSV trace is untouched; terminal_out is true every cycle.
    let mut sim = ol_sim::Sim::new(&project, "Watcher").unwrap();
    let trace = sim.run_csv("speed,armed\n3,true\n5,false\n").unwrap();
    let lines: Vec<String> = trace.to_csv().trim().lines().map(str::to_owned).collect();
    assert_eq!(lines[0], "cycle,twice,terminal_out");
    assert_eq!(lines[1], "0,6,true");
    assert_eq!(lines[2], "1,10,true");

    // Production C has NO printing outside the OL_DEBUG guard.
    let emitted = ol_clite_emit::emit_project(&project);
    assert!(emitted.source.contains("#ifdef OL_DEBUG"), "{}", emitted.source);
    assert!(
        emitted.source.contains("terminal_out | speed=%lld armed=%s"),
        "debug fprintf missing:\n{}",
        emitted.source
    );
    let mut in_debug = false;
    for line in emitted.source.lines() {
        if line.contains("#ifdef OL_DEBUG") { in_debug = true; }
        if line.contains("#endif") { in_debug = false; }
        if !in_debug {
            assert!(
                !line.contains("printf"),
                "printing outside OL_DEBUG: {line}"
            );
        }
    }

    // Dual-backend: traces agree cell by cell (stderr chatter is invisible).
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("po.csv"), "speed,armed\n3,true\n5,false\n-9,true\n").unwrap();
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
    let (ok, out) = run(&["test", "run", model.to_str().unwrap(),
        "--scenarios", scen.to_str().unwrap(), "--backend", "both"]);
    assert!(ok, "run: {out}");
    assert!(out.contains("[PASS] po (ir)"), "{out}");
    assert!(out.contains("[PASS] po (c )"), "printout C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

// --- Studio: dropping the block creates the special terminal_out output -------

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
    let tmp = make_tempdir("po_srv");
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
                    if let Ok(n) = p.parse::<u16>() { port = Some(n); break; }
                }
            }
            Err(_) => sleep(Duration::from_millis(20)),
        }
    }
    let port = port.expect("server should print bound port");
    for _ in 0..50 {
        if request(port, "GET", "/api/health", "").is_some() { break; }
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
fn printout_drop_creates_the_terminal_out_output() {
    let g = start_server();
    let port = g.port;
    let post_ok = |path: &str, body: &str| {
        let (s, b) = request(port, "POST", path, body).expect(path);
        assert_eq!(s, 200, "{path}: {b}");
    };
    post_ok("/api/edit/add_node", r#"{"name":"Dash","kind":"operator"}"#);
    post_ok("/api/edit/add_operation",
        r#"{"node":"Dash","op":"printout","inputs":2,"x":40.0,"y":40.0}"#);
    let (s, b) = request(port, "GET", "/api/diagram?node=Dash", "").unwrap();
    assert_eq!(s, 200, "{b}");
    let d: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(d["equations"][0]["body"], "printout(p0_1, p0_2)");
    assert_eq!(d["equations"][0]["symbol"]["text"], "PRINT");
    // The special output: a bool local named terminal_out, not user-defined.
    let l = d["locals"].as_array().unwrap().iter()
        .find(|l| l["name"] == "terminal_out").expect("terminal_out local");
    assert_eq!(l["type"]["kind"], "Bool");
    // The pin count obeys the same 12 ceiling as the variadic operations.
    let (s, _) = request(port, "POST", "/api/edit/add_operation",
        r#"{"node":"Dash","op":"printout","inputs":13,"x":0,"y":0}"#).unwrap();
    assert_eq!(s, 400, "13 printout pins must be rejected");
}
