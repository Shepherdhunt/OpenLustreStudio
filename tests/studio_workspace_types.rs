//! The SCADE project workflow: open a workspace folder (project skeleton is
//! created on first open), define named types in the types file, give every
//! port/local a defined data type, change a variable's role or name in
//! place, edit/remove equations, and draw on the canvas by dropping operator
//! instances that arrive with typed fresh outputs and red unbound pins.

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

/// Serve a path (file or workspace directory) that already lives inside the
/// temp dir `tmp`.
fn start_server(tmp: PathBuf, serve_path: &std::path::Path) -> ServerGuard {
    let mut child = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "studio", "serve"])
        .arg(serve_path)
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

/// Open a fresh workspace directory — the skeleton must be created by the
/// server on first open.
fn start_server_on_workspace(tag: &str) -> ServerGuard {
    let tmp = make_tempdir(tag);
    let ws = tmp.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let serve = ws.clone();
    start_server(tmp, &serve)
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

fn post_ok(port: u16, path: &str, body: &str) {
    let (s, b) = request(port, "POST", path, body).expect(path);
    assert_eq!(s, 200, "{path} failed: {b}");
}

fn get_json(port: u16, path: &str) -> serde_json::Value {
    let (s, body) = request(port, "GET", path, "").expect(path);
    assert_eq!(s, 200, "{path} failed: {body}");
    serde_json::from_str(&body).unwrap()
}

// --- Workspace: opening a folder creates the project skeleton ---------------

#[test]
fn opening_a_workspace_directory_creates_the_project_skeleton() {
    let g = start_server_on_workspace("ws_skel");
    let ws = g.tmp.join("ws");

    // The skeleton exists on disk: the `.wksc` workspace file (named after the
    // folder), the types file, and the scenarios dir.
    assert!(ws.join("ws.wksc").exists(), "ws.wksc workspace file missing");
    assert!(ws.join("types.json").exists(), "types.json missing");
    assert!(ws.join("scenarios").is_dir(), "scenarios/ missing");

    // The served project is the workspace: named after the folder, includes
    // the starter operator, and the types file is wired through `includes`.
    let ins = get_json(g.port, "/api/inspect");
    assert_eq!(ins["project"]["name"], "ws");
    assert_eq!(ins["project"]["main"], "Heartbeat");
    let on_disk = std::fs::read_to_string(ws.join("ws.wksc")).unwrap();
    assert!(on_disk.contains("types.json"), "workspace must include types.json");
}

/// The `.wksc` workspace file ties every operator together: operators created
/// in the workspace are persisted into that single file, and the workspace
/// lists them all on re-inspect.
#[test]
fn workspace_wksc_ties_operators_together() {
    let g = start_server_on_workspace("ws_wksc");
    let ws = g.tmp.join("ws");
    let port = g.port;

    let wksc = ws.join("ws.wksc");
    assert!(wksc.exists(), "workspace .wksc created");

    // Operators added in the workspace land in the same `.wksc`.
    post_ok(port, "/api/edit/add_node", r#"{"name":"Alpha","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_node", r#"{"name":"Beta","kind":"operator"}"#);
    let on_disk = std::fs::read_to_string(&wksc).unwrap();
    assert!(
        on_disk.contains("Alpha") && on_disk.contains("Beta"),
        "both operators are tied together in the one workspace file: {on_disk}"
    );

    // The workspace lists every operator (the starter plus the two new ones).
    let ins = get_json(port, "/api/inspect");
    let ops: Vec<String> = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().cloned().unwrap_or_default())
        .map(|n| n["name"].as_str().unwrap_or("").to_string())
        .collect();
    for want in ["Heartbeat", "Alpha", "Beta"] {
        assert!(ops.iter().any(|n| n == want), "{want} in the workspace: {ops:?}");
    }
}

/// Dragging a TYPE onto an operator's canvas inserts the kind-specific action:
/// record MAKE/FLATTEN, array MAKE/FLATTEN/SLICE, and enum variant/compare/
/// match. Each call succeeds (its IR template parses + saves) and leaves the
/// expected locals on the operator.
#[test]
fn drag_a_type_inserts_make_flatten_slice_and_enum_ops() {
    let g = start_server_on_workspace("ws_typeops");
    let port = g.port;

    post_ok(port, "/api/edit/add_type",
        r#"{"kind":"record","name":"Pt","fields":[{"name":"x","type":"int32"},{"name":"y","type":"int32"}]}"#);
    post_ok(port, "/api/edit/add_type", r#"{"kind":"alias","name":"Vec3","target":"int32[3]"}"#);
    post_ok(port, "/api/edit/add_type", r#"{"kind":"enum","name":"Mode","variants":["OFF","ON"]}"#);
    post_ok(port, "/api/edit/add_node", r#"{"name":"Builder","kind":"operator"}"#);

    let op = |ty: &str, action: &str, param: Option<&str>| {
        let mut p = serde_json::json!({ "node": "Builder", "type": ty, "action": action, "x": 40, "y": 40 });
        if let Some(pm) = param { p["param"] = serde_json::json!(pm); }
        let (s, b) = request(port, "POST", "/api/edit/add_type_op", &p.to_string()).expect("type op");
        assert_eq!(s, 200, "{ty}/{action} should succeed: {b}");
    };

    op("Pt", "make", None);
    op("Pt", "flatten", None);
    op("Vec3", "make", None);
    op("Vec3", "flatten", None);
    op("Vec3", "slice", Some("0,2"));
    op("Mode", "variant", Some("ON"));
    op("Mode", "compare", Some("OFF"));
    op("Mode", "match", Some("int32"));

    let ins = get_json(port, "/api/inspect");
    let builder = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().cloned().unwrap_or_default())
        .find(|n| n["name"] == "Builder").expect("Builder present");
    let locals: Vec<String> = builder["locals"].as_array().unwrap().iter()
        .map(|l| l["name"].as_str().unwrap_or("").to_string()).collect();
    // Struct/array FLATTEN exposed the field/element locals; SLICE, variant,
    // compare (named "is"), and match each left their result local.
    for want in ["Pt_in", "x", "y", "Vec3_in", "elem0", "elem1", "elem2", "slice", "variant", "is", "match"] {
        assert!(locals.iter().any(|n| n == want), "local `{want}` from a type op: {locals:?}");
    }

    // A bad slice range is rejected.
    let (s, _) = request(port, "POST", "/api/edit/add_type_op",
        &serde_json::json!({"node":"Builder","type":"Vec3","action":"slice","x":40,"y":40,"param":"2,5"}).to_string())
        .expect("bad slice");
    assert_eq!(s, 400, "out-of-bounds slice must be rejected");
}

/// The literal / expression block (and a dragged constant, which posts the same
/// way) infers its result type from the operator's context, and rejects an
/// expression over a name that isn't in scope.
#[test]
fn literal_expression_block_infers_type_and_rejects_unknown() {
    let g = start_server_on_workspace("ws_expr");
    let port = g.port;
    post_ok(port, "/api/edit/add_node", r#"{"name":"Calc","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_port", r#"{"node":"Calc","side":"input","name":"x","type":"int32"}"#);
    post_ok(port, "/api/edit/add_constant", r#"{"name":"limit","type":"int32","value":"100"}"#);

    let expr = |body: &str| -> u16 {
        let p = serde_json::json!({ "node": "Calc", "body": body, "x": 40, "y": 40 });
        request(port, "POST", "/api/edit/add_expression", &p.to_string()).expect("expr").0
    };

    // A condition over an in-scope input, a typed literal, and a constant
    // reference (the constant is upper-cased to LIMIT on create) all succeed.
    assert_eq!(expr("8 > x"), 200, "condition over an input");
    assert_eq!(expr("8_i32"), 200, "typed literal");
    assert_eq!(expr("LIMIT"), 200, "constant reference");
    // An expression over an unknown name is rejected (works only "if x is visible").
    assert_eq!(expr("y + 1"), 400, "unknown identifier must be rejected");

    // The first block (`8 > x`) produced a bool-typed `expr` local.
    let ins = get_json(port, "/api/inspect");
    let calc = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().cloned().unwrap_or_default())
        .find(|n| n["name"] == "Calc").expect("Calc present");
    let expr_local = calc["locals"].as_array().unwrap().iter()
        .find(|l| l["name"] == "expr").cloned().expect("expr local created");
    assert_eq!(expr_local["type"]["kind"], "Bool", "`8 > x` infers bool: {expr_local}");
}

/// File ▸ New / Open Workspace switch the server's active workspace at runtime:
/// New seeds and switches to a fresh `.wksc`, edits land there (not the old
/// one), and Open switches back.
#[test]
fn workspace_new_open_save_switches_the_active_workspace() {
    let g = start_server_on_workspace("ws_switch");
    let port = g.port;
    // The helper opened tmp/ws (named "ws").
    assert_eq!(get_json(port, "/api/inspect")["project"]["name"], "ws");

    // New Workspace in tmp/wsB → switch to a fresh EMPTY project named wsB
    // (no starter/Heartbeat carried in).
    let wsb = g.tmp.join("wsB");
    let (s, b) = request(port, "POST", "/api/workspace/new",
        &serde_json::json!({ "path": wsb.to_str().unwrap() }).to_string()).expect("new");
    assert_eq!(s, 200, "new workspace: {b}");
    assert!(wsb.join("wsB.wksc").exists(), "wsB.wksc created");
    let fresh = get_json(port, "/api/inspect");
    assert_eq!(fresh["project"]["name"], "wsB", "switched to wsB");
    assert!(fresh["project"]["main"].is_null(), "a new workspace is empty (no main)");
    let user_ops: Vec<String> = fresh["project"]["packages"].as_array().unwrap().iter()
        .filter(|p| p["name"] != "stdlib")
        .flat_map(|p| p["nodes"].as_array().cloned().unwrap_or_default())
        .filter_map(|n| n["name"].as_str().map(String::from))
        .collect();
    assert!(user_ops.is_empty(), "a new workspace has no user operators, got {user_ops:?}");

    // An edit now lands in wsB's file.
    post_ok(port, "/api/edit/add_node", r#"{"name":"OnlyInB","kind":"operator"}"#);
    assert!(std::fs::read_to_string(wsb.join("wsB.wksc")).unwrap().contains("OnlyInB"));

    // Open the original workspace back; OnlyInB must not be there.
    let (s2, _) = request(port, "POST", "/api/workspace/open",
        &serde_json::json!({ "path": g.tmp.join("ws").to_str().unwrap() }).to_string()).expect("open");
    assert_eq!(s2, 200);
    let ins = get_json(port, "/api/inspect");
    assert_eq!(ins["project"]["name"], "ws", "switched back to ws");
    let leaked = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().cloned().unwrap_or_default())
        .any(|n| n["name"] == "OnlyInB");
    assert!(!leaked, "OnlyInB must not leak into ws");

    // Save succeeds; opening a non-existent path is rejected.
    assert_eq!(request(port, "POST", "/api/workspace/save", "").expect("save").0, 200);
    let (s3, _) = request(port, "POST", "/api/workspace/open",
        r#"{"path":"/no/such/workspace.wksc"}"#).expect("bad open");
    assert_eq!(s3, 400, "opening a missing path is rejected");
}

/// The Open/Save "browse" backend: `/api/fs/list` navigates the filesystem
/// (dirs + workspace files + a parent for "Up"), and `save_as` writes the
/// current workspace to a new path and switches to it.
#[test]
fn fs_list_navigates_and_save_as_switches() {
    let g = start_server_on_workspace("ws_fs");
    let port = g.port;
    let ws = g.tmp.join("ws");

    // Browse the workspace folder: its `.wksc` shows up and there is a parent.
    let v = get_json(port, &format!("/api/fs/list?path={}", ws.to_str().unwrap()));
    let files: Vec<String> = v["files"].as_array().unwrap().iter()
        .filter_map(|f| f["name"].as_str().map(String::from)).collect();
    assert!(files.iter().any(|f| f == "ws.wksc"), "fs/list shows the workspace file: {files:?}");
    assert!(v["parent"].is_string(), "fs/list provides a parent for Up");

    // Save As to a new folder: writes the .wksc + switches to it.
    let dst = g.tmp.join("saved").join("copy.wksc");
    let (s, b) = request(port, "POST", "/api/workspace/save_as",
        &serde_json::json!({ "path": dst.to_str().unwrap() }).to_string()).expect("save_as");
    assert_eq!(s, 200, "save_as: {b}");
    assert!(dst.exists(), "save_as wrote {}", dst.display());
    assert_eq!(get_json(port, "/api/inspect")["project"]["name"], "copy", "switched to the saved copy");
}

/// `openlustre new --empty` seeds a project with no operators; the Studio
/// serves it without trouble and it stays editable (you can add operators in).
#[test]
fn empty_new_project_has_no_operators_and_is_editable() {
    let tmp = make_tempdir("ws_empty");
    let ws = tmp.join("ws");
    let status = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--", "new", "--empty"])
        .arg(&ws)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .status()
        .expect("new --empty");
    assert!(status.success(), "`new --empty` should succeed");

    let g = start_server(tmp, &ws);
    let port = g.port;

    // No root, and no operators in the (non-stdlib) packages.
    let ins = get_json(port, "/api/inspect");
    assert!(ins["project"]["main"].is_null(), "empty project has no main: {}", ins["project"]["main"]);
    let user_nodes: usize = ins["project"]["packages"].as_array().unwrap().iter()
        .filter(|p| p["name"] != "stdlib")
        .map(|p| p["nodes"].as_array().map_or(0, |a| a.len()))
        .sum();
    assert_eq!(user_nodes, 0, "an empty project starts with no operators: {ins}");

    // It is still fully editable — add an operator into the empty project.
    post_ok(port, "/api/edit/add_node", r#"{"name":"First","kind":"operator"}"#);
    let ins2 = get_json(port, "/api/inspect");
    let names: Vec<String> = ins2["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().cloned().unwrap_or_default())
        .filter_map(|n| n["name"].as_str().map(str::to_string))
        .collect();
    assert!(names.contains(&"First".to_string()), "added operator should appear: {names:?}");
}

/// Project-wide constants: add one via the API (the name upper-cases by
/// convention), see it in inspect as NAME : type = value, reference it from an
/// operator (`out = NAME`) so the operator builds clean with the constant in
/// its Lustre, then remove it.
#[test]
fn project_constants_add_use_and_remove() {
    let g = start_server_on_workspace("ws_const");
    let port = g.port;

    post_ok(port, "/api/edit/add_constant", r#"{"name":"max_speed","type":"uint16","value":"32"}"#);
    let ins = get_json(port, "/api/inspect");
    let consts: Vec<serde_json::Value> = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["constants"].as_array().cloned().unwrap_or_default())
        .collect();
    let c = consts.iter().find(|c| c["name"] == "MAX_SPEED").expect("MAX_SPEED present");
    assert_eq!(c["value"], "32", "value shown in inspect: {c}");

    // Reference it from a fresh operator — out = MAX_SPEED builds clean.
    post_ok(port, "/api/edit/add_node", r#"{"name":"UsesConst","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_port", r#"{"node":"UsesConst","side":"output","name":"out","type":"uint16"}"#);
    post_ok(port, "/api/edit/add_equation", r#"{"node":"UsesConst","lhs":"out","body":"MAX_SPEED"}"#);
    let (s, body) = request(port, "POST", "/api/build", r#"{"node":"UsesConst"}"#).expect("build");
    assert_eq!(s, 200);
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["ok"], true, "operator using a constant should build: {d}");
    assert!(d["lustre"].as_str().unwrap().contains("MAX_SPEED"), "constant in lustre: {d}");

    // A duplicate (case-folded) name is rejected.
    let (s2, _) = request(port, "POST", "/api/edit/add_constant",
        r#"{"name":"MAX_SPEED","type":"int8","value":"1"}"#).expect("dup");
    assert_eq!(s2, 400, "duplicate constant name must be rejected");

    // Remove it.
    post_ok(port, "/api/edit/remove_constant", r#"{"name":"MAX_SPEED"}"#);
    let ins2 = get_json(port, "/api/inspect");
    let still = ins2["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["constants"].as_array().cloned().unwrap_or_default())
        .any(|c| c["name"] == "MAX_SPEED");
    assert!(!still, "constant removed");
}

/// Composite constant VALUES authored through the same dialog/endpoint: an
/// `int32[3]` array and a `char`. Both land in the project (names upper-cased),
/// the char round-trips its quoted value, and an operator that indexes the
/// array constant builds clean.
#[test]
fn project_composite_constants_array_and_char() {
    let g = start_server_on_workspace("ws_composite_const");
    let port = g.port;

    post_ok(port, "/api/edit/add_constant",
        r#"{"name":"palette","type":"int32[3]","value":"[10; 20; 30]"}"#);
    post_ok(port, "/api/edit/add_constant",
        r#"{"name":"letter","type":"char","value":"'A'"}"#);

    let ins = get_json(port, "/api/inspect");
    let consts: Vec<serde_json::Value> = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["constants"].as_array().cloned().unwrap_or_default())
        .collect();
    assert!(consts.iter().any(|c| c["name"] == "PALETTE"), "PALETTE present: {consts:?}");
    let letter = consts.iter().find(|c| c["name"] == "LETTER").expect("LETTER present");
    assert_eq!(letter["value"], "'A'", "char constant round-trips its quoted value: {letter}");

    // Index the array constant from a fresh operator — it builds clean and the
    // constant is referenced by name in the generated Lustre.
    post_ok(port, "/api/edit/add_node", r#"{"name":"UsesArray","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"UsesArray","side":"input","name":"i","type":"int32"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"UsesArray","side":"output","name":"out","type":"int32"}"#);
    post_ok(port, "/api/edit/add_equation",
        r#"{"node":"UsesArray","lhs":"out","body":"PALETTE[i]"}"#);
    let (s, body) = request(port, "POST", "/api/build", r#"{"node":"UsesArray"}"#).expect("build");
    assert_eq!(s, 200);
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["ok"], true, "operator indexing an array constant should build: {d}");
    assert!(d["lustre"].as_str().unwrap().contains("PALETTE"), "array const used in lustre: {d}");
}

/// Import existing Lustre: a node, a type, and a const are parsed and added to
/// the project; the imported operator builds and carries the imported const;
/// re-importing the same name collides and is rejected (all-or-nothing).
#[test]
fn import_lustre_adds_nodes_types_constants() {
    let g = start_server_on_workspace("ws_import");
    let port = g.port;
    let lus = "type Mode = enum { OFF, ON };\n\
               const LIMIT : int = 7;\n\
               node MyLimiter(x: int) returns (y: int);\n\
               let y = if x > LIMIT then LIMIT else x; tel\n";
    let payload = serde_json::json!({ "lustre": lus }).to_string();

    let (s, body) = request(port, "POST", "/api/edit/import_lustre", &payload).expect("import");
    assert_eq!(s, 200, "import failed: {body}");
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["ok"], true, "{d}");
    assert!(d["nodes"].as_array().unwrap().iter().any(|n| n == "MyLimiter"), "MyLimiter reported: {d}");

    // It's in the project and builds, carrying the imported constant.
    let ins = get_json(port, "/api/inspect");
    let has_node = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().cloned().unwrap_or_default())
        .any(|n| n["name"] == "MyLimiter");
    assert!(has_node, "MyLimiter imported into the project: {ins}");
    let (sb, bb) = request(port, "POST", "/api/build", r#"{"node":"MyLimiter"}"#).expect("build");
    assert_eq!(sb, 200);
    let bd: serde_json::Value = serde_json::from_str(&bb).unwrap();
    assert_eq!(bd["ok"], true, "imported operator should build: {bd}");
    assert!(bd["lustre"].as_str().unwrap().contains("LIMIT"), "imported const used: {bd}");

    // Re-importing the same operator name collides.
    let (s2, _) = request(port, "POST", "/api/edit/import_lustre", &payload).expect("reimport");
    assert_eq!(s2, 400, "duplicate import must be rejected");
}

/// State machines: create one textually, see it listed distinctly in inspect,
/// edit it in place (update adds a state), and confirm it is owned by exactly
/// one operator (merged into its body, not a standalone node), that the owning
/// operator builds, and the one-per-operator / owner-exists rules hold.
#[test]
fn state_machine_owned_by_operator_create_edit_build_remove() {
    let g = start_server_on_workspace("ws_sm");
    let port = g.port;

    // The owning operator: Switch(flip) returns (lit).
    post_ok(port, "/api/edit/add_node", r#"{"name":"Switch","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_port", r#"{"node":"Switch","side":"input","name":"flip","type":"bool"}"#);
    post_ok(port, "/api/edit/add_port", r#"{"node":"Switch","side":"output","name":"lit","type":"bool"}"#);

    let machine = |name: &str, op: &str, states: &str| -> String {
        format!(
            r#"{{"name":"{name}","operator":"{op}","initial_state":"Off",
                 "inputs":[{{"name":"flip","type":"bool"}}],
                 "outputs":[{{"name":"lit","type":"bool"}}],
                 "states":[{states}]}}"#
        )
    };
    let off = r#"{"name":"Off","equations":[{"lhs":"lit","body":"false"}],"transitions":[{"guard":"flip","target":"On"}]}"#;
    let on = r#"{"name":"On","equations":[{"lhs":"lit","body":"true"}],"transitions":[{"guard":"flip","target":"Off"}]}"#;
    let blink = r#"{"name":"Blink","equations":[{"lhs":"lit","body":"true"}],"transitions":[]}"#;

    post_ok(port, "/api/edit/add_state_machine", &machine("Toggle", "Switch", &format!("{off},{on}")));

    // Owned by Switch, with its state names; NOT a standalone operator node.
    let ins = get_json(port, "/api/inspect");
    let sm = ins["project"]["state_machines"].as_array().unwrap().iter()
        .find(|m| m["name"] == "Toggle").cloned().expect("Toggle present");
    assert_eq!(sm["owner"], "Switch", "owned by Switch: {sm}");
    assert_eq!(sm["states"].as_array().unwrap().len(), 2);
    let is_node = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().cloned().unwrap_or_default())
        .any(|n| n["name"] == "Toggle");
    assert!(!is_node, "an owned machine must not be a standalone operator node");

    // Edit in place: add a third state.
    post_ok(port, "/api/edit/update_state_machine", &machine("Toggle", "Switch", &format!("{off},{on},{blink}")));
    let ins = get_json(port, "/api/inspect");
    let sm = ins["project"]["state_machines"].as_array().unwrap().iter()
        .find(|m| m["name"] == "Toggle").cloned().unwrap();
    assert_eq!(sm["states"].as_array().unwrap().len(), 3, "update added a state");

    // The owning operator builds — its merged automaton drives `lit`.
    let (sb, bb) = request(port, "POST", "/api/build", r#"{"node":"Switch"}"#).expect("build");
    assert_eq!(sb, 200);
    let bd: serde_json::Value = serde_json::from_str(&bb).unwrap();
    assert_eq!(bd["ok"], true, "operator with an owned machine should build: {bd}");
    assert!(bd["lustre"].as_str().unwrap().contains("node Switch"), "Switch in lustre: {bd}");

    // One machine per operator, and the owner must exist.
    let (s2, _) = request(port, "POST", "/api/edit/add_state_machine",
        &machine("Toggle2", "Switch", &format!("{off},{on}"))).expect("dup owner");
    assert_eq!(s2, 400, "an operator owns at most one machine");
    let (s3, _) = request(port, "POST", "/api/edit/add_state_machine",
        &machine("Orphan", "Ghost", &format!("{off},{on}"))).expect("bad owner");
    assert_eq!(s3, 400, "the owning operator must exist");

    // Remove: missing rejected, existing succeeds.
    let (sr, _) = request(port, "POST", "/api/edit/remove_state_machine", r#"{"name":"Nope"}"#).expect("rm");
    assert_eq!(sr, 400, "removing a missing machine is rejected");
    post_ok(port, "/api/edit/remove_state_machine", r#"{"name":"Toggle"}"#);
    let ins = get_json(port, "/api/inspect");
    let gone = !ins["project"]["state_machines"].as_array().unwrap().iter().any(|m| m["name"] == "Toggle");
    assert!(gone, "Toggle removed");
}

/// A state machine that would not translate cleanly (a per-state output
/// assigned a value of the wrong type) is REJECTED at create time — before it
/// is ever saved — so the model never holds a machine that fails to lower to
/// Lustre / autogenerate C. The corrected machine is then accepted and its
/// operator builds.
#[test]
fn state_machine_create_is_rejected_unless_it_translates_cleanly() {
    let g = start_server_on_workspace("ws_sm_validate");
    let port = g.port;

    post_ok(port, "/api/edit/add_node", r#"{"name":"Gate","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_port", r#"{"node":"Gate","side":"input","name":"flip","type":"bool"}"#);
    post_ok(port, "/api/edit/add_port", r#"{"node":"Gate","side":"output","name":"lit","type":"bool"}"#);

    // `lit` is bool; both states must assign it (SCADE cover). The On state's
    // body is the variable — `true` is valid, `5` is an int (a type error that
    // only surfaces once the machine is merged into Gate and type-checked).
    let machine = |on_body: &str| -> String {
        let off = r#"{"name":"Off","equations":[{"lhs":"lit","body":"false"}],"transitions":[{"guard":"flip","target":"On"}]}"#;
        let on = format!(
            r#"{{"name":"On","equations":[{{"lhs":"lit","body":"{on_body}"}}],"transitions":[{{"guard":"flip","target":"Off"}}]}}"#
        );
        format!(
            r#"{{"name":"M","operator":"Gate","initial_state":"Off",
                 "inputs":[{{"name":"flip","type":"bool"}}],
                 "outputs":[{{"name":"lit","type":"bool"}}],
                 "states":[{off},{on}]}}"#
        )
    };

    let (bad, body) = request(port, "POST", "/api/edit/add_state_machine", &machine("5")).expect("bad sm");
    assert_eq!(bad, 400, "a machine that would not type-check must be rejected: {body}");
    let ins = get_json(port, "/api/inspect");
    assert!(
        !ins["project"]["state_machines"].as_array().unwrap().iter().any(|m| m["name"] == "M"),
        "the rejected machine must not be persisted"
    );

    // The corrected machine is accepted, and its operator builds + generates.
    post_ok(port, "/api/edit/add_state_machine", &machine("true"));
    let (sb, bb) = request(port, "POST", "/api/build", r#"{"node":"Gate"}"#).expect("build");
    assert_eq!(sb, 200);
    let bd: serde_json::Value = serde_json::from_str(&bb).unwrap();
    assert_eq!(bd["ok"], true, "the valid machine's operator builds: {bd}");
}

// --- Types file: enums, records, aliases/arrays round-trip ------------------

#[test]
fn named_types_save_into_the_types_file_and_serve_back() {
    let g = start_server_on_workspace("ws_types");
    let ws = g.tmp.join("ws");
    let port = g.port;

    post_ok(port, "/api/edit/add_type",
        r#"{"kind":"enum","name":"Mode","variants":["OFF","ARMED","FIRING"]}"#);
    post_ok(port, "/api/edit/add_type",
        r#"{"kind":"record","name":"Sample","fields":[{"name":"value","type":"float32"},{"name":"valid","type":"bool"}]}"#);
    post_ok(port, "/api/edit/add_type",
        r#"{"kind":"alias","name":"Vec3","target":"float32[3]"}"#);

    // All three serve back with the primitive palette.
    let t = get_json(port, "/api/types");
    let prims: Vec<&str> = t["primitives"].as_array().unwrap()
        .iter().map(|p| p.as_str().unwrap()).collect();
    for p in ["bool", "int8", "int16", "int32", "int64",
              "uint8", "uint16", "uint32", "uint64", "float32", "float64"] {
        assert!(prims.contains(&p), "primitive palette missing {p}");
    }
    let names: Vec<&str> = t["types"].as_array().unwrap()
        .iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Mode") && names.contains(&"Sample") && names.contains(&"Vec3"),
        "served types: {names:?}");

    // The definitions live in the TYPES file, not the workspace file.
    let types_disk = std::fs::read_to_string(ws.join("types.json")).unwrap();
    let project_disk = std::fs::read_to_string(ws.join("ws.wksc")).unwrap();
    for n in ["Mode", "Sample", "Vec3"] {
        assert!(types_disk.contains(n), "{n} not in types.json");
        assert!(!project_disk.contains(n), "{n} leaked into the workspace file");
    }

    // Duplicates are rejected; removal works and is durable.
    let (s, _) = request(port, "POST", "/api/edit/add_type",
        r#"{"kind":"enum","name":"Mode","variants":["X"]}"#).unwrap();
    assert_eq!(s, 400, "duplicate type must be rejected");
    post_ok(port, "/api/edit/remove_type", r#"{"name":"Vec3"}"#);
    let types_disk = std::fs::read_to_string(ws.join("types.json")).unwrap();
    assert!(!types_disk.contains("Vec3"));

    // Ports can use the named types; the model checks clean.
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Heartbeat","side":"input","name":"sample","type":"Sample"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Heartbeat","side":"input","name":"mode","type":"Mode"}"#);
    let ins = get_json(port, "/api/inspect");
    assert_eq!(ins["summary"]["errors"], 0, "named-type ports must typecheck: {}",
        ins["diagnostics"]);
}

// --- Variable properties: rename, retype, role change, delete ---------------

#[test]
fn update_port_renames_retypes_and_changes_role_in_place() {
    let g = start_server_on_workspace("ws_props");
    let port = g.port;

    post_ok(port, "/api/edit/add_node", r#"{"name":"Calc","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Calc","side":"input","name":"x","type":"int32"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Calc","side":"output","name":"y","type":"int32"}"#);
    post_ok(port, "/api/edit/add_local", r#"{"node":"Calc","name":"t","type":"int32"}"#);
    post_ok(port, "/api/edit/add_equation", r#"{"node":"Calc","lhs":"t","body":"x + 1"}"#);
    post_ok(port, "/api/edit/add_equation", r#"{"node":"Calc","lhs":"y","body":"t * 2"}"#);

    // Rename the local: every use in every equation follows.
    post_ok(port, "/api/edit/update_port",
        r#"{"node":"Calc","name":"t","new_name":"doubled_in"}"#);
    let d = get_json(port, "/api/diagram?node=Calc");
    assert_eq!(d["equations"][0]["lhs"][0], "doubled_in");
    assert!(d["equations"][1]["body"].as_str().unwrap().contains("doubled_in"),
        "reader not rewritten: {}", d["equations"][1]["body"]);
    assert!(d["ghosts"].as_array().unwrap().is_empty(), "rename left ghosts");

    // "Treat local as output": role change, keeping type and equations.
    post_ok(port, "/api/edit/update_port",
        r#"{"node":"Calc","name":"doubled_in","new_role":"output"}"#);
    let ins = get_json(port, "/api/inspect");
    let calc = ins["project"]["packages"].as_array().unwrap().iter()
        .flat_map(|p| p["nodes"].as_array().unwrap())
        .find(|n| n["name"] == "Calc").unwrap();
    let outs: Vec<&str> = calc["outputs"].as_array().unwrap()
        .iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(outs.contains(&"doubled_in"), "role change missing: {outs:?}");
    assert_eq!(ins["summary"]["errors"], 0, "{}", ins["diagnostics"]);

    // Retype the input: int32 -> float32 makes `x + 1`'s target mismatch
    // visible immediately (the red-wire substrate at work).
    post_ok(port, "/api/edit/update_port",
        r#"{"node":"Calc","name":"x","new_type":"float32"}"#);
    let d = get_json(port, "/api/diagram?node=Calc");
    assert_eq!(d["equations"][0]["invalid"], true,
        "retype must surface the type mismatch: {}", d["equations"][0]);

    // Delete the input: its readers show it as a ghost, not silence.
    post_ok(port, "/api/edit/remove_port", r#"{"node":"Calc","name":"x"}"#);
    let d = get_json(port, "/api/diagram?node=Calc");
    assert!(d["ghosts"].as_array().unwrap().iter().any(|g| g["name"] == "x"),
        "deleted variable must ghost: {}", d["ghosts"]);
}

// --- Equations: edit and delete in place ------------------------------------

#[test]
fn equations_update_and_remove_with_layout_renumbering() {
    let g = start_server_on_workspace("ws_eqs");
    let port = g.port;

    post_ok(port, "/api/edit/add_node", r#"{"name":"Two","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Two","side":"input","name":"a","type":"bool"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Two","side":"output","name":"p","type":"bool"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Two","side":"output","name":"q","type":"bool"}"#);
    post_ok(port, "/api/edit/add_equation", r#"{"node":"Two","lhs":"p","body":"a"}"#);
    post_ok(port, "/api/edit/add_equation", r#"{"node":"Two","lhs":"q","body":"not a"}"#);
    post_ok(port, "/api/edit/set_layout",
        r#"{"node":"Two","positions":{"eq0":{"x":100.0,"y":50.0},"eq1":{"x":100.0,"y":150.0}}}"#);

    // In-place edit.
    post_ok(port, "/api/edit/update_equation",
        r#"{"node":"Two","index":1,"lhs":"q","body":"a and a"}"#);
    let d = get_json(port, "/api/diagram?node=Two");
    assert!(d["equations"][1]["text"].as_str().unwrap().contains("and"),
        "{}", d["equations"][1]["text"]);

    // Remove eq0: the old eq1 becomes eq0 and keeps its canvas position.
    post_ok(port, "/api/edit/remove_equation", r#"{"node":"Two","index":0}"#);
    let d = get_json(port, "/api/diagram?node=Two");
    assert_eq!(d["equations"].as_array().unwrap().len(), 1);
    assert_eq!(d["positions"]["eq0"]["y"], 150.0,
        "layout must follow the renumbered equation: {}", d["positions"]);

    // Out-of-range edits are rejected.
    let (s, _) = request(port, "POST", "/api/edit/remove_equation",
        r#"{"node":"Two","index":7}"#).unwrap();
    assert_eq!(s, 400);
}

// --- Draw on canvas: drop an operator instance, then bind its pins ----------

#[test]
fn dropping_a_block_creates_a_placed_call_with_red_pins_then_binds() {
    let g = start_server_on_workspace("ws_drop");
    let port = g.port;

    post_ok(port, "/api/edit/add_node", r#"{"name":"Press","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Press","side":"input","name":"button","type":"bool"}"#);

    // Drop a stdlib RisingEdge at (96, 200): the call equation lands there,
    // a fresh typed local is created for the block's output, and the block's
    // unbound input pin `x` shows as a red ghost.
    post_ok(port, "/api/edit/add_block_call",
        r#"{"node":"Press","callee":"RisingEdge","x":96.0,"y":200.0}"#);
    let d = get_json(port, "/api/diagram?node=Press");
    assert_eq!(d["equations"][0]["calls"][0], "RisingEdge");
    assert_eq!(d["positions"]["eq0"]["x"], 96.0);
    assert_eq!(d["positions"]["eq0"]["y"], 200.0);
    let locals: Vec<&str> = d["locals"].as_array().unwrap()
        .iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert!(locals.iter().any(|l| l.starts_with("risingedge_")),
        "fresh output local missing: {locals:?}");
    assert!(d["ghosts"].as_array().unwrap().iter().any(|gh| gh["name"] == "x"),
        "unbound pin must ghost red: {}", d["ghosts"]);

    // Bind the pin: rewrite the call argument to the host's variable.
    post_ok(port, "/api/edit/update_equation",
        &format!(r#"{{"node":"Press","index":0,"lhs":"{}","body":"RisingEdge(button)"}}"#,
            locals[0]));
    let d = get_json(port, "/api/diagram?node=Press");
    assert!(d["ghosts"].as_array().unwrap().is_empty(),
        "bound pin still ghosting: {}", d["ghosts"]);
    for w in d["wires"].as_array().unwrap() {
        assert!(w.get("invalid").is_none(), "wire still red after binding: {w}");
    }

    // Unknown callee is a 400, not a panic.
    let (s, _) = request(port, "POST", "/api/edit/add_block_call",
        r#"{"node":"Press","callee":"NoSuchBlock","x":0,"y":0}"#).unwrap();
    assert_eq!(s, 400);
}
