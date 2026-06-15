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
    /// The workspace's types file (`types.json` next to the model), when one
    /// exists. Named type definitions created in the GUI are saved here —
    /// the SCADE "types file" — and reach the model via its `includes`.
    pub types_file: Option<PathBuf>,
    /// Undo/redo journal: file snapshots taken before each successful edit.
    pub history: std::sync::Mutex<History>,
}

/// A snapshot is the full text of every editable file (model + types).
type Snapshot = Vec<(PathBuf, String)>;

#[derive(Default)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

const HISTORY_CAP: usize = 100;

fn take_snapshot(ctx: &ServerCtx) -> Snapshot {
    let mut files = vec![ctx.model.clone()];
    if let Some(t) = &ctx.types_file {
        files.push(t.clone());
    }
    files
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|text| (p, text)))
        .collect()
}

/// Record `before` as an undoable state. Called only after an edit actually
/// saved, so failed edits never pollute the journal. New edits invalidate
/// the redo branch, like every editor.
fn record_edit(ctx: &ServerCtx, before: Snapshot) {
    let mut h = ctx.history.lock().unwrap();
    h.undo.push(before);
    if h.undo.len() > HISTORY_CAP {
        h.undo.remove(0);
    }
    h.redo.clear();
}

fn history_response(ctx: &ServerCtx, undo: bool) -> (u16, &'static str, Vec<u8>) {
    let restored = {
        let mut h = ctx.history.lock().unwrap();
        let from = if undo { &mut h.undo } else { &mut h.redo };
        let Some(snap) = from.pop() else {
            return (
                400,
                "application/json",
                json_error(if undo { "nothing to undo" } else { "nothing to redo" }).into_bytes(),
            );
        };
        let current = take_snapshot(ctx);
        if undo {
            h.redo.push(current);
        } else {
            h.undo.push(current);
        }
        snap
    };
    for (path, text) in &restored {
        if let Err(e) = std::fs::write(path, text) {
            return (
                500,
                "application/json",
                json_error(&format!("restoring {}: {e}", path.display())).into_bytes(),
            );
        }
    }
    match build_inspect(ctx) {
        Ok(b) => (200, "application/json", b.into_bytes()),
        Err(e) => (500, "application/json", json_error(&e).into_bytes()),
    }
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
        ("POST", "/api/build") => build_model_response(ctx),
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
        ("GET", "/api/types") => match types_list(ctx) {
            Ok(b) => (200, "application/json", b.into_bytes()),
            Err(e) => (500, "application/json", json_error(&e).into_bytes()),
        },
        ("POST", "/api/edit/add_type") => add_type_response(ctx, body),
        ("POST", "/api/edit/remove_type") => remove_type_response(ctx, body),
        ("POST", "/api/edit/update_port") => {
            apply_edit_response(ctx, body, edit_update_port)
        }
        ("POST", "/api/edit/remove_port") => {
            apply_edit_response(ctx, body, edit_remove_port)
        }
        ("POST", "/api/edit/update_equation") => {
            apply_edit_response(ctx, body, edit_update_equation)
        }
        ("POST", "/api/edit/remove_equation") => {
            apply_edit_response(ctx, body, edit_remove_equation)
        }
        ("POST", "/api/edit/add_block_call") => add_block_call_response(ctx, body),
        ("GET", "/api/operations") => (
            200,
            "application/json",
            operations_catalog().to_string().into_bytes(),
        ),
        ("POST", "/api/edit/add_operation") => add_operation_response(ctx, body),
        ("POST", "/api/edit/set_operation_inputs") => {
            apply_edit_response(ctx, body, edit_set_operation_inputs)
        }
        ("POST", "/api/edit/add_probe") => apply_edit_response(ctx, body, edit_add_probe),
        ("POST", "/api/edit/remove_probe") => apply_edit_response(ctx, body, edit_remove_probe),
        ("POST", "/api/edit/undo") => history_response(ctx, true),
        ("POST", "/api/edit/redo") => history_response(ctx, false),
        ("POST", "/api/clite/compile") => clite_compile_response(ctx, body),
        ("POST", "/api/clite/run") => clite_run_response(ctx, body),
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
    let (undo_depth, redo_depth) = {
        let h = ctx.history.lock().unwrap();
        (h.undo.len(), h.redo.len())
    };
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
        "history": { "undo": undo_depth, "redo": redo_depth },
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

/// "Build the model": the SCADE model-checker step. Type- and contract-check
/// the project; on a clean check, emit the main operator's Lustre (the root
/// plus everything it uses) and write it to `<main>.lus` next to the model,
/// then hand it back so the GUI can show it. A model that does not build is
/// not simulated and has no generated `.lus` — failures are loud, never silent.
fn build_model_response(ctx: &ServerCtx) -> (u16, &'static str, Vec<u8>) {
    let project = match load(ctx) {
        Ok(p) => p,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    let report = ol_typecheck::check_project(&project);
    let contract = ol_contract_check::check_project(&project);
    let count = |sev: ol_ir::Severity| {
        report.diagnostics.iter().filter(|d| d.severity == sev).count()
            + contract.diagnostics.iter().filter(|d| d.severity == sev).count()
    };
    let errors = count(ol_ir::Severity::Error);
    let warnings = count(ol_ir::Severity::Warning);

    let fail = |msg: String, warnings: usize| {
        let v = serde_json::json!({ "ok": false, "errors": errors, "warnings": warnings, "message": msg });
        (200, "application/json", v.to_string().into_bytes())
    };
    if errors > 0 {
        return fail(
            format!("{errors} error(s) — fix them in Messages before building"),
            warnings,
        );
    }
    let Some(main) = project.main.clone() else {
        return fail("no main operator — set one with File ▸ Set Main Operator".into(), warnings);
    };
    let sliced = match project.slice_for_root(&main) {
        Ok(s) => s,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    let lus = ol_lustre_emit::emit_project(&sliced);
    let con = ol_cocospec_emit::emit_project(&sliced, ol_cocospec_emit::Target::Modern);
    let full = format!("{lus}\n{con}");
    let dir = ctx.model.parent().unwrap_or_else(|| std::path::Path::new("."));
    let path = dir.join(format!("{main}.lus"));
    if let Err(e) = std::fs::write(&path, &full) {
        return (
            500,
            "application/json",
            json_error(&format!("writing {}: {e}", path.display())).into_bytes(),
        );
    }
    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("model.lus");
    let value = serde_json::json!({
        "ok": true,
        "errors": 0,
        "warnings": warnings,
        "main": main,
        "path": path.display().to_string(),
        "lustre": full,
        "message": format!("model valid — wrote {fname}"),
    });
    (200, "application/json", value.to_string().into_bytes())
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

/// Identifiers quoted in backticks inside a diagnostic message — the hook
/// that lets the diagram map a typecheck error onto the boxes and wires that
/// caused it.
fn backticked(msg: &str) -> Vec<String> {
    msg.split('`').skip(1).step_by(2).map(|s| s.to_string()).collect()
}

/// The SCADE-style block symbol an equation renders as: a compact operator
/// box (`+`, `×`, `FBY`, `pre`, `ITE`, a cast type, a callee name, a literal)
/// when the rhs has a recognizable shape, or null for free-form text.
fn eq_symbol(rhs: &ol_ir::Expr) -> serde_json::Value {
    use ol_ir::{BinOp, Expr, UnaryOp};
    let op = |s: &str| serde_json::json!({ "kind": "op", "text": s });
    match rhs {
        Expr::Const { .. } => serde_json::json!({
            "kind": "const",
            "text": ol_lustre_emit::format_expr(rhs),
        }),
        Expr::Var { .. } => op("="),
        Expr::Binary { op: b, .. } => op(match b {
            BinOp::Add => "+",
            BinOp::Sub => "−",
            BinOp::Mul => "×",
            BinOp::Div => "/",
            BinOp::Mod => "mod",
            BinOp::Eq => "=",
            BinOp::Neq => "≠",
            BinOp::Lt => "<",
            BinOp::Le => "≤",
            BinOp::Gt => ">",
            BinOp::Ge => "≥",
            BinOp::And => "AND",
            BinOp::Or => "OR",
            BinOp::Xor => "XOR",
            BinOp::Implies => "⇒",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
        }),
        Expr::Unary { op: UnaryOp::Not, .. } => op("NOT"),
        Expr::Unary { op: UnaryOp::Neg, .. } => op("−"),
        // `init -> pre x` is SCADE's followed-by.
        Expr::Arrow { body, .. } if matches!(body.as_ref(), Expr::Pre { .. }) => op("FBY"),
        Expr::Arrow { .. } => op("->"),
        Expr::Pre { .. } => op("pre"),
        Expr::IfThenElse { .. } => op("ITE"),
        Expr::When { on: true, .. } => op("WHEN"),
        Expr::When { on: false, .. } => op("WHEN¬"),
        Expr::Merge { .. } => op("MERGE"),
        // Iterators render as a call-style block naming the iterated function,
        // so the user can dive into F like any operator call.
        Expr::Iterate { kind, node, .. } => serde_json::json!({
            "kind": "call",
            "text": format!("{}({node})", if *kind == ol_ir::IterKind::Map { "map" } else { "fold" }),
        }),
        Expr::Cast { to, .. } => op(&type_str(to)),
        Expr::Call { node, .. } => serde_json::json!({ "kind": "call", "text": node }),
        _ => serde_json::Value::Null,
    }
}

/// Build a diagram JSON for the requested node (or `main`). Inputs, locals,
/// equations, and outputs become boxes; wires are derived from each
/// equation's free variables (reads) and its lhs (writes). The front end
/// lays these out in columns and draws SVG lines — no layout engine needed.
///
/// Every wire and box also carries validity: reads or writes of undeclared
/// names become red "ghost" boxes with red wires, and typecheck errors for
/// this node are mapped onto the equation boxes (and their defining wires)
/// whose lhs the message names. Errors that cannot be pinned to an element
/// are returned in `problems` and rendered as a banner.
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

    // Free variables that are global constants or enum variants are
    // expression inputs, not wires — and not errors.
    let mut globals: std::collections::HashSet<String> = std::collections::HashSet::new();
    for pkg in &project.packages {
        for c in &pkg.constants {
            globals.insert(c.name.clone());
        }
        for t in &pkg.types {
            if let ol_ir::TypeBody::Enum(e) = &t.body {
                globals.extend(e.variants.iter().cloned());
            }
        }
    }

    // Map this node's typecheck errors onto equations — directly via the
    // `… · equation N` context the checker attaches, or by the lhs names the
    // message quotes — onto never-assigned ports, or onto ghosts; the rest
    // become banner problems.
    let report = ol_typecheck::check_project(&project);
    let node_ctx = format!("node {node_name}");
    let eq_ctx_prefix = format!("{node_ctx} · equation ");
    let mut eq_problems: Vec<Vec<String>> = vec![Vec::new(); node.equations.len()];
    let mut box_problems: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut box_warnings: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut ghost_reasons: std::collections::BTreeMap<String, String> = Default::default();
    let mut problems: Vec<String> = Vec::new();
    for d in report.diagnostics.iter().filter(|d| {
        d.context
            .iter()
            .any(|c| c == &node_ctx || c.starts_with(&eq_ctx_prefix))
    }) {
        let names = backticked(&d.message);
        let mut mapped = false;
        // Equation-tagged diagnostics pin to their box outright.
        if d.severity == ol_ir::Severity::Error {
            for c in &d.context {
                if let Some(i) = c
                    .strip_prefix(&eq_ctx_prefix)
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    if let Some(slot) = eq_problems.get_mut(i) {
                        slot.push(d.message.clone());
                        mapped = true;
                    }
                }
            }
        }
        let ctx_mapped = mapped;
        for n in &names {
            if !known.contains(n.as_str()) {
                if !globals.contains(n) && node.equations.iter().any(|eq| {
                    eq.lhs.iter().any(|l| l == n) || eq.rhs.free_vars().iter().any(|v| v == n)
                }) {
                    ghost_reasons.insert(n.clone(), d.message.clone());
                    mapped = true;
                }
                continue;
            }
            let mut assigned = false;
            for (i, eq) in node.equations.iter().enumerate() {
                if eq.lhs.iter().any(|l| l == n) {
                    assigned = true;
                    // Already pinned via its equation context — don't repeat
                    // the message on the same box.
                    if d.severity == ol_ir::Severity::Error && !ctx_mapped {
                        eq_problems[i].push(d.message.clone());
                        mapped = true;
                    }
                }
            }
            if !assigned {
                // Declared but never assigned (E0050/W0051): the port box
                // itself is the problem — an unconnected pin.
                match d.severity {
                    ol_ir::Severity::Error => {
                        box_problems.entry(n.clone()).or_default().push(d.message.clone())
                    }
                    _ => box_warnings.entry(n.clone()).or_default().push(d.message.clone()),
                }
                mapped = true;
            }
        }
        if !mapped && d.severity == ol_ir::Severity::Error {
            problems.push(format!("[{}] {}", d.code, d.message));
        }
    }

    let mut ghosts: std::collections::BTreeMap<String, String> = Default::default();
    let mut equations = Vec::new();
    let mut wires = Vec::new();
    for (i, eq) in node.equations.iter().enumerate() {
        let eq_id = format!("eq{i}");
        let invalid = !eq_problems[i].is_empty();
        let reason = eq_problems[i].join("; ");
        // The operand pins on the block's left edge, in evaluation order:
        // each free variable is one input pin (global constants are inlined,
        // not pins). Bound pins carry a wire from their source variable;
        // unbound (ghost) pins render red on the block itself.
        let mut reads: Vec<String> = Vec::new();
        let mut input_pins: Vec<serde_json::Value> = Vec::new();
        for v in eq.rhs.free_vars() {
            if globals.contains(&v) {
                continue;
            }
            let port = input_pins.len();
            if known.contains(v.as_str()) {
                reads.push(v.clone());
                wires.push(serde_json::json!({
                    "from": v.clone(), "to": eq_id, "to_port": port,
                }));
                input_pins.push(serde_json::json!({ "name": v, "bound": true }));
            } else {
                let why = ghost_reasons
                    .get(&v)
                    .cloned()
                    .unwrap_or_else(|| format!("`{v}` is not declared as an input, output, or local"));
                ghosts.insert(v.clone(), why.clone());
                input_pins.push(serde_json::json!({ "name": v, "bound": false, "reason": why }));
            }
        }
        let mut calls: Vec<String> = Vec::new();
        eq.rhs.visit(|e| {
            let callee = match e {
                ol_ir::Expr::Call { node, .. } => Some(node),
                // The iterated function is divable too.
                ol_ir::Expr::Iterate { node, .. } => Some(node),
                _ => None,
            };
            if let Some(callee) = callee {
                if !calls.contains(callee) {
                    calls.push(callee.clone());
                }
            }
        });
        for l in &eq.lhs {
            if !known.contains(l.as_str()) {
                let why = ghost_reasons
                    .get(l)
                    .cloned()
                    .unwrap_or_else(|| format!("`{l}` is not declared as an output or local"));
                ghosts.insert(l.clone(), why.clone());
                wires.push(serde_json::json!({
                    "from": eq_id, "to": l, "invalid": true, "reason": why,
                }));
            } else if invalid {
                wires.push(serde_json::json!({
                    "from": eq_id, "to": l, "invalid": true, "reason": reason,
                }));
            } else {
                wires.push(serde_json::json!({ "from": eq_id, "to": l }));
            }
        }
        // Variadic operation chains advertise their adjustable pin count so
        // the Properties sheet can offer an inputs control.
        let nary = flatten_nary(&eq.rhs)
            .and_then(|(op, operands)| {
                variadic_op_id(op).map(|id| {
                    serde_json::json!({
                        "op": id,
                        "inputs": operands.len(),
                        "min": MIN_VARIADIC_INPUTS,
                        "max": MAX_VARIADIC_INPUTS,
                    })
                })
            })
            .unwrap_or(serde_json::Value::Null);
        equations.push(serde_json::json!({
            "id": eq_id,
            "lhs": eq.lhs,
            "text": format!("{} = {}", eq.lhs.join(", "), ol_lustre_emit::format_expr(&eq.rhs)),
            "body": ol_lustre_emit::format_expr(&eq.rhs),
            "symbol": eq_symbol(&eq.rhs),
            "nary": nary,
            "reads": reads,
            "inputs": input_pins,
            "calls": calls,
            "invalid": invalid,
            "reason": if invalid { serde_json::Value::String(reason) } else { serde_json::Value::Null },
        }));
    }

    let port_json = |name: &str, ty: &ol_ir::Type| {
        let mut v = serde_json::json!({ "name": name, "type": ty });
        if let Some(msgs) = box_problems.get(name) {
            v["invalid"] = serde_json::Value::Bool(true);
            v["reason"] = serde_json::Value::String(msgs.join("; "));
        } else if let Some(msgs) = box_warnings.get(name) {
            v["warn"] = serde_json::Value::Bool(true);
            v["reason"] = serde_json::Value::String(msgs.join("; "));
        }
        v
    };

    let value = serde_json::json!({
        "schema_version": 1,
        "node": node.name,
        "kind": format!("{:?}", node.kind),
        "positions": node.diagram.positions,
        "grid": node.diagram.grid,
        "inputs": node.inputs.iter().map(|p| port_json(&p.name, &p.ty)).collect::<Vec<_>>(),
        "outputs": node.outputs.iter().map(|p| port_json(&p.name, &p.ty)).collect::<Vec<_>>(),
        "locals": node.locals.iter().map(|l| port_json(&l.name, &l.ty)).collect::<Vec<_>>(),
        "ghosts": ghosts.iter().map(|(name, reason)| serde_json::json!({
            "name": name, "reason": reason,
        })).collect::<Vec<_>>(),
        "equations": equations,
        "wires": wires,
        "problems": problems,
        "probes": node.probes.iter().map(|p| serde_json::json!({
            "label": p.label, "var": p.var,
        })).collect::<Vec<_>>(),
    });
    Ok(serde_json::to_string(&value).unwrap_or_default())
}

// --- Editing: parse, mutate, save back, return refreshed inspect ---

/// Parse one file directly (includes left untouched) so a save-back writes
/// only what the user authored. The full pipeline — includes, stdlib merge,
/// state-machine lowering — still runs on the *read* path (`load`), so
/// diagnostics reflect the complete picture.
fn load_raw_path(path: &std::path::Path) -> Result<ol_ir::Project, String> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    match path
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

fn save_raw_path(path: &std::path::Path, project: &ol_ir::Project) -> Result<(), String> {
    let text = match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => serde_json::to_string_pretty(project).map_err(|e| e.to_string())?,
        _ => serde_yaml::to_string(project).map_err(|e| e.to_string())?,
    };
    std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

fn load_raw(ctx: &ServerCtx) -> Result<ol_ir::Project, String> {
    load_raw_path(&ctx.model)
}

fn save_raw(ctx: &ServerCtx, project: &ol_ir::Project) -> Result<(), String> {
    save_raw_path(&ctx.model, project)
}

type EditFn = fn(&mut ol_ir::Project, &serde_json::Value) -> Result<(), String>;

fn apply_edit_response(ctx: &ServerCtx, body: &[u8], f: EditFn) -> (u16, &'static str, Vec<u8>) {
    apply_edit_response_to(ctx, &ctx.model.clone(), body, f)
}

/// Apply an edit to a specific file in the workspace (the model file or the
/// types file), then respond with a refreshed inspect of the whole project.
fn apply_edit_response_to(
    ctx: &ServerCtx,
    path: &std::path::Path,
    body: &[u8],
    f: EditFn,
) -> (u16, &'static str, Vec<u8>) {
    let req: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, "application/json", json_error(&format!("bad JSON: {e}")).into_bytes()),
    };
    let before = take_snapshot(ctx);
    let mut project = match load_raw_path(path) {
        Ok(p) => p,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    if let Err(e) = f(&mut project, &req) {
        return (400, "application/json", json_error(&e).into_bytes());
    }
    if let Err(e) = save_raw_path(path, &project) {
        return (500, "application/json", json_error(&e).into_bytes());
    }
    record_edit(ctx, before);
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
        probes: vec![],
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
        "mcdc": outcome.mcdc,
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

/// Persist free-form canvas positions (and optionally the grid pitch) for a
/// node: `{ "node": "...", "positions": { "id": {"x": .., "y": ..}, ... },
/// "grid": 8 }`.
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
    let grid = match req.get("grid") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|g| (1..=256).contains(g))
                .ok_or("grid must be an integer between 1 and 256")? as u32,
        ),
    };
    let mut map = std::collections::BTreeMap::new();
    for (id, pos) in positions {
        let x = pos.get("x").and_then(|v| v.as_f64()).ok_or("position missing x")?;
        let y = pos.get("y").and_then(|v| v.as_f64()).ok_or("position missing y")?;
        map.insert(id.clone(), ol_ir::NodePos { x, y });
    }
    for pkg in &mut project.packages {
        if let Some(n) = pkg.nodes.iter_mut().find(|n| n.name == node_name) {
            n.diagram.positions = map;
            if grid.is_some() {
                n.diagram.grid = grid;
            }
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

// --- Types file: named types, structs, enums, arrays ------------------------

/// Surface syntax for a type, matching what `parse_type` accepts so the GUI
/// can round-trip everything it displays.
fn type_str(t: &ol_ir::Type) -> String {
    use ol_ir::Type::*;
    match t {
        Bool => "bool".into(),
        Int8 => "int8".into(),
        Int16 => "int16".into(),
        Int32 => "int32".into(),
        Int64 => "int64".into(),
        Uint8 => "uint8".into(),
        Uint16 => "uint16".into(),
        Uint32 => "uint32".into(),
        Uint64 => "uint64".into(),
        Float32 => "float32".into(),
        Float64 => "float64".into(),
        Array { elem, len } => format!("{}[{}]", type_str(elem), len),
        Named { name } => name.clone(),
    }
}

/// The SCADE-style primitive palette every port/local type selector offers.
const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "int8", "int16", "int32", "int64", "uint8", "uint16", "uint32", "uint64",
    "float32", "float64",
];

fn types_list(ctx: &ServerCtx) -> Result<String, String> {
    let project = load(ctx)?;
    let mut types: Vec<serde_json::Value> = Vec::new();
    for pkg in &project.packages {
        for t in &pkg.types {
            let (kind, detail) = match &t.body {
                ol_ir::TypeBody::Enum(e) => ("enum", e.variants.join(" | ")),
                ol_ir::TypeBody::Record { fields, .. } => (
                    "record",
                    fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name, type_str(&f.ty)))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                ol_ir::TypeBody::Alias { target, .. } => ("alias", type_str(target)),
            };
            types.push(serde_json::json!({
                "package": pkg.name,
                "kind": kind,
                "name": t.name(),
                "detail": detail,
            }));
        }
    }
    let value = serde_json::json!({
        "schema_version": 1,
        "primitives": PRIMITIVE_TYPES,
        "types": types,
        "types_file": ctx.types_file.as_ref().map(|p| p.display().to_string()),
    });
    Ok(serde_json::to_string(&value).unwrap_or_default())
}

/// Create a named type. Uniqueness is checked against the *full* loaded
/// project (stdlib included), but the definition is saved into the workspace
/// types file when one exists — the SCADE types-file experience — falling
/// back to the model file itself.
fn add_type_response(ctx: &ServerCtx, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let bad = |msg: &str| (400, "application/json", json_error(msg).into_bytes());
    let req: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return bad(&format!("bad JSON: {e}")),
    };
    let name = match req.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return bad("missing string field `name`"),
    };
    if !is_identifier(&name) {
        return bad(&format!("`{name}` is not a valid type name"));
    }
    let full = match load(ctx) {
        Ok(p) => p,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    if full
        .packages
        .iter()
        .any(|p| p.types.iter().any(|t| t.name() == name))
    {
        return bad(&format!("type `{name}` already exists"));
    }

    let body_def = match req.get("kind").and_then(|v| v.as_str()) {
        Some("enum") => {
            let variants: Vec<String> = req
                .get("variants")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if variants.is_empty() {
                return bad("enum needs at least one variant");
            }
            if let Some(v) = variants.iter().find(|v| !is_identifier(v)) {
                return bad(&format!("`{v}` is not a valid variant name"));
            }
            ol_ir::TypeBody::Enum(ol_ir::EnumDef { name: name.clone(), variants })
        }
        Some("record") => {
            let mut fields = Vec::new();
            for f in req.get("fields").and_then(|v| v.as_array()).unwrap_or(&vec![]) {
                let fname = match f.get("name").and_then(|v| v.as_str()) {
                    Some(n) if is_identifier(n) => n.to_string(),
                    Some(n) => return bad(&format!("`{n}` is not a valid field name")),
                    None => return bad("record field missing `name`"),
                };
                let tstr = match f.get("type").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => return bad("record field missing `type`"),
                };
                let ty = match ol_stdlib::parse_type(tstr) {
                    Ok(t) => t,
                    Err(e) => return bad(&format!("field `{fname}`: {e}")),
                };
                fields.push(ol_ir::RecordField { name: fname, ty });
            }
            if fields.is_empty() {
                return bad("record needs at least one field");
            }
            ol_ir::TypeBody::Record { name: name.clone(), fields }
        }
        Some("alias") => {
            let target = match req.get("target").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => return bad("alias missing `target` (e.g. \"float32[3]\")"),
            };
            let ty = match ol_stdlib::parse_type(target) {
                Ok(t) => t,
                Err(e) => return bad(&format!("target `{target}`: {e}")),
            };
            ol_ir::TypeBody::Alias { name: name.clone(), target: ty }
        }
        other => return bad(&format!("kind must be enum|record|alias, got {other:?}")),
    };

    let target_path = ctx.types_file.clone().unwrap_or_else(|| ctx.model.clone());
    let before = take_snapshot(ctx);
    let mut doc = match load_raw_path(&target_path) {
        Ok(p) => p,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    if doc.packages.is_empty() {
        doc.packages.push(ol_ir::Package { name: "user".into(), ..Default::default() });
    }
    doc.packages[0].types.push(ol_ir::TypeDef { body: body_def });
    if let Err(e) = save_raw_path(&target_path, &doc) {
        return (500, "application/json", json_error(&e).into_bytes());
    }
    record_edit(ctx, before);
    match build_inspect(ctx) {
        Ok(b) => (200, "application/json", b.into_bytes()),
        Err(e) => (500, "application/json", json_error(&e).into_bytes()),
    }
}

/// Remove a named type from whichever editable file defines it (types file
/// first, then the model file). Uses of a removed type surface as typecheck
/// errors on the next refresh rather than blocking the removal.
fn remove_type_response(ctx: &ServerCtx, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let req: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, "application/json", json_error(&format!("bad JSON: {e}")).into_bytes()),
    };
    let name = match req.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return (400, "application/json", json_error("missing string field `name`").into_bytes()),
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(t) = &ctx.types_file {
        candidates.push(t.clone());
    }
    candidates.push(ctx.model.clone());
    for path in candidates {
        let mut doc = match load_raw_path(&path) {
            Ok(p) => p,
            Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
        };
        let mut removed = false;
        for pkg in &mut doc.packages {
            let before = pkg.types.len();
            pkg.types.retain(|t| t.name() != name);
            removed |= pkg.types.len() != before;
        }
        if removed {
            let before = take_snapshot(ctx);
            if let Err(e) = save_raw_path(&path, &doc) {
                return (500, "application/json", json_error(&e).into_bytes());
            }
            record_edit(ctx, before);
            return match build_inspect(ctx) {
                Ok(b) => (200, "application/json", b.into_bytes()),
                Err(e) => (500, "application/json", json_error(&e).into_bytes()),
            };
        }
    }
    (400, "application/json", json_error(&format!(
        "type `{name}` not found in the editable files (stdlib types cannot be removed)"
    )).into_bytes())
}

// --- Variable properties + in-place equation editing -------------------------

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Input,
    Output,
    Local,
}

fn role_of(node: &ol_ir::NodeDef, name: &str) -> Option<Role> {
    if node.inputs.iter().any(|p| p.name == name) {
        Some(Role::Input)
    } else if node.outputs.iter().any(|p| p.name == name) {
        Some(Role::Output)
    } else if node.locals.iter().any(|l| l.name == name) {
        Some(Role::Local)
    } else {
        None
    }
}

/// Update a variable: rename (rewriting every use in the node's equations),
/// retype, and/or change its role — the "treat this local as an output"
/// option, and its inverses.
fn edit_update_port(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let name = req_str(req, "name")?.to_string();
    let node = find_node_mut(project, &node_name)?;
    let role = role_of(node, &name)
        .ok_or_else(|| format!("`{name}` is not a port or local of `{node_name}`"))?;

    let new_name = req.get("new_name").and_then(|v| v.as_str()).map(str::to_string);
    let new_type = req.get("new_type").and_then(|v| v.as_str()).map(str::to_string);
    let new_role = req.get("new_role").and_then(|v| v.as_str()).map(str::to_string);

    if let Some(n) = &new_name {
        if n != &name {
            if !is_identifier(n) {
                return Err(format!("`{n}` is not a valid identifier"));
            }
            if role_of(node, n).is_some() {
                return Err(format!("`{n}` already exists on `{node_name}`"));
            }
            let rename = |list_name: &mut String| {
                if list_name == &name {
                    *list_name = n.clone();
                }
            };
            node.inputs.iter_mut().for_each(|p| rename(&mut p.name));
            node.outputs.iter_mut().for_each(|p| rename(&mut p.name));
            node.locals.iter_mut().for_each(|l| rename(&mut l.name));
            for eq in &mut node.equations {
                for l in &mut eq.lhs {
                    if l == &name {
                        *l = n.clone();
                    }
                }
                eq.rhs.rename_var(&name, n);
            }
            // The persisted layout follows the rename too.
            if let Some(pos) = node.diagram.positions.remove(&name) {
                node.diagram.positions.insert(n.clone(), pos);
            }
        }
    }
    let cur_name = new_name.unwrap_or_else(|| name.clone());

    if let Some(t) = &new_type {
        let ty = ol_stdlib::parse_type(t).map_err(|e| format!("type `{t}`: {e}"))?;
        node.inputs.iter_mut().filter(|p| p.name == cur_name).for_each(|p| p.ty = ty.clone());
        node.outputs.iter_mut().filter(|p| p.name == cur_name).for_each(|p| p.ty = ty.clone());
        node.locals.iter_mut().filter(|l| l.name == cur_name).for_each(|l| l.ty = ty.clone());
    }

    if let Some(r) = &new_role {
        let want = match r.as_str() {
            "input" => Role::Input,
            "output" => Role::Output,
            "local" => Role::Local,
            other => return Err(format!("new_role must be input|output|local, got `{other}`")),
        };
        if want != role {
            let ty = match role {
                Role::Input => {
                    let i = node.inputs.iter().position(|p| p.name == cur_name).unwrap();
                    node.inputs.remove(i).ty
                }
                Role::Output => {
                    let i = node.outputs.iter().position(|p| p.name == cur_name).unwrap();
                    node.outputs.remove(i).ty
                }
                Role::Local => {
                    let i = node.locals.iter().position(|l| l.name == cur_name).unwrap();
                    node.locals.remove(i).ty
                }
            };
            match want {
                Role::Input => node.inputs.push(ol_ir::Port { name: cur_name, ty }),
                Role::Output => node.outputs.push(ol_ir::Port { name: cur_name, ty }),
                Role::Local => node.locals.push(ol_ir::Local { name: cur_name, ty }),
            }
        }
    }
    Ok(())
}

/// Remove a variable. Equations that still reference it keep working as
/// red ghost pins on the canvas — visible, not silent.
fn edit_remove_port(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let name = req_str(req, "name")?.to_string();
    let node = find_node_mut(project, &node_name)?;
    let before =
        node.inputs.len() + node.outputs.len() + node.locals.len();
    node.inputs.retain(|p| p.name != name);
    node.outputs.retain(|p| p.name != name);
    node.locals.retain(|l| l.name != name);
    if node.inputs.len() + node.outputs.len() + node.locals.len() == before {
        return Err(format!("`{name}` is not a port or local of `{node_name}`"));
    }
    node.diagram.positions.remove(&name);
    Ok(())
}

fn req_index(req: &serde_json::Value) -> Result<usize, String> {
    req.get("index")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .ok_or_else(|| "missing integer field `index`".to_string())
}

fn edit_update_equation(
    project: &mut ol_ir::Project,
    req: &serde_json::Value,
) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let index = req_index(req)?;
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
    let eq = node
        .equations
        .get_mut(index)
        .ok_or_else(|| format!("`{node_name}` has no equation #{index}"))?;
    *eq = ol_ir::Equation { lhs, rhs };
    Ok(())
}

fn edit_remove_equation(
    project: &mut ol_ir::Project,
    req: &serde_json::Value,
) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let index = req_index(req)?;
    let node = find_node_mut(project, &node_name)?;
    if index >= node.equations.len() {
        return Err(format!("`{node_name}` has no equation #{index}"));
    }
    node.equations.remove(index);
    // Re-key the persisted layout: eqN ids above the removed index shift down.
    let old = std::mem::take(&mut node.diagram.positions);
    for (k, v) in old {
        match k.strip_prefix("eq").and_then(|s| s.parse::<usize>().ok()) {
            Some(n) if n == index => {}
            Some(n) if n > index => {
                node.diagram.positions.insert(format!("eq{}", n - 1), v);
            }
            _ => {
                node.diagram.positions.insert(k, v);
            }
        }
    }
    Ok(())
}

// --- Draw-on-canvas: drop a block/operator instance at a position -----------

/// Instantiate a callee on the host node's canvas at (x, y): fresh typed
/// locals are created for each callee output, and the call's arguments are
/// the callee's own input names — they bind to same-named host variables
/// automatically and show as red unbound pins otherwise, exactly the
/// drop-then-wire flow a SCADE user expects.
fn add_block_call_response(ctx: &ServerCtx, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let bad = |msg: &str| (400, "application/json", json_error(msg).into_bytes());
    let req: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return bad(&format!("bad JSON: {e}")),
    };
    let host_name = match req.get("node").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return bad("missing string field `node`"),
    };
    let callee_name = match req.get("callee").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return bad("missing string field `callee`"),
    };
    if host_name == callee_name {
        return bad("an operator cannot call itself");
    }
    let x = req.get("x").and_then(|v| v.as_f64()).unwrap_or(40.0);
    let y = req.get("y").and_then(|v| v.as_f64()).unwrap_or(40.0);

    // The callee usually lives in the stdlib, so its signature comes from the
    // fully merged project; the edit itself only touches the model file.
    let full = match load(ctx) {
        Ok(p) => p,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    let callee = match full.find_node(&callee_name) {
        Some(c) => c,
        None => return bad(&format!("operator `{callee_name}` not found")),
    };
    let callee_inputs: Vec<String> = callee.inputs.iter().map(|p| p.name.clone()).collect();
    let callee_outputs: Vec<(String, ol_ir::Type)> = callee
        .outputs
        .iter()
        .map(|p| (p.name.clone(), p.ty.clone()))
        .collect();

    let before = take_snapshot(ctx);
    let mut project = match load_raw(ctx) {
        Ok(p) => p,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    {
        let node = match find_node_mut(&mut project, &host_name) {
            Ok(n) => n,
            Err(e) => return bad(&e),
        };
        let mut known: std::collections::HashSet<String> = node
            .inputs
            .iter()
            .map(|p| p.name.clone())
            .chain(node.outputs.iter().map(|p| p.name.clone()))
            .chain(node.locals.iter().map(|l| l.name.clone()))
            .collect();
        let mut lhs = Vec::new();
        let mut fresh = Vec::new();
        for (oname, oty) in &callee_outputs {
            let base = format!("{}_{}", callee_name.to_lowercase(), oname);
            let mut name = base.clone();
            let mut n = 2;
            while known.contains(&name) {
                name = format!("{base}{n}");
                n += 1;
            }
            known.insert(name.clone());
            node.locals.push(ol_ir::Local { name: name.clone(), ty: oty.clone() });
            lhs.push(name.clone());
            fresh.push(name);
        }
        let args = callee_inputs.iter().map(ol_ir::Expr::var).collect();
        let eq_index = node.equations.len();
        node.equations.push(ol_ir::Equation {
            lhs,
            rhs: ol_ir::Expr::call(&callee_name, args),
        });
        node.diagram
            .positions
            .insert(format!("eq{eq_index}"), ol_ir::NodePos { x, y });
        for (i, l) in fresh.iter().enumerate() {
            node.diagram.positions.insert(
                l.clone(),
                ol_ir::NodePos { x: x + 320.0, y: y + 44.0 * i as f64 },
            );
        }
    }
    if let Err(e) = save_raw(ctx, &project) {
        return (500, "application/json", json_error(&e).into_bytes());
    }
    record_edit(ctx, before);
    match build_inspect(ctx) {
        Ok(b) => (200, "application/json", b.into_bytes()),
        Err(e) => (500, "application/json", json_error(&e).into_bytes()),
    }
}

// --- Predefined operations: the SCADE-style toolbox --------------------------

/// One predefined operation the toolbox offers. `pins` is the number of
/// ghost input pins the dropped instance starts with; `param` names an extra
/// piece of information the GUI must collect (`n`, `type`, `field`, `index`).
struct OpDef {
    id: &'static str,
    label: &'static str,
    pins: u8,
    out_type: &'static str,
    param: Option<&'static str>,
    enabled: bool,
    hint: &'static str,
}

/// Variadic (associative) operation blocks carry between 2 and 12 input
/// pins; 12 is a sanity ceiling — beyond it a block stops being readable.
const MIN_VARIADIC_INPUTS: usize = 2;
const MAX_VARIADIC_INPUTS: usize = 12;

/// The associative operations whose blocks may grow extra input pins,
/// with their IR operator and surface-syntax separator.
const VARIADIC_OPS: &[(&str, ol_ir::BinOp, &str)] = &[
    ("plus", ol_ir::BinOp::Add, " + "),
    ("multiply", ol_ir::BinOp::Mul, " * "),
    ("and", ol_ir::BinOp::And, " and "),
    ("or", ol_ir::BinOp::Or, " or "),
    ("xor", ol_ir::BinOp::Xor, " xor "),
    ("bit_and", ol_ir::BinOp::BitAnd, " & "),
    ("bit_or", ol_ir::BinOp::BitOr, " | "),
    ("bit_xor", ol_ir::BinOp::BitXor, " ^ "),
];

fn variadic_sep(id: &str) -> Option<&'static str> {
    VARIADIC_OPS.iter().find(|(i, _, _)| *i == id).map(|(_, _, s)| *s)
}

fn variadic_op_id(op: ol_ir::BinOp) -> Option<&'static str> {
    VARIADIC_OPS.iter().find(|(_, b, _)| *b == op).map(|(i, _, _)| *i)
}

/// The connection-point contract an operation presents to the engineer:
/// per-pin input types and the produced type. `T` means any type,
/// `number`/`integer` any numeric/integer type — the exact rules live in
/// the typechecker; this is the guidance the GUI displays on pins.
fn operation_signature(o: &OpDef) -> (Vec<&'static str>, &'static str) {
    let n = |t: &'static str| vec![t; o.pins as usize];
    match o.id {
        "constant" => (vec![], "literal type"),
        "plus" | "minus" | "multiply" | "divide" | "modulo" | "squared" | "cubed"
        | "to_nth_power" => (n("number"), "number"),
        "numeric_cast" => (vec!["number"], "target type"),
        "square_root" => (vec!["float"], "float"),
        "equal" | "not_equal" => (vec!["T", "T"], "bool"),
        "greater_than" | "greater_equal" | "less_than" | "less_equal" => {
            (vec!["number", "number"], "bool")
        }
        "and" | "or" | "xor" => (n("bool"), "bool"),
        "not" => (vec!["bool"], "bool"),
        "implies" => (vec!["bool", "bool"], "bool"),
        "record_field" => (vec!["structure"], "field type"),
        "array_index" => (vec!["array"], "element type"),
        "init_pre" | "arrow" => (vec!["T", "T"], "T"),
        "when" | "when_not" => (vec!["T", "bool clock"], "T on the clock"),
        "merge" => (vec!["bool clock", "T", "T"], "T"),
        "map" => (vec!["array(s)"], "array of F's output"),
        "fold" => (vec!["seed", "array"], "F's output"),
        "if_then_else" => (vec!["bool", "T", "T"], "T"),
        "bit_and" | "bit_or" | "bit_xor" => (n("integer"), "integer"),
        "shift_left" | "shift_right" => (vec!["integer", "integer"], "integer"),
        _ => (n("T"), "T"),
    }
}

/// If `rhs` is a chain of one associative toolbox operator, return that
/// operator and the operands in surface order. Only the top-level chain is
/// the block: `a and (b or c)` flattens to two operands, not three.
fn flatten_nary(rhs: &ol_ir::Expr) -> Option<(ol_ir::BinOp, Vec<ol_ir::Expr>)> {
    let ol_ir::Expr::Binary { op, .. } = rhs else { return None };
    variadic_op_id(*op)?;
    fn walk(e: &ol_ir::Expr, op: ol_ir::BinOp, out: &mut Vec<ol_ir::Expr>) {
        match e {
            ol_ir::Expr::Binary { op: o, lhs, rhs } if *o == op => {
                walk(lhs, op, out);
                walk(rhs, op, out);
            }
            _ => out.push(e.clone()),
        }
    }
    let mut operands = Vec::new();
    walk(rhs, *op, &mut operands);
    Some((*op, operands))
}

/// Human-readable pin contract, e.g. `bool × 2…12 → bool` for a variadic
/// operation or `bool, T, T → T` for a fixed one.
fn signature_text(o: &OpDef) -> String {
    let (ins, out) = operation_signature(o);
    if variadic_sep(o.id).is_some() {
        let t = ins.first().copied().unwrap_or("T");
        format!("{t} × {MIN_VARIADIC_INPUTS}…{MAX_VARIADIC_INPUTS} → {out}")
    } else if ins.is_empty() {
        format!("→ {out}")
    } else {
        format!("{} → {out}", ins.join(", "))
    }
}

const fn op(id: &'static str, label: &'static str, pins: u8, out_type: &'static str) -> OpDef {
    OpDef { id, label, pins, out_type, param: None, enabled: true, hint: "" }
}

/// The toolbox, in SCADE's operator-family order.
fn operation_families() -> Vec<(&'static str, Vec<OpDef>)> {
    vec![
        ("Mathematics", vec![
            OpDef { id: "constant", label: "constant (literal)", pins: 0, out_type: "int32",
                    param: Some("value"), enabled: true,
                    hint: "a literal source block: 2, 2.5, true" },
            op("plus", "plus (+)", 2, "int32"),
            op("minus", "minus (-)", 2, "int32"),
            op("multiply", "multiply (*)", 2, "int32"),
            op("divide", "divide (/)", 2, "int32"),
            op("modulo", "modulo (mod)", 2, "int32"),
            OpDef { id: "numeric_cast", label: "numeric_cast", pins: 1, out_type: "int32",
                    param: Some("type"), enabled: true, hint: "convert to int8…uint64, float32/64" },
            OpDef { id: "square_root", label: "square_root", pins: 1, out_type: "float64",
                    param: None, enabled: false,
                    hint: "needs float intrinsics (sqrt) across sim/C/Lustre — roadmap" },
            op("squared", "squared (x*x)", 1, "int32"),
            op("cubed", "cubed (x*x*x)", 1, "int32"),
            OpDef { id: "to_nth_power", label: "to_nth_power(n)", pins: 1, out_type: "int32",
                    param: Some("n"), enabled: true, hint: "n between 2 and 8" },
        ]),
        ("Comparisons", vec![
            op("equal", "equal (=)", 2, "bool"),
            op("not_equal", "not equal (<>)", 2, "bool"),
            op("greater_than", "greater than (>)", 2, "bool"),
            op("greater_equal", "greater or equal (>=)", 2, "bool"),
            op("less_than", "less than (<)", 2, "bool"),
            op("less_equal", "less or equal (<=)", 2, "bool"),
        ]),
        ("Logical", vec![
            op("and", "and", 2, "bool"),
            op("or", "or", 2, "bool"),
            op("xor", "xor", 2, "bool"),
            op("not", "not", 1, "bool"),
            op("implies", "implies (=>)", 2, "bool"),
        ]),
        ("Structures/Arrays", vec![
            OpDef { id: "record_field", label: "field access (.f)", pins: 1, out_type: "int32",
                    param: Some("field"), enabled: true, hint: "read one field of a structure" },
            OpDef { id: "array_index", label: "array index ([i])", pins: 1, out_type: "int32",
                    param: Some("index"), enabled: true, hint: "read one element by constant index" },
        ]),
        ("Time/Statefuls", vec![
            op("init_pre", "init -> pre (followed by)", 2, "int32"),
            op("arrow", "init -> (initialization)", 2, "int32"),
            OpDef { id: "when", label: "when (sample on clock)", pins: 2, out_type: "int32",
                    param: None, enabled: true,
                    hint: "pin 1 = the stream, pin 2 = a bool clock; runs only on true cycles" },
            OpDef { id: "when_not", label: "when not (sample on false)", pins: 2, out_type: "int32",
                    param: None, enabled: true,
                    hint: "pin 1 = the stream, pin 2 = a bool clock; runs only on false cycles" },
            OpDef { id: "merge", label: "merge (join clocked streams)", pins: 3, out_type: "int32",
                    param: None, enabled: true,
                    hint: "pin 1 = bool clock, pin 2 = stream when true, pin 3 = stream when false" },
        ]),
        ("Choice", vec![
            op("if_then_else", "if / then / else", 3, "int32"),
        ]),
        ("Bitwise", vec![
            op("bit_and", "bitwise and (&)", 2, "int32"),
            op("bit_or", "bitwise or (|)", 2, "int32"),
            op("bit_xor", "bitwise xor (^)", 2, "int32"),
            op("shift_left", "shift left (<<)", 2, "int32"),
            op("shift_right", "shift right (>>)", 2, "int32"),
        ]),
        ("Higher Order", vec![
            OpDef { id: "map", label: "map(F)", pins: 1, out_type: "int32",
                    param: Some("iterator"), enabled: true,
                    hint: "apply a function element-wise across array(s) → array" },
            OpDef { id: "fold", label: "fold(F)", pins: 2, out_type: "int32",
                    param: Some("iterator"), enabled: true,
                    hint: "reduce an array to a scalar: acc = F(acc, element)" },
        ]),
    ]
}

fn operations_catalog() -> serde_json::Value {
    let cats: Vec<serde_json::Value> = operation_families()
        .into_iter()
        .map(|(name, items)| {
            serde_json::json!({
                "name": name,
                "items": items.iter().map(|o| {
                    let (ins, out) = operation_signature(o);
                    let variadic = variadic_sep(o.id).is_some();
                    serde_json::json!({
                        "id": o.id,
                        "label": o.label,
                        "pins": o.pins,
                        "param": o.param,
                        "enabled": o.enabled,
                        "hint": o.hint,
                        "inputs": ins,
                        "output": out,
                        "signature": signature_text(o),
                        "variadic": variadic,
                        "min_pins": if variadic { MIN_VARIADIC_INPUTS } else { o.pins as usize },
                        "max_pins": if variadic { MAX_VARIADIC_INPUTS } else { o.pins as usize },
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "schema_version": 1, "categories": cats })
}

/// Build the equation body for an operation given its ghost pin names and
/// the collected parameter. Returns (body text, out type text).
fn operation_body(
    opdef: &OpDef,
    pins: &[String],
    param: Option<&str>,
) -> Result<(String, String), String> {
    let a = pins.first().cloned().unwrap_or_default();
    let b = pins.get(1).cloned().unwrap_or_default();
    let c = pins.get(2).cloned().unwrap_or_default();
    let body = match opdef.id {
        "constant" => {
            let v = param.ok_or("constant needs parameter `value`")?.trim().to_string();
            let parsed = ol_stdlib::parse_expr(&v).map_err(|e| format!("value `{v}`: {e}"))?;
            // A constant block is a literal source — nothing else.
            let lit = match &parsed {
                ol_ir::Expr::Const { lit } => lit.clone(),
                ol_ir::Expr::Unary { op: ol_ir::UnaryOp::Neg, arg } => match arg.as_ref() {
                    ol_ir::Expr::Const { lit } => lit.clone(),
                    _ => return Err(format!("`{v}` is not a literal")),
                },
                _ => return Err(format!("`{v}` is not a literal (e.g. 2, 2.5, true)")),
            };
            let ty = match lit {
                ol_ir::Literal::Bool { .. } => "bool",
                ol_ir::Literal::Int { .. } => "int32",
                ol_ir::Literal::Float { .. } => "float64",
            };
            return Ok((v, ty.to_string()));
        }
        // Associative operations join every pin: `a + b + c + …`.
        "plus" | "multiply" | "and" | "or" | "xor" | "bit_and" | "bit_or" | "bit_xor" => {
            pins.join(variadic_sep(opdef.id).expect("listed in VARIADIC_OPS"))
        }
        "minus" => format!("{a} - {b}"),
        "divide" => format!("{a} / {b}"),
        "modulo" => format!("{a} mod {b}"),
        "squared" => format!("{a} * {a}"),
        "cubed" => format!("{a} * {a} * {a}"),
        "to_nth_power" => {
            let n: u32 = param
                .and_then(|p| p.parse().ok())
                .ok_or("to_nth_power needs integer parameter `n`")?;
            if !(2..=8).contains(&n) {
                return Err("to_nth_power: n must be between 2 and 8".into());
            }
            vec![a.clone(); n as usize].join(" * ")
        }
        "numeric_cast" => {
            let t = param.ok_or("numeric_cast needs parameter `type`")?;
            let ty = ol_stdlib::parse_type(t).map_err(|e| format!("cast type `{t}`: {e}"))?;
            if !ty.is_numeric() {
                return Err(format!("numeric_cast target must be numeric, got `{t}`"));
            }
            let body = format!("{t}({a})");
            return Ok((body, t.to_string()));
        }
        "equal" => format!("{a} = {b}"),
        "not_equal" => format!("{a} <> {b}"),
        "greater_than" => format!("{a} > {b}"),
        "greater_equal" => format!("{a} >= {b}"),
        "less_than" => format!("{a} < {b}"),
        "less_equal" => format!("{a} <= {b}"),
        "not" => format!("not {a}"),
        "implies" => format!("{a} => {b}"),
        "record_field" => {
            let f = param.ok_or("field access needs parameter `field`")?;
            if !is_identifier(f) {
                return Err(format!("`{f}` is not a valid field name"));
            }
            format!("{a}.{f}")
        }
        "array_index" => {
            let i: u64 = param
                .and_then(|p| p.parse().ok())
                .ok_or("array index needs integer parameter `index`")?;
            format!("{a}[{i}]")
        }
        "init_pre" => format!("{a} -> pre {b}"),
        "arrow" => format!("{a} -> {b}"),
        "when" => format!("{a} when {b}"),
        "when_not" => format!("{a} when not {b}"),
        "merge" => format!("merge({a}, {b}, {c})"),
        "if_then_else" => format!("if {a} then {b} else {c}"),
        "shift_left" => format!("{a} << {b}"),
        "shift_right" => format!("{a} >> {b}"),
        other => return Err(format!("unknown operation `{other}`")),
    };
    Ok((body, opdef.out_type.to_string()))
}

/// Resolve a `map`/`fold` drop against the project: the `param` carries the
/// iterated function's name (and, for `map`, the array length as `F:N`).
/// Returns the ghost pins, the equation body, and the result local's type —
/// derived from the function's signature so the diagram is typed on drop.
fn resolve_iterator_drop(
    project: &ol_ir::Project,
    op_id: &str,
    param: Option<&str>,
    eq_index: usize,
) -> Result<(Vec<String>, String, ol_ir::Type), String> {
    let raw = param.ok_or("map/fold needs the iterated function name (e.g. `Scale:4` for map)")?;
    let (f_name, len_opt) = match raw.split_once(':') {
        Some((f, n)) => (f.trim(), Some(n.trim())),
        None => (raw.trim(), None),
    };
    let f = project
        .find_node(f_name)
        .ok_or_else(|| format!("unknown function `{f_name}`"))?;
    if !matches!(f.kind, ol_ir::NodeKind::Function) {
        return Err(format!("`{f_name}` must be a stateless `function` to iterate"));
    }
    if f.outputs.len() != 1 {
        return Err(format!("`{f_name}` must have exactly one output"));
    }
    let out_elem = f.outputs[0].ty.clone();
    if op_id == "map" {
        let k = f.inputs.len();
        if k == 0 {
            return Err(format!("`{f_name}` has no inputs to map over"));
        }
        let n: u32 = len_opt
            .and_then(|s| s.parse().ok())
            .ok_or("map needs the array length, e.g. `Scale:4`")?;
        let pins: Vec<String> = (1..=k).map(|i| format!("p{eq_index}_{i}")).collect();
        let body = format!("map({f_name}, {})", pins.join(", "));
        Ok((pins, body, ol_ir::Type::Array { elem: Box::new(out_elem), len: n }))
    } else {
        // fold(F, seed, array): F is (accumulator, element) -> accumulator.
        if f.inputs.len() != 2 {
            return Err(format!(
                "fold needs `{f_name}` to take two inputs (accumulator, element)"
            ));
        }
        let pins = vec![format!("p{eq_index}_1"), format!("p{eq_index}_2")];
        let body = format!("fold({f_name}, {}, {})", pins[0], pins[1]);
        Ok((pins, body, out_elem))
    }
}

/// Drop a predefined operation onto a node's canvas at (x, y): a fresh typed
/// local receives the result, the inputs start as red unbound pins, and the
/// new equation lands at the drop position.
fn add_operation_response(ctx: &ServerCtx, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let bad = |msg: &str| (400, "application/json", json_error(msg).into_bytes());
    let req: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return bad(&format!("bad JSON: {e}")),
    };
    let host_name = match req.get("node").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return bad("missing string field `node`"),
    };
    let op_id = match req.get("op").and_then(|v| v.as_str()) {
        Some(o) => o.to_string(),
        None => return bad("missing string field `op`"),
    };
    let x = req.get("x").and_then(|v| v.as_f64()).unwrap_or(40.0);
    let y = req.get("y").and_then(|v| v.as_f64()).unwrap_or(40.0);
    let param = req.get("param").and_then(|v| v.as_str()).map(|s| s.to_string());

    let opdef = match operation_families()
        .into_iter()
        .flat_map(|(_, items)| items)
        .find(|o| o.id == op_id)
    {
        Some(o) => o,
        None => return bad(&format!("unknown operation `{op_id}`")),
    };
    if !opdef.enabled {
        return bad(&format!("`{op_id}` is not implemented yet: {}", opdef.hint));
    }
    // Variadic operations may be dropped with extra pins right away;
    // everything else has a fixed contract.
    let pin_count = match req.get("inputs").and_then(|v| v.as_u64()) {
        None => opdef.pins as usize,
        Some(n) => {
            if variadic_sep(&op_id).is_none() {
                return bad(&format!("`{op_id}` has a fixed number of inputs ({})", opdef.pins));
            }
            let n = n as usize;
            if !(MIN_VARIADIC_INPUTS..=MAX_VARIADIC_INPUTS).contains(&n) {
                return bad(&format!(
                    "inputs must be between {MIN_VARIADIC_INPUTS} and {MAX_VARIADIC_INPUTS}"
                ));
            }
            n
        }
    };

    let before = take_snapshot(ctx);
    let mut project = match load_raw(ctx) {
        Ok(p) => p,
        Err(e) => return (500, "application/json", json_error(&e).into_bytes()),
    };
    // Resolve the equation's pins, body, and result type. Iterators read the
    // iterated function's signature from the project (immutable), so this
    // happens before the mutable node borrow below.
    let eq_index = match project.find_node(&host_name) {
        Some(n) => n.equations.len(),
        None => return bad(&format!("node `{host_name}` not found")),
    };
    let (_pins, body_text, out_ty) = if op_id == "map" || op_id == "fold" {
        match resolve_iterator_drop(&project, &op_id, param.as_deref(), eq_index) {
            Ok(v) => v,
            Err(e) => return bad(&e),
        }
    } else {
        let pins: Vec<String> = (1..=pin_count).map(|k| format!("p{eq_index}_{k}")).collect();
        let (body_text, out_type) = match operation_body(&opdef, &pins, param.as_deref()) {
            Ok(v) => v,
            Err(e) => return bad(&e),
        };
        match ol_stdlib::parse_type(&out_type) {
            Ok(t) => (pins, body_text, t),
            Err(e) => return bad(&format!("internal out-type error: {e}")),
        }
    };
    let rhs = match ol_stdlib::parse_expr(&body_text) {
        Ok(e) => e,
        Err(e) => return bad(&format!("internal template error: {e}")),
    };
    {
        let node = match find_node_mut(&mut project, &host_name) {
            Ok(n) => n,
            Err(e) => return bad(&e),
        };
        let known: std::collections::HashSet<String> = node
            .inputs
            .iter()
            .map(|p| p.name.clone())
            .chain(node.outputs.iter().map(|p| p.name.clone()))
            .chain(node.locals.iter().map(|l| l.name.clone()))
            .collect();
        let base = format!("{op_id}{eq_index}");
        let mut lhs_name = base.clone();
        let mut n = 2;
        while known.contains(&lhs_name) {
            lhs_name = format!("{base}_{n}");
            n += 1;
        }
        node.locals.push(ol_ir::Local { name: lhs_name.clone(), ty: out_ty });
        node.equations.push(ol_ir::Equation { lhs: vec![lhs_name.clone()], rhs });
        node.diagram
            .positions
            .insert(format!("eq{eq_index}"), ol_ir::NodePos { x, y });
        node.diagram
            .positions
            .insert(lhs_name, ol_ir::NodePos { x: x + 320.0, y });
    }
    if let Err(e) = save_raw(ctx, &project) {
        return (500, "application/json", json_error(&e).into_bytes());
    }
    record_edit(ctx, before);
    match build_inspect(ctx) {
        Ok(b) => (200, "application/json", b.into_bytes()),
        Err(e) => (500, "application/json", json_error(&e).into_bytes()),
    }
}

/// Change the input-pin count of a variadic operation block in place
/// (`{node, index, inputs}`): growing appends fresh red ghost pins so the
/// engineer sees exactly what still needs wiring, shrinking drops the
/// trailing operands. Bound pins keep their wiring. 2..=12.
fn edit_set_operation_inputs(
    project: &mut ol_ir::Project,
    req: &serde_json::Value,
) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let index = req_index(req)?;
    let want = req
        .get("inputs")
        .and_then(|v| v.as_u64())
        .ok_or("missing integer field `inputs`")? as usize;
    if !(MIN_VARIADIC_INPUTS..=MAX_VARIADIC_INPUTS).contains(&want) {
        return Err(format!(
            "inputs must be between {MIN_VARIADIC_INPUTS} and {MAX_VARIADIC_INPUTS}"
        ));
    }
    let node = find_node_mut(project, &node_name)?;
    // Every name visible in this node — fresh ghost pins must not collide
    // with a declared variable (silent binding) or another ghost (merged pins).
    let mut used: std::collections::HashSet<String> = node
        .inputs
        .iter()
        .map(|p| p.name.clone())
        .chain(node.outputs.iter().map(|p| p.name.clone()))
        .chain(node.locals.iter().map(|l| l.name.clone()))
        .collect();
    for eq in &node.equations {
        used.extend(eq.lhs.iter().cloned());
        used.extend(eq.rhs.free_vars());
    }
    let eq = node
        .equations
        .get_mut(index)
        .ok_or_else(|| format!("node `{node_name}` has no equation {index}"))?;
    let (op, mut operands) = flatten_nary(&eq.rhs).ok_or(
        "this block has a fixed number of inputs — edit its expression instead",
    )?;
    if operands.len() == want {
        return Ok(());
    }
    if want < operands.len() {
        operands.truncate(want);
    } else {
        let mut k = operands.len() + 1;
        while operands.len() < want {
            let mut name = format!("p{index}_{k}");
            while used.contains(&name) {
                k += 1;
                name = format!("p{index}_{k}");
            }
            used.insert(name.clone());
            operands.push(ol_ir::Expr::Var { name });
            k += 1;
        }
    }
    // Rebuild a left-associative chain — the shape the parser produces.
    let mut it = operands.into_iter();
    let first = it.next().expect("at least MIN_VARIADIC_INPUTS operands");
    eq.rhs = it.fold(first, |acc, e| ol_ir::Expr::bin(op, acc, e));
    Ok(())
}

/// Add a debug log probe (`{node, var, label}`): logs `<label>: <var value>`
/// in a debug run. `var` must be a name in the node.
fn edit_add_probe(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let var = req_str(req, "var")?.to_string();
    let label = req
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or(&var)
        .to_string();
    let node = find_node_mut(project, &node_name)?;
    let known = node
        .inputs
        .iter()
        .map(|p| &p.name)
        .chain(node.outputs.iter().map(|p| &p.name))
        .chain(node.locals.iter().map(|l| &l.name))
        .any(|n| n == &var);
    if !known {
        return Err(format!("`{var}` is not an input, output, or local of `{node_name}`"));
    }
    node.probes.push(ol_ir::Probe { label, var });
    Ok(())
}

/// Remove the probe at `index` from a node (`{node, index}`).
fn edit_remove_probe(project: &mut ol_ir::Project, req: &serde_json::Value) -> Result<(), String> {
    let node_name = req_str(req, "node")?.to_string();
    let index = req_index(req)?;
    let node = find_node_mut(project, &node_name)?;
    if index >= node.probes.len() {
        return Err(format!("node `{node_name}` has no log message {index}"));
    }
    node.probes.remove(index);
    Ok(())
}

// --- Compile C-Lite: emit + compile into a user directory --------------------

/// Emit the selected root's C (header, source, driver, monitors, Makefile)
/// into `out_dir` and compile it on this machine with the requested
/// compiler. Cross-compilation is not attempted — the target is the host.
fn clite_compile_response(ctx: &ServerCtx, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let bad = |msg: &str| (400, "application/json", json_error(msg).into_bytes());
    let req: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return bad(&format!("bad JSON: {e}")),
    };
    let compiler = req.get("compiler").and_then(|v| v.as_str()).unwrap_or("auto");
    let out_dir = match req.get("out_dir").and_then(|v| v.as_str()) {
        Some(d) if !d.trim().is_empty() => PathBuf::from(d),
        _ => ctx
            .model
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("build"),
    };

    let project = match sliced_for_main(ctx) {
        Ok(p) => p,
        Err(e) => return bad(&e),
    };
    let entry_name = match project.main.clone() {
        Some(m) => m,
        None => return bad("project has no `main` operator; set one first"),
    };
    let entry = match project.find_node(&entry_name) {
        Some(n) => n.clone(),
        None => return bad(&format!("main operator `{entry_name}` not found")),
    };

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        return bad(&format!("creating {}: {e}", out_dir.display()));
    }
    let bundle = ol_clite_emit::emit_project(&project);
    let has_contract = entry.contract.is_some();
    let driver = if has_contract {
        ol_clite_emit::harness::emit_csv_driver_with_monitor(&entry, entry.contract.as_deref())
    } else {
        ol_clite_emit::harness::emit_csv_driver(&entry)
    };
    let mut wrote = vec![
        ("openlustre_generated.h", bundle.header),
        ("openlustre_generated.c", bundle.source),
        ("driver.c", driver),
        ("Makefile", crate::makefile_for_entry(&entry_name)),
    ];
    let mut sources = vec!["openlustre_generated.c", "driver.c"];
    if has_contract {
        let mon = ol_clite_emit::monitor::emit_monitors(&project);
        wrote.push(("openlustre_monitors.h", mon.header));
        wrote.push(("openlustre_monitors.c", mon.source));
        sources.push("openlustre_monitors.c");
    }
    for (name, text) in &wrote {
        if let Err(e) = std::fs::write(out_dir.join(name), text) {
            return bad(&format!("writing {name}: {e}"));
        }
    }

    let exe_name = if cfg!(windows) {
        format!("{entry_name}.exe")
    } else {
        entry_name.clone()
    };
    let compile_log =
        crate::scenario::compile_in_dir(&out_dir, &sources, &exe_name, Some(compiler));
    let (compiled, log) = match compile_log {
        Ok(l) => (true, l),
        Err(e) => (false, e),
    };
    let value = serde_json::json!({
        "schema_version": 1,
        "out_dir": out_dir.display().to_string(),
        "wrote": wrote.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        "compiled": compiled,
        "exe": if compiled { serde_json::Value::String(out_dir.join(&exe_name).display().to_string()) } else { serde_json::Value::Null },
        "log": log,
    });
    (200, "application/json", value.to_string().into_bytes())
}

/// Compile + run the C-Lite in DEBUG mode and launch it in its own terminal
/// window: a free-running build (no CSV) that prints a banner, the held
/// inputs, and the outputs plus any log-message probes every 50 cycles. This
/// is the GUI's fourth pipeline button — "watch the generated code run".
fn clite_run_response(ctx: &ServerCtx, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let bad = |msg: &str| (400, "application/json", json_error(msg).into_bytes());
    let req: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    let compiler = req.get("compiler").and_then(|v| v.as_str()).unwrap_or("auto");
    let out_dir = ctx
        .model
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("build");

    let project = match sliced_for_main(ctx) {
        Ok(p) => p,
        Err(e) => return bad(&e),
    };
    let entry_name = match project.main.clone() {
        Some(m) => m,
        None => return bad("project has no `main` operator; set one first"),
    };
    let entry = match project.find_node(&entry_name) {
        Some(n) => n.clone(),
        None => return bad(&format!("main operator `{entry_name}` not found")),
    };
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        return bad(&format!("creating {}: {e}", out_dir.display()));
    }

    let bundle = ol_clite_emit::emit_project(&project);
    let driver = ol_clite_emit::harness::emit_debug_driver(&entry);
    for (name, text) in [
        ("openlustre_generated.h", &bundle.header),
        ("openlustre_generated.c", &bundle.source),
        ("debug_driver.c", &driver),
    ] {
        if let Err(e) = std::fs::write(out_dir.join(name), text) {
            return bad(&format!("writing {name}: {e}"));
        }
    }
    let exe_name = if cfg!(windows) {
        format!("{entry_name}_debug.exe")
    } else {
        format!("{entry_name}_debug")
    };
    let sources = ["openlustre_generated.c", "debug_driver.c"];
    let log = match crate::scenario::compile_in_dir_defs(
        &out_dir,
        &sources,
        &exe_name,
        Some(compiler),
        &["OL_DEBUG"],
    ) {
        Ok(l) => l,
        Err(e) => {
            let v = serde_json::json!({ "ok": false, "compiled": false, "log": e });
            return (200, "application/json", v.to_string().into_bytes());
        }
    };

    let exe = out_dir.join(&exe_name);
    let (launched, note) = launch_in_terminal(&exe);
    let value = serde_json::json!({
        "ok": true,
        "compiled": true,
        "launched": launched,
        "exe": exe.display().to_string(),
        "message": note,
        "log": log,
    });
    (200, "application/json", value.to_string().into_bytes())
}

/// Open the compiled executable in its own terminal window so the user can
/// watch it run. Windows pops a `cmd` window that stays open; other platforms
/// try common terminals, falling back to a detached run.
fn launch_in_terminal(exe: &std::path::Path) -> (bool, String) {
    let exe_str = exe.display().to_string();
    #[cfg(windows)]
    {
        // `cmd /c start "" cmd /k "<exe>"` — the empty title keeps the exe path
        // from being parsed as the window title; /k leaves the window open.
        let r = std::process::Command::new("cmd")
            .args(["/c", "start", "", "cmd", "/k", &exe_str])
            .spawn();
        match r {
            Ok(_) => (true, "launched in a new terminal window".into()),
            Err(e) => (false, format!("compiled, but could not open a terminal: {e}")),
        }
    }
    #[cfg(not(windows))]
    {
        for (term, args) in [
            ("x-terminal-emulator", vec!["-e"]),
            ("gnome-terminal", vec!["--"]),
            ("xterm", vec!["-e"]),
        ] {
            if std::process::Command::new(term)
                .args(&args)
                .arg(&exe_str)
                .spawn()
                .is_ok()
            {
                return (true, format!("launched in {term}"));
            }
        }
        match std::process::Command::new(&exe_str).spawn() {
            Ok(_) => (true, "no terminal found — running detached".into()),
            Err(e) => (false, format!("compiled, but could not run: {e}")),
        }
    }
}
