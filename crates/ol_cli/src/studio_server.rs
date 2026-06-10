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
    /// Merge the library embedded in this binary when no on-disk
    /// `--with-stdlib` directory was given (the deployed-app default).
    pub use_embedded: bool,
    /// Directory of test scenarios (*.csv + *.golden.csv). Defaults to a
    /// `scenarios` directory next to the model file.
    pub scenarios: PathBuf,
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
        ("GET", "/api/tests") => match tests_list(ctx) {
            Ok(b) => (200, "application/json", b.into_bytes()),
            Err(e) => (500, "application/json", json_error(&e).into_bytes()),
        },
        ("POST", "/api/tests/run") => match tests_run(ctx) {
            Ok(b) => (200, "application/json", b.into_bytes()),
            Err(e) => (400, "application/json", json_error(&e).into_bytes()),
        },
        ("POST", "/api/tests/record") => match tests_record(ctx) {
            Ok(b) => (200, "application/json", b.into_bytes()),
            Err(e) => (400, "application/json", json_error(&e).into_bytes()),
        },
        ("POST", "/api/prove") => match prove_run(ctx, &parse_query(query)) {
            Ok(b) => (200, "application/json", b.into_bytes()),
            Err(e) => (400, "application/json", json_error(&e).into_bytes()),
        },
        ("GET", "/api/fsm") => match fsm_get(ctx, &parse_query(query)) {
            Ok(b) => (200, "application/json", b.into_bytes()),
            Err(e) => (400, "application/json", json_error(&e).into_bytes()),
        },
        ("POST", "/api/edit/set_layout") => {
            apply_edit_response(ctx, body, edit_set_layout)
        }
        ("POST", "/api/edit/add_state_machine") => {
            apply_edit_response(ctx, body, edit_add_state_machine)
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
    crate::load_for_studio(&ctx.model, ctx.with_stdlib.as_deref(), ctx.use_embedded)
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

/// The Build/C-Lite views generate the SCADE way: the selected root (the
/// project's `main`) and everything it transitively uses — never the whole
/// merged project (which would drag every stdlib block into the generated C).
fn sliced_for_main(ctx: &ServerCtx) -> Result<ol_ir::Project, String> {
    let project = load(ctx)?;
    match project.main.clone() {
        Some(root) => project.slice_for_root(&root),
        None => Ok(project),
    }
}

fn build_clite(ctx: &ServerCtx) -> Result<(String, String), String> {
    let project = sliced_for_main(ctx)?;
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
        "positions": node.diagram.positions,
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
    let is_machine = project
        .packages
        .iter()
        .any(|p| p.state_machines.iter().any(|m| m.name == main));
    if project.find_node(main).is_none() && !is_machine {
        return Err(format!(
            "`{main}` is neither a node nor a state machine in the model file"
        ));
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

// --- Test scenarios (SCADE Test analog): list / run / record ---

fn main_node(project: &ol_ir::Project) -> Result<String, String> {
    project
        .main
        .clone()
        .ok_or_else(|| "project has no `main` node".to_string())
}

fn tests_list(ctx: &ServerCtx) -> Result<String, String> {
    let scenarios = crate::scenario::list_scenarios(&ctx.scenarios);
    let value = serde_json::json!({
        "schema_version": 1,
        "scenarios_dir": ctx.scenarios.display().to_string(),
        "scenarios": scenarios.iter().map(|s| serde_json::json!({
            "name": s.name,
            "has_golden": s.has_golden,
        })).collect::<Vec<_>>(),
        "cc_available": crate::scenario::cc_available(),
    });
    Ok(serde_json::to_string(&value).unwrap_or_default())
}

fn tests_run(ctx: &ServerCtx) -> Result<String, String> {
    let project = load(ctx)?;
    let node = main_node(&project)?;
    let outcome = crate::scenario::run_scenarios(
        &project,
        &ctx.scenarios,
        &node,
        &[crate::scenario::Backend::Ir, crate::scenario::Backend::C],
    );
    let value = serde_json::json!({
        "schema_version": 1,
        "all_green": crate::scenario::all_green(&outcome.results),
        "results": outcome.results,
        "coverage": outcome.coverage,
    });
    Ok(serde_json::to_string(&value).unwrap_or_default())
}

fn tests_record(ctx: &ServerCtx) -> Result<String, String> {
    let project = load(ctx)?;
    let node = main_node(&project)?;
    let recorded = crate::scenario::record_goldens(&project, &ctx.scenarios, &node)?;
    let value = serde_json::json!({
        "schema_version": 1,
        "recorded": recorded.iter().map(|(name, path)| serde_json::json!({
            "name": name,
            "golden": path.display().to_string(),
        })).collect::<Vec<_>>(),
    });
    Ok(serde_json::to_string(&value).unwrap_or_default())
}

// --- Verify (Kind 2), free-form layout, and FSM authoring ------------------

/// Run Kind 2 on the project's generated Lustre + contracts and return the
/// property results as JSON, with counterexamples rendered as per-cycle
/// waveform tables when they parse. When the `kind2` binary is not on PATH
/// the response still succeeds, with `kind2_found: false` and an install
/// hint — the Verify tab shows that instead of an opaque error.
fn prove_run(
    ctx: &ServerCtx,
    query: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let project = sliced_for_main(ctx)?;
    let main = project.main.clone();
    let lus = ol_lustre_emit::emit_project(&project);
    let con = ol_cocospec_emit::emit_project(&project, ol_cocospec_emit::Target::Modern);
    let combined = format!("{lus}\n{con}");

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let work = std::env::temp_dir().join(format!("openlustre_prove_{stamp}"));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let lus_path = work.join("model_with_contracts.lus");
    std::fs::write(&lus_path, &combined).map_err(|e| e.to_string())?;

    let timeout = query.get("timeout").and_then(|t| t.parse::<u32>().ok());
    let opts = ol_kind2::Kind2Options {
        main_node: main,
        timeout_seconds: timeout,
        ..Default::default()
    };
    let result = ol_kind2::run_kind2(&lus_path, &opts).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&work);

    let kind2_found = !(result.exit_code == -1 && result.stderr.contains("could not launch"));
    let properties: Vec<serde_json::Value> = result
        .properties
        .iter()
        .map(|p| {
            let waveform = p
                .counterexample
                .as_ref()
                .and_then(ol_kind2::render_counterexample_waveform);
            serde_json::json!({
                "name": p.name,
                "status": p.status,
                "waveform": waveform,
            })
        })
        .collect();
    let stdout_tail: String = result
        .stdout
        .lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let value = serde_json::json!({
        "schema_version": 1,
        "kind2_found": kind2_found,
        "invocation": result.invocation,
        "exit_code": result.exit_code,
        "properties": properties,
        "stdout_tail": stdout_tail,
        "hint": if kind2_found { serde_json::Value::Null } else {
            serde_json::Value::String(
                "kind2 not found on PATH — install it from https://kind2-mc.github.io/kind2/ to prove contracts".into()
            )
        },
    });
    Ok(serde_json::to_string(&value).unwrap_or_default())
}

/// Persist free-form canvas positions for a node:
/// `{ "node": "...", "positions": { "id": {"x": .., "y": ..}, ... } }`.
fn edit_set_layout(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let node_name = req
        .get("node")
        .and_then(|v| v.as_str())
        .ok_or("missing string field `node`")?
        .to_string();
    let positions = req
        .get("positions")
        .and_then(|v| v.as_object())
        .ok_or("missing object field `positions`")?;
    let mut map = std::collections::BTreeMap::new();
    for (id, pos) in positions {
        let x = pos.get("x").and_then(|v| v.as_f64()).ok_or("position missing x")?;
        let y = pos.get("y").and_then(|v| v.as_f64()).ok_or("position missing y")?;
        map.insert(id.clone(), ol_ir::NodePos { x, y });
    }
    for pkg in &mut project.packages {
        if let Some(n) = pkg.nodes.iter_mut().find(|n| n.name == node_name) {
            n.diagram.positions = map;
            return Ok(());
        }
    }
    Err(format!("node `{node_name}` not found in the model file"))
}

/// List the model file's state machines, or return one machine's full
/// definition (`?name=X`) with expressions rendered as text for the editor.
fn fsm_get(
    ctx: &ServerCtx,
    query: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let project = load_raw(ctx)?;
    match query.get("name") {
        None => {
            let machines: Vec<&str> = project
                .packages
                .iter()
                .flat_map(|p| p.state_machines.iter().map(|m| m.name.as_str()))
                .collect();
            Ok(serde_json::json!({ "schema_version": 1, "machines": machines }).to_string())
        }
        Some(name) => {
            for pkg in &project.packages {
                for m in &pkg.state_machines {
                    if &m.name == name {
                        let states: Vec<serde_json::Value> = m
                            .states
                            .iter()
                            .map(|st| {
                                serde_json::json!({
                                    "name": st.name,
                                    "equations": st.equations.iter().map(|eq| serde_json::json!({
                                        "lhs": eq.lhs.join(", "),
                                        "body": ol_lustre_emit::format_expr(&eq.rhs),
                                    })).collect::<Vec<_>>(),
                                    "transitions": st.transitions.iter().map(|t| serde_json::json!({
                                        "guard": ol_lustre_emit::format_expr(&t.guard),
                                        "target": t.target,
                                    })).collect::<Vec<_>>(),
                                })
                            })
                            .collect();
                        let value = serde_json::json!({
                            "schema_version": 1,
                            "name": m.name,
                            "inputs": m.inputs.iter().map(|p| serde_json::json!({
                                "name": p.name, "type": p.ty,
                            })).collect::<Vec<_>>(),
                            "outputs": m.outputs.iter().map(|p| serde_json::json!({
                                "name": p.name, "type": p.ty,
                            })).collect::<Vec<_>>(),
                            "initial_state": m.initial_state,
                            "states": states,
                        });
                        return Ok(value.to_string());
                    }
                }
            }
            Err(format!("state machine `{name}` not found in the model file"))
        }
    }
}

/// Create a state machine from the structured editor payload. The machine is
/// validated by lowering it once before the file is saved, so malformed
/// machines (unknown initial state, unassigned outputs, ...) are rejected
/// with the lowering error and the file stays untouched.
fn edit_add_state_machine(
    project: &mut ol_ir::Project,
    req: &serde_json::Value,
) -> Result<(), String> {
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing string field `name`")?;
    if project.find_node(name).is_some()
        || project
            .packages
            .iter()
            .any(|p| p.state_machines.iter().any(|m| m.name == name))
    {
        return Err(format!("`{name}` already exists"));
    }

    let parse_ports = |key: &str| -> Result<Vec<ol_ir::Port>, String> {
        let mut out = Vec::new();
        for item in req.get(key).and_then(|v| v.as_array()).unwrap_or(&vec![]) {
            let pname = item
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or(format!("{key}: port missing name"))?;
            let tstr = item
                .get("type")
                .and_then(|v| v.as_str())
                .ok_or(format!("{key}: port missing type"))?;
            let ty = ol_stdlib::parse_type(tstr).map_err(|e| format!("{key} `{pname}`: {e}"))?;
            out.push(ol_ir::Port { name: pname.to_string(), ty });
        }
        Ok(out)
    };
    let inputs = parse_ports("inputs")?;
    let outputs = parse_ports("outputs")?;
    let initial_state = req
        .get("initial_state")
        .and_then(|v| v.as_str())
        .ok_or("missing string field `initial_state`")?
        .to_string();

    let mut states = Vec::new();
    for st in req.get("states").and_then(|v| v.as_array()).unwrap_or(&vec![]) {
        let sname = st
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or("state missing name")?;
        let mut equations = Vec::new();
        for eq in st.get("equations").and_then(|v| v.as_array()).unwrap_or(&vec![]) {
            let lhs_str = eq.get("lhs").and_then(|v| v.as_str()).ok_or("equation missing lhs")?;
            let body = eq.get("body").and_then(|v| v.as_str()).ok_or("equation missing body")?;
            let lhs: Vec<String> = lhs_str
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect();
            let rhs = ol_stdlib::parse_expr(body)
                .map_err(|e| format!("state `{sname}` equation: {e}"))?;
            equations.push(ol_ir::Equation { lhs, rhs });
        }
        let mut transitions = Vec::new();
        for t in st.get("transitions").and_then(|v| v.as_array()).unwrap_or(&vec![]) {
            let guard_str = t.get("guard").and_then(|v| v.as_str()).ok_or("transition missing guard")?;
            let target = t.get("target").and_then(|v| v.as_str()).ok_or("transition missing target")?;
            let guard = ol_stdlib::parse_expr(guard_str)
                .map_err(|e| format!("state `{sname}` transition guard: {e}"))?;
            transitions.push(ol_ir::Transition { guard, target: target.to_string() });
        }
        states.push(ol_ir::StateDef {
            name: sname.to_string(),
            equations,
            transitions,
        });
    }

    let machine = ol_ir::StateMachineDef {
        name: name.to_string(),
        inputs,
        outputs,
        locals: vec![],
        initial_state,
        states,
        contract: None,
    };
    // Validate before saving: lowering reports unknown initial state,
    // unknown transition targets, and outputs unassigned in a state.
    ol_ir::lower_state_machine(&machine).map_err(|e| e.to_string())?;

    if project.packages.is_empty() {
        project.packages.push(ol_ir::Package {
            name: "user".into(),
            ..Default::default()
        });
    }
    project.packages[0].state_machines.push(machine);
    Ok(())
}
