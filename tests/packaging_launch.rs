//! Deployment behavior: the binary must be self-contained (embedded
//! standard library identical to the on-disk one) and `studio launch` must
//! provide the installed-shortcut experience — create the welcome project on
//! first run and serve it with the embedded palette, no flags, no checkout.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

#[test]
fn embedded_library_is_identical_to_the_on_disk_library() {
    let embedded = ol_stdlib::load_embedded().expect("embedded loads");
    let on_disk = ol_stdlib::load_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../libraries"),
    )
    .expect("on-disk loads");

    let e_names: BTreeSet<&str> = embedded.nodes().map(|n| n.name.as_str()).collect();
    let d_names: BTreeSet<&str> = on_disk.nodes().map(|n| n.name.as_str()).collect();
    assert_eq!(e_names, d_names, "embedded palette must match libraries/");
    assert!(e_names.len() >= 41, "expected the full palette, got {}", e_names.len());

    // The embedded library must pass the same checks the on-disk one does.
    let errors: Vec<String> = embedded
        .check()
        .into_iter()
        .filter(|d| matches!(d.severity, ol_ir::Severity::Error))
        .map(|d| d.render())
        .collect();
    assert!(errors.is_empty(), "embedded library has errors:\n{}", errors.join("\n"));
}

struct LaunchGuard {
    child: Child,
    port: u16,
    home: PathBuf,
    /// Set by a test that reuses the home in a later launch: Drop kills the
    /// server but leaves the directory (the next guard cleans it up).
    no_remove: bool,
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !self.no_remove {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}

fn start_launch() -> LaunchGuard {
    use std::time::{SystemTime, UNIX_EPOCH};
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let home =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("__trace_tmp_home_{stamp}"));
    start_launch_in(home)
}

/// Launch on an existing (fake) home — the recents list is per-user, so a
/// fresh launch over the SAME home is the restart-under-test.
fn start_launch_in(home: PathBuf) -> LaunchGuard {
    std::fs::create_dir_all(&home).unwrap();

    // Fake HOME so the welcome project lands in our temp dir — but keep
    // cargo/rustup resolving against the real home, otherwise the `cargo`
    // shim cannot find its toolchain.
    let real_home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .expect("real home");
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| real_home.join(".cargo"));
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| real_home.join(".rustup"));

    let mut child = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "studio", "launch", "--no-open"])
        .args(["--port", "0"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CARGO_HOME", cargo_home)
        .env("RUSTUP_HOME", rustup_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("studio launch");

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
    let port = port.expect("launch should print the served URL");
    for _ in 0..50 {
        if http_get(port, "/api/health").is_some() {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    LaunchGuard { child, port, home, no_remove: false }
}

fn http_get(port: u16, path: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    stream.shutdown(Shutdown::Write).ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let (head, payload) = raw.split_once("\r\n\r\n")?;
    let status: u16 = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, payload.to_string()))
}

fn http_post(port: u16, path: &str, body: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
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

#[test]
fn launch_opens_the_welcome_page_then_a_new_workspace_serves_the_palette() {
    let g = start_launch();

    // First-run experience: the welcome page, and NOTHING auto-created —
    // the engineer picks what to open (VS Code / SCADE style).
    let welcome = g.home.join("OpenLustre/welcome.wksc");
    assert!(!welcome.exists(), "launch must not auto-create a project");
    let (s, body) = http_get(g.port, "/api/inspect").expect("inspect");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["welcome"]["base"], g.home.join("OpenLustre").display().to_string());
    assert!(!v["welcome"]["samples"].as_array().unwrap().is_empty(), "shipped samples listed");
    assert!(!v["welcome"]["version"].as_str().unwrap_or("").is_empty(), "version reported");

    // Creating a workspace from the welcome page switches to it and serves
    // the embedded 41-block palette — no --with-stdlib flag, no libraries/
    // checkout needed. The starter operator seeds the new project.
    let dir = g.home.join("OpenLustre/First");
    let (s, body) = http_post(
        g.port,
        "/api/workspace/new",
        &serde_json::json!({ "path": dir }).to_string(),
    )
    .expect("workspace/new");
    assert_eq!(s, 200, "new workspace: {body}");
    assert!(dir.join("First.wksc").exists() || dir.join("project.json").exists(), "workspace seeded");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["welcome"].is_null(), "a workspace is open now — no more welcome");
    assert_eq!(v["project"]["main"], "Heartbeat");
    let names: Vec<&str> = v["project"]["packages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["nodes"].as_array().unwrap())
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert!(names.contains(&"Heartbeat"), "starter node missing: {names:?}");
    assert!(names.contains(&"RisingEdge"), "embedded palette missing: {names:?}");
    assert!(names.contains(&"SRFlipFlop"), "embedded FSM block missing");
    assert_eq!(v["summary"]["errors"], 0, "starter project must be clean");

    // A reload keeps the workspace (the welcome state is server-side).
    let (s, body) = http_get(g.port, "/api/inspect").expect("inspect again");
    assert_eq!(s, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["project"]["main"], "Heartbeat");
}

#[test]
fn the_welcome_page_lists_the_last_four_workspaces_the_user_opened() {
    // Open (and create) several workspaces in one session, then start a
    // FRESH launch on the same home: the welcome page offers the latest
    // four, most recent first, and nothing from anyone else's folders.
    let mut g = start_launch();
    let mut opened = Vec::new();
    for name in ["One", "Two", "Three", "Four", "Five"] {
        let dir = g.home.join("OpenLustre").join(name);
        let (s, body) = http_post(
            g.port,
            "/api/workspace/new",
            &serde_json::json!({ "path": dir }).to_string(),
        )
        .expect("workspace/new");
        assert_eq!(s, 200, "{body}");
        opened.push(dir.join(format!("{name}.wksc")).display().to_string());
    }
    let home = g.home.clone();
    g.no_remove = true;
    drop(g); // ends that server; a fresh launch starts on the same home

    let g2 = start_launch_in(home);
    let (s, body) = http_get(g2.port, "/api/inspect").expect("inspect");
    assert_eq!(s, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["welcome"].is_object(), "fresh launch is on the welcome page");
    let recent: Vec<&str> = v["recent"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["path"].as_str())
        .collect();
    assert_eq!(
        recent,
        ["Five", "Four", "Three", "Two"]
            .iter()
            .map(|n| g2.home.join("OpenLustre").join(n).join(format!("{n}.wksc")).display().to_string())
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        "the latest four, most recent first (the fifth-oldest drops off)"
    );
}
