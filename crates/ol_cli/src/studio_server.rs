//! Phase 8: a tiny browser-driven Studio UI in pure Rust.
//!
//! `openlustre studio serve --model M.ols [--with-stdlib DIR] [--port N]`
//! starts a localhost HTTP server with a single-page UI for the
//! Project Explorer, diagnostics, generated Lustre / C-Lite views, and a
//! simulation panel. The page loads the JSON the existing
//! `studio inspect` command produces; the same load_with_stdlib pipeline
//! that every CLI command uses powers each request.
//!
//! There is no JavaScript build step. The single static HTML page is
//! embedded with `include_str!`, vanilla JS only, and the back end is just
//! `std::net::TcpListener` driving one thread per connection.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const STUDIO_UI_HTML: &str = include_str!("studio_ui.html");
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Server configuration captured at startup; the project is re-loaded on every
/// request so external edits to the model are reflected by a page refresh.
pub struct ServerCtx {
    pub model: PathBuf,
    pub with_stdlib: Option<PathBuf>,
}

/// Bind a listener on `addr`. The caller can pass port `0` to let the OS pick
/// one and read it back via `local_addr` — used by tests to avoid port
/// collisions.
pub fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let listener = TcpListener::bind(addr)?;
    Ok(listener)
}

/// Run the server forever, spawning a thread per accepted connection.
pub fn serve(listener: TcpListener, ctx: ServerCtx) -> std::io::Result<()> {
    let ctx = Arc::new(ctx);
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let ctx = Arc::clone(&ctx);
        thread::spawn(move || {
            let _ = handle_connection(stream, &ctx);
        });
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, ctx: &ServerCtx) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;

    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    // Read until we have the end of the HTTP headers, then enough body
    // bytes to satisfy Content-Length (if any).
    let headers_end;
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_double_crlf(&buf) {
            headers_end = pos;
            break;
        }
        if buf.len() > MAX_REQUEST_BYTES {
            return write_response(&mut stream, 413, "text/plain", b"request too large");
        }
    }

    let header_str = std::str::from_utf8(&buf[..headers_end]).unwrap_or("");
    let (method, path) = parse_request_line(header_str);
    let content_length = parse_content_length(header_str).unwrap_or(0).min(MAX_REQUEST_BYTES);

    let body_start = headers_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_REQUEST_BYTES {
            return write_response(&mut stream, 413, "text/plain", b"request too large");
        }
    }
    let body = if buf.len() >= body_start + content_length {
        &buf[body_start..body_start + content_length]
    } else {
        &[][..]
    };

    let (status, ctype, payload) = route(&method, &path, body, ctx);
    write_response(&mut stream, status, ctype, &payload)
}

fn route(method: &str, path: &str, body: &[u8], ctx: &ServerCtx) -> (u16, &'static str, Vec<u8>) {
    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            (200, "text/html; charset=utf-8", STUDIO_UI_HTML.as_bytes().to_vec())
        }
        ("GET", "/api/health") => (200, "text/plain; charset=utf-8", b"ok".to_vec()),
        ("GET", "/api/inspect") => match build_inspect(ctx) {
            Ok(body) => (200, "application/json", body.into_bytes()),
            Err(e) => (500, "application/json", json_error(&e).into_bytes()),
        },
        ("GET", "/api/lustre") => match build_lustre(ctx) {
            Ok(body) => (200, "text/plain; charset=utf-8", body.into_bytes()),
            Err(e) => (500, "text/plain", e.into_bytes()),
        },
        ("GET", "/api/clite/header") => match build_clite(ctx) {
            Ok((h, _)) => (200, "text/plain; charset=utf-8", h.into_bytes()),
            Err(e) => (500, "text/plain", e.into_bytes()),
        },
        ("GET", "/api/clite/source") => match build_clite(ctx) {
            Ok((_, s)) => (200, "text/plain; charset=utf-8", s.into_bytes()),
            Err(e) => (500, "text/plain", e.into_bytes()),
        },
        ("GET", "/api/clite/driver") => match build_driver(ctx) {
            Ok(s) => (200, "text/plain; charset=utf-8", s.into_bytes()),
            Err(e) => (400, "text/plain", e.into_bytes()),
        },
        ("GET", "/api/clite/makefile") => match build_makefile(ctx) {
            Ok(s) => (200, "text/plain; charset=utf-8", s.into_bytes()),
            Err(e) => (400, "text/plain", e.into_bytes()),
        },
        ("POST", "/api/simulate") => {
            let csv = std::str::from_utf8(body).unwrap_or("");
            match run_sim(ctx, csv) {
                Ok(trace) => (200, "text/csv; charset=utf-8", trace.into_bytes()),
                Err(e) => (400, "text/plain", e.into_bytes()),
            }
        }
        _ => (404, "text/plain", b"not found".to_vec()),
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    ctype: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = status_text(status);
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        len = body.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

fn status_text(s: u16) -> &'static str {
    match s {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_request_line(headers: &str) -> (String, String) {
    let line = headers.lines().next().unwrap_or("");
    let mut it = line.split_whitespace();
    let method = it.next().unwrap_or("").to_string();
    let path = it.next().unwrap_or("/").to_string();
    (method, path)
}

fn parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                return Some(n);
            }
        }
    }
    None
}

fn json_error(msg: &str) -> String {
    format!(
        "{{\"schema_version\":1,\"error\":{}}}",
        serde_json::Value::String(msg.to_string())
    )
}

// --- back-end glue: re-load the project per request and call existing
// --- emit / inspect / simulate paths.

fn load(ctx: &ServerCtx) -> Result<ol_ir::Project, String> {
    crate::load_with_stdlib(&ctx.model, ctx.with_stdlib.as_deref())
        .map_err(|e| format!("{e:#}"))
}

fn build_inspect(ctx: &ServerCtx) -> Result<String, String> {
    let project = load(ctx)?;
    let typecheck = ol_typecheck::check_project(&project);
    let contract = ol_contract_check::check_project(&project);

    let mut diagnostics: Vec<serde_json::Value> = Vec::new();
    for d in &typecheck.diagnostics {
        diagnostics.push(crate::diag_to_json(d, "typecheck"));
    }
    for d in &contract.diagnostics {
        diagnostics.push(crate::diag_to_json(d, "contract"));
    }
    let packages: Vec<serde_json::Value> = project
        .packages
        .iter()
        .map(crate::package_to_json)
        .collect();
    let value = serde_json::json!({
        "schema_version": 1,
        "tool": "openlustre studio inspect",
        "project": {
            "name": project.name,
            "main": project.main,
            "package_count": project.packages.len(),
            "node_count": project.all_nodes().count(),
            "packages": packages,
        },
        "diagnostics": diagnostics,
        "summary": {
            "errors": diagnostics.iter()
                .filter(|d| d.get("severity").and_then(|s| s.as_str()) == Some("Error"))
                .count(),
            "warnings": diagnostics.iter()
                .filter(|d| d.get("severity").and_then(|s| s.as_str()) == Some("Warning"))
                .count(),
        }
    });
    Ok(serde_json::to_string(&value).unwrap_or_default())
}

fn build_lustre(ctx: &ServerCtx) -> Result<String, String> {
    let project = load(ctx)?;
    let lus = ol_lustre_emit::emit_project(&project);
    let con = ol_cocospec_emit::emit_project(&project, ol_cocospec_emit::Target::Modern);
    Ok(format!("{lus}\n{con}"))
}

fn build_clite(ctx: &ServerCtx) -> Result<(String, String), String> {
    let project = load(ctx)?;
    let bundle = ol_clite_emit::emit_project(&project);
    Ok((bundle.header, bundle.source))
}

fn build_driver(ctx: &ServerCtx) -> Result<String, String> {
    let project = load(ctx)?;
    let entry_name = project
        .main
        .clone()
        .ok_or_else(|| "project has no `main` operator; nothing to drive".to_string())?;
    let entry = project
        .find_node(&entry_name)
        .ok_or_else(|| format!("main operator `{entry_name}` not found"))?;
    Ok(ol_clite_emit::harness::emit_csv_driver(entry))
}

fn build_makefile(ctx: &ServerCtx) -> Result<String, String> {
    let project = load(ctx)?;
    let entry = project
        .main
        .clone()
        .ok_or_else(|| "project has no `main` operator".to_string())?;
    Ok(crate::makefile_for_entry(&entry))
}

fn run_sim(ctx: &ServerCtx, csv: &str) -> Result<String, String> {
    let project = load(ctx)?;
    let entry = project
        .main
        .clone()
        .ok_or_else(|| "project has no `main` node".to_string())?;
    let mut sim = ol_sim::Sim::new(&project, &entry).map_err(|e| format!("{e}"))?;
    let trace = sim.run_csv(csv).map_err(|e| format!("{e}"))?;
    Ok(trace.to_csv())
}

/// Convenience for the CLI dispatcher. Passing port `0` lets the OS pick an
/// unused port, which tests use to avoid collisions.
pub fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
}
