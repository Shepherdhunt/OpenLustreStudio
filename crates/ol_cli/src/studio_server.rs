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
    // Split path?query — every endpoint just sees the path; query parameters
    // are routed to the handlers that ask for them.
    let (raw_path, query) = match path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path, ""),
    };
    match (method, raw_path) {
        ("GET", "/") | ("GET", "/index.html") => {
            (200, "text/html; charset=utf-8", STUDIO_UI_HTML.as_bytes().to_vec())
        }
        ("GET", "/api/health") => (200, "text/plain; charset=utf-8", b"ok".to_vec()),
        ("GET", "/api/inspect") => match build_inspect(ctx) {
            Ok(body) => (200, "application/json", body.into_bytes()),
            Err(e) => (500, "application/json", json_error(&e).into_bytes()),
        },
        ("GET", "/api/diagram") => match build_diagram(ctx, &parse_query(query)) {
            Ok(body) => (200, "application/json", body.into_bytes()),
            Err(e) => (400, "application/json", json_error(&e).into_bytes()),
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
            let full = parse_query(query).get("full").map(|v| v == "1").unwrap_or(false);
            match run_sim(ctx, csv, full) {
                Ok(trace) => (200, "text/csv; charset=utf-8", trace.into_bytes()),
                Err(e) => (400, "text/plain", e.into_bytes()),
            }
        }
        ("POST", "/api/edit/add_port") => {
            apply_edit_response(ctx, body, edit_add_port)
        }
        ("POST", "/api/edit/add_local") => {
            apply_edit_response(ctx, body, edit_add_local)
        }
        ("POST", "/api/edit/add_equation") => {
            apply_edit_response(ctx, body, edit_add_equation)
        }
        ("POST", "/api/edit/set_main") => {
            apply_edit_response(ctx, body, edit_set_main)
        }
        ("POST", "/api/edit/add_node") => {
            apply_edit_response(ctx, body, edit_add_node)
        }
        _ => (404, "text/plain", b"not found".to_vec()),
    }
}

fn parse_query(q: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for kv in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        out.insert(url_decode(k), url_decode(v));
    }
    out
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.bytes();
    while let Some(b) = it.next() {
        match b {
            b'+' => out.push(' '),
            b'%' => {
                let h1 = it.next();
                let h2 = it.next();
                match (h1.and_then(hex), h2.and_then(hex)) {
                    (Some(a), Some(c)) => out.push(((a << 4) | c) as char),
                    _ => {
                        out.push('%');
                        if let Some(c) = h1 { out.push(c as char); }
                        if let Some(c) = h2 { out.push(c as char); }
                    }
                }
            }
            other => out.push(other as char),
        }
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
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

fn run_sim(ctx: &ServerCtx, csv: &str, full: bool) -> Result<String, String> {
    let project = load(ctx)?;
    let entry = project
        .main
        .clone()
        .ok_or_else(|| "project has no `main` node".to_string())?;
    let mut sim = ol_sim::Sim::new(&project, &entry).map_err(|e| format!("{e}"))?;
    let trace = if full {
        sim.run_csv_full(csv).map_err(|e| format!("{e}"))?
    } else {
        sim.run_csv(csv).map_err(|e| format!("{e}"))?
    };
    Ok(trace.to_csv())
}

// --- Diagram: a renderable dataflow view of one node ---

/// Build a diagram JSON for the requested node (or `main`). Inputs, locals,
/// equations, and outputs become boxes; wires are derived from each
/// equation's free variables (reads) and its lhs (writes). The front end
/// lays these out in columns and draws SVG lines — no layout engine needed.
fn build_diagram(
    ctx: &ServerCtx,
    query: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let project = load(ctx)?;
    let node_name = query
        .get("node")
        .cloned()
        .or_else(|| project.main.clone())
        .ok_or_else(|| "no node specified and project has no main".to_string())?;
    let node = project
        .find_node(&node_name)
        .ok_or_else(|| format!("node `{node_name}` not found"))?;

    let known: std::collections::HashSet<&str> = node
        .inputs
        .iter()
        .map(|p| p.name.as_str())
        .chain(node.outputs.iter().map(|p| p.name.as_str()))
        .chain(node.locals.iter().map(|l| l.name.as_str()))
        .collect();

    let mut equations = Vec::new();
    let mut wires = Vec::new();
    for (i, eq) in node.equations.iter().enumerate() {
        let eq_id = format!("eq{i}");
        let reads: Vec<String> = eq
            .rhs
            .free_vars()
            .into_iter()
            .filter(|v| known.contains(v.as_str()))
            .collect();
        let mut calls: Vec<String> = Vec::new();
        eq.rhs.visit(|e| {
            if let ol_ir::Expr::Call { node: callee, .. } = e {
                if !calls.contains(callee) {
                    calls.push(callee.clone());
                }
            }
        });
        for r in &reads {
            wires.push(serde_json::json!({ "from": r, "to": eq_id }));
        }
        for l in &eq.lhs {
            wires.push(serde_json::json!({ "from": eq_id, "to": l }));
        }
        equations.push(serde_json::json!({
            "id": eq_id,
            "lhs": eq.lhs,
            "text": format!("{} = {}", eq.lhs.join(", "), ol_lustre_emit::format_expr(&eq.rhs)),
            "reads": reads,
            "calls": calls,
        }));
    }

    let value = serde_json::json!({
        "schema_version": 1,
        "node": node.name,
        "kind": format!("{:?}", node.kind),
        "inputs": node.inputs.iter().map(|p| serde_json::json!({
            "name": p.name, "type": p.ty,
        })).collect::<Vec<_>>(),
        "outputs": node.outputs.iter().map(|p| serde_json::json!({
            "name": p.name, "type": p.ty,
        })).collect::<Vec<_>>(),
        "locals": node.locals.iter().map(|l| serde_json::json!({
            "name": l.name, "type": l.ty,
        })).collect::<Vec<_>>(),
        "equations": equations,
        "wires": wires,
    });
    Ok(serde_json::to_string(&value).unwrap_or_default())
}

// --- Editing: parse, mutate, save back, return refreshed inspect ---

/// Parse the model file directly (single file, includes left untouched) so a
/// save-back writes only what the user authored. The full pipeline —
/// includes, stdlib merge, state-machine lowering — still runs on the *read*
/// path (`load`), so diagnostics reflect the complete picture.
fn load_raw(ctx: &ServerCtx) -> Result<ol_ir::Project, String> {
    let data = std::fs::read_to_string(&ctx.model)
        .map_err(|e| format!("reading {}: {e}", ctx.model.display()))?;
    match ctx
        .model
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => serde_json::from_str(&data).map_err(|e| format!("JSON: {e}")),
        Some("ols") | Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&data).map_err(|e| format!("YAML: {e}"))
        }
        other => Err(format!("unsupported model extension: {other:?}")),
    }
}

fn save_raw(ctx: &ServerCtx, project: &ol_ir::Project) -> Result<(), String> {
    let text = match ctx
        .model
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => serde_json::to_string_pretty(project).map_err(|e| e.to_string())?,
        _ => serde_yaml::to_string(project).map_err(|e| e.to_string())?,
    };
    std::fs::write(&ctx.model, text).map_err(|e| format!("writing {}: {e}", ctx.model.display()))
}

type EditFn = fn(&mut ol_ir::Project, &serde_json::Value) -> Result<(), String>;

fn apply_edit_response(ctx: &ServerCtx, body: &[u8], f: EditFn) -> (u16, &'static str, Vec<u8>) {
    let req: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, "application/json", json_error(&format!("bad JSON: {e}")).into_bytes()),
    };
    let mut project = match load_raw(ctx) {
        Ok(p) => p,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    if let Err(e) = f(&mut project, &req) {
        return (400, "application/json", json_error(&e).into_bytes());
    }
    if let Err(e) = save_raw(ctx, &project) {
        return (500, "application/json", json_error(&e).into_bytes());
    }
    match build_inspect(ctx) {
        Ok(b) => (200, "application/json", b.into_bytes()),
        Err(e) => (500, "application/json", json_error(&e).into_bytes()),
    }
}

fn req_str<'a>(req: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    req.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing string field `{key}`"))
}

fn find_node_mut<'a>(
    project: &'a mut ol_ir::Project,
    name: &str,
) -> Result<&'a mut ol_ir::NodeDef, String> {
    for pkg in &mut project.packages {
        if let Some(n) = pkg.nodes.iter_mut().find(|n| n.name == name) {
            return Ok(n);
        }
    }
    Err(format!(
        "node `{name}` not found in {} (nodes in included files or the stdlib cannot be edited here)",
        "the model file"
    ))
}

fn edit_add_node(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let name = req_str(req, "name")?;
    if !is_identifier(name) {
        return Err(format!("`{name}` is not a valid identifier"));
    }
    if project.find_node(name).is_some() {
        return Err(format!("node `{name}` already exists"));
    }
    let kind = match req.get("kind").and_then(|v| v.as_str()).unwrap_or("operator") {
        "function" => ol_ir::NodeKind::Function,
        "operator" => ol_ir::NodeKind::Operator,
        other => return Err(format!("unknown kind `{other}` (function|operator)")),
    };
    let node = ol_ir::NodeDef {
        name: name.to_string(),
        kind,
        inputs: vec![],
        outputs: vec![],
        locals: vec![],
        equations: vec![],
        contract: None,
        diagram: Default::default(),
    };
    if project.packages.is_empty() {
        project.packages.push(ol_ir::Package {
            name: "user".into(),
            ..Default::default()
        });
    }
    let pkg_name = req.get("package").and_then(|v| v.as_str());
    let pkg = match pkg_name {
        Some(p) => project
            .packages
            .iter_mut()
            .find(|x| x.name == p)
            .ok_or_else(|| format!("package `{p}` not found"))?,
        None => &mut project.packages[0],
    };
    pkg.nodes.push(node);
    Ok(())
}

fn edit_add_port(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let port_name = req_str(req, "name")?.to_string();
    let side = req_str(req, "side")?;
    let ty_str = req_str(req, "type")?;
    if !is_identifier(&port_name) {
        return Err(format!("`{port_name}` is not a valid identifier"));
    }
    let ty = ol_stdlib::parse_type(ty_str).map_err(|e| format!("type `{ty_str}`: {e}"))?;
    let node = find_node_mut(project, &node_name)?;
    let exists = node.inputs.iter().any(|p| p.name == port_name)
        || node.outputs.iter().any(|p| p.name == port_name)
        || node.locals.iter().any(|l| l.name == port_name);
    if exists {
        return Err(format!("`{port_name}` already exists on `{node_name}`"));
    }
    let port = ol_ir::Port { name: port_name, ty };
    match side {
        "input" => node.inputs.push(port),
        "output" => node.outputs.push(port),
        other => return Err(format!("side must be input|output, got `{other}`")),
    }
    Ok(())
}

fn edit_add_local(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let local_name = req_str(req, "name")?.to_string();
    let ty_str = req_str(req, "type")?;
    if !is_identifier(&local_name) {
        return Err(format!("`{local_name}` is not a valid identifier"));
    }
    let ty = ol_stdlib::parse_type(ty_str).map_err(|e| format!("type `{ty_str}`: {e}"))?;
    let node = find_node_mut(project, &node_name)?;
    let exists = node.inputs.iter().any(|p| p.name == local_name)
        || node.outputs.iter().any(|p| p.name == local_name)
        || node.locals.iter().any(|l| l.name == local_name);
    if exists {
        return Err(format!("`{local_name}` already exists on `{node_name}`"));
    }
    node.locals.push(ol_ir::Local { name: local_name, ty });
    Ok(())
}

fn edit_add_equation(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let lhs_str = req_str(req, "lhs")?;
    let body = req_str(req, "body")?;
    let lhs: Vec<String> = lhs_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if lhs.is_empty() {
        return Err("lhs must name at least one variable".into());
    }
    for l in &lhs {
        if !is_identifier(l) {
            return Err(format!("`{l}` is not a valid identifier"));
        }
    }
    let rhs = ol_stdlib::parse_expr(body).map_err(|e| format!("body: {e}"))?;
    let node = find_node_mut(project, &node_name)?;
    node.equations.push(ol_ir::Equation { lhs, rhs });
    Ok(())
}

fn edit_set_main(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let main = req_str(req, "main")?;
    if project.find_node(main).is_none() {
        return Err(format!("node `{main}` not found in the model file"));
    }
    project.main = Some(main.to_string());
    Ok(())
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Convenience for the CLI dispatcher. Passing port `0` lets the OS pick an
/// unused port, which tests use to avoid collisions.
pub fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
}
