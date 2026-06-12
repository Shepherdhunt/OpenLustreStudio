//! numeric_cast as a first-class IR operator (parse, typecheck, simulate,
//! generated C, all agreeing), the SCADE-style predefined-operations toolbox
//! (/api/operations + /api/edit/add_operation), and in-GUI C compilation
//! (/api/clite/compile).

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

// --- Cast: parser + formatter round-trip ------------------------------------

#[test]
fn cast_parses_and_round_trips_through_the_surface_formatter() {
    let e = ol_stdlib::parse_expr("int8(x + 200)").expect("parse cast");
    assert!(matches!(&e, ol_ir::Expr::Cast { to: ol_ir::Type::Int8, .. }), "{e:?}");
    let text = ol_lustre_emit::format_expr(&e);
    assert_eq!(text, "int8(x + 200)");
    let again = ol_stdlib::parse_expr(&text).expect("re-parse");
    assert_eq!(e, again, "surface text must round-trip");

    // The Kind 2 view renders casts as user-suppliable functions, like bit ops.
    let lus = ol_lustre_emit::format_expr_lustre(&e);
    assert_eq!(lus, "int_cast(x + 200)");
    let f = ol_stdlib::parse_expr("float32(x)").unwrap();
    assert_eq!(ol_lustre_emit::format_expr_lustre(&f), "real_cast(x)");

    // Casting is numeric-only: a two-argument "cast" is a parse error.
    assert!(ol_stdlib::parse_expr("int8(a, b)").is_err());
}

fn cast_model() -> serde_json::Value {
    serde_json::json!({
        "name": "casts",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Caster",
                "kind": "Function",
                "inputs": [{"name": "x", "ty": {"kind": "Int32"}}],
                "outputs": [
                    {"name": "wrapped", "ty": {"kind": "Int8"}},
                    {"name": "as_u8", "ty": {"kind": "Uint8"}},
                    {"name": "widened", "ty": {"kind": "Float64"}}
                ],
                "equations": [
                    {"lhs": ["wrapped"],
                     "rhs": {"expr": "Cast", "to": {"kind": "Int8"},
                             "arg": {"expr": "Binary", "op": "Add",
                                 "lhs": {"expr": "Var", "name": "x"},
                                 "rhs": {"expr": "Const", "lit": {"lit": "Int", "value": 200}}}}},
                    {"lhs": ["as_u8"],
                     "rhs": {"expr": "Cast", "to": {"kind": "Uint8"},
                             "arg": {"expr": "Var", "name": "x"}}},
                    {"lhs": ["widened"],
                     "rhs": {"expr": "Cast", "to": {"kind": "Float64"},
                             "arg": {"expr": "Var", "name": "x"}}}
                ]
            }]
        }],
        "main": "Caster"
    })
}

// --- Cast: C semantics in the IR simulator -----------------------------------

#[test]
fn cast_simulates_with_c_semantics() {
    let tmp = make_tempdir("cast_sim");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&cast_model()).unwrap()).unwrap();
    let project = ol_ir::load_project(&model).unwrap();
    let mut sim = ol_sim::Sim::new(&project, "Caster").unwrap();
    // 100 + 200 = 300 wraps to 44 in int8; -1 is 255 in uint8.
    let trace = sim.run_csv("x\n100\n-1\n").unwrap();
    let csv = trace.to_csv();
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines[0], "cycle,wrapped,as_u8,widened");
    assert_eq!(lines[1], "0,44,100,100");
    assert_eq!(lines[2], "1,-57,255,-1");
    let _ = std::fs::remove_dir_all(&tmp);
}

// --- Cast: model and generated C agree, cycle by cycle -----------------------

#[test]
fn cast_traces_match_between_ir_and_compiled_c() {
    let tmp = make_tempdir("cast_c");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&cast_model()).unwrap()).unwrap();
    let scen = tmp.join("scenarios");
    std::fs::create_dir_all(&scen).unwrap();
    std::fs::write(scen.join("sweep.csv"), "x\n0\n100\n-1\n127\n-128\n255\n").unwrap();

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
    assert!(out.contains("[PASS] sweep (ir)"), "{out}");
    assert!(out.contains("[PASS] sweep (c )"), "cast C backend: {out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

// --- Cast: typecheck rejects non-numeric operands ----------------------------

#[test]
fn cast_of_a_bool_is_a_type_error() {
    let project: ol_ir::Project = serde_json::from_value(serde_json::json!({
        "name": "bad",
        "packages": [{
            "name": "user",
            "nodes": [{
                "name": "Bad",
                "kind": "Function",
                "inputs": [{"name": "flag", "ty": {"kind": "Bool"}}],
                "outputs": [{"name": "y", "ty": {"kind": "Int8"}}],
                "equations": [{"lhs": ["y"],
                    "rhs": {"expr": "Cast", "to": {"kind": "Int8"},
                            "arg": {"expr": "Var", "name": "flag"}}}]
            }]
        }]
    })).unwrap();
    let report = ol_typecheck::check_project(&project);
    assert!(
        report.diagnostics.iter().any(|d| d.code == "E0093"),
        "expected E0093, got: {:?}",
        report.diagnostics
    );
}

// --- Server harness (workspace mode) -----------------------------------------

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

fn post_ok(port: u16, path: &str, body: &str) {
    let (s, b) = request(port, "POST", path, body).expect(path);
    assert_eq!(s, 200, "{path} failed: {b}");
}

fn get_json(port: u16, path: &str) -> serde_json::Value {
    let (s, body) = request(port, "GET", path, "").expect(path);
    assert_eq!(s, 200, "{path} failed: {body}");
    serde_json::from_str(&body).unwrap()
}

// --- The toolbox catalog and operation drops ----------------------------------

#[test]
fn operations_catalog_has_the_scade_families() {
    let g = start_server_on_workspace("ops_cat");
    let cat = get_json(g.port, "/api/operations");
    let names: Vec<&str> = cat["categories"].as_array().unwrap()
        .iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec![
        "Mathematics", "Comparisons", "Logical", "Structures/Arrays",
        "Time/Statefuls", "Choice", "Bitwise", "Higher Order",
    ]);
    let math = &cat["categories"][0]["items"];
    let ids: Vec<&str> = math.as_array().unwrap()
        .iter().map(|i| i["id"].as_str().unwrap()).collect();
    for id in ["plus", "minus", "divide", "multiply", "modulo", "numeric_cast",
               "square_root", "squared", "cubed", "to_nth_power"] {
        assert!(ids.contains(&id), "Mathematics missing {id}: {ids:?}");
    }
    // square_root is offered but explicitly disabled with a hint, not silent.
    let sqrt = math.as_array().unwrap().iter().find(|i| i["id"] == "square_root").unwrap();
    assert_eq!(sqrt["enabled"], false);
    assert!(sqrt["hint"].as_str().unwrap().contains("roadmap"));
}

#[test]
fn dropping_operations_creates_placed_equations_with_red_pins() {
    let g = start_server_on_workspace("ops_drop");
    let port = g.port;

    post_ok(port, "/api/edit/add_node", r#"{"name":"Calc","kind":"operator"}"#);

    // plus at (96, 48): ghost pins p0_1/p0_2, result local plus0 : int32.
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Calc","op":"plus","x":96.0,"y":48.0}"#);
    let (s, body) = request(port, "GET", "/api/diagram?node=Calc", "").unwrap();
    assert_eq!(s, 200);
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["equations"][0]["body"], "p0_1 + p0_2");
    assert_eq!(d["positions"]["eq0"]["x"], 96.0);
    let ghosts: Vec<&str> = d["ghosts"].as_array().unwrap()
        .iter().map(|g| g["name"].as_str().unwrap()).collect();
    assert!(ghosts.contains(&"p0_1") && ghosts.contains(&"p0_2"), "{ghosts:?}");

    // numeric_cast carries its parameter into both the body and the result type.
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Calc","op":"numeric_cast","param":"float32","x":96.0,"y":120.0}"#);
    let (_, body) = request(port, "GET", "/api/diagram?node=Calc", "").unwrap();
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["equations"][1]["body"], "float32(p1_1)");
    let cast_local = d["locals"].as_array().unwrap()
        .iter().find(|l| l["name"] == "numeric_cast1").unwrap();
    assert_eq!(cast_local["type"]["kind"], "Float32");

    // to_nth_power expands the multiplication n times; n is validated.
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Calc","op":"to_nth_power","param":"3","x":96.0,"y":192.0}"#);
    let (_, body) = request(port, "GET", "/api/diagram?node=Calc", "").unwrap();
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["equations"][2]["body"], "p2_1 * p2_1 * p2_1");
    let (s, _) = request(port, "POST", "/api/edit/add_operation",
        r#"{"node":"Calc","op":"to_nth_power","param":"99","x":0,"y":0}"#).unwrap();
    assert_eq!(s, 400, "n out of range must be rejected");

    // Disabled operations are rejected with their hint.
    let (s, body) = request(port, "POST", "/api/edit/add_operation",
        r#"{"node":"Calc","op":"square_root","x":0,"y":0}"#).unwrap();
    assert_eq!(s, 400);
    assert!(body.contains("roadmap"), "{body}");
}

// --- Constant blocks + SCADE-style symbol descriptors -------------------------

#[test]
fn constant_blocks_are_typed_literals_and_equations_carry_symbols() {
    let g = start_server_on_workspace("const_sym");
    let port = g.port;
    post_ok(port, "/api/edit/add_node", r#"{"name":"Sym","kind":"operator"}"#);

    // A dropped constant is a typed literal source block.
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Sym","op":"constant","param":"2.5","x":40.0,"y":40.0}"#);
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Sym","op":"plus","x":40.0,"y":120.0}"#);
    let (s, body) = request(port, "GET", "/api/diagram?node=Sym", "").unwrap();
    assert_eq!(s, 200, "{body}");
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["equations"][0]["symbol"]["kind"], "const");
    assert_eq!(d["equations"][0]["symbol"]["text"], "2.5");
    let c = d["locals"].as_array().unwrap().iter()
        .find(|l| l["name"] == "constant0").expect("constant local");
    assert_eq!(c["type"]["kind"], "Float64", "2.5 must infer float64");
    // Operations render as compact operator symbols.
    assert_eq!(d["equations"][1]["symbol"]["kind"], "op");
    assert_eq!(d["equations"][1]["symbol"]["text"], "+");

    // Bool literal infers bool; non-literals are rejected.
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Sym","op":"constant","param":"true","x":40.0,"y":200.0}"#);
    let (_, body) = request(port, "GET", "/api/diagram?node=Sym", "").unwrap();
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    let b = d["locals"].as_array().unwrap().iter()
        .find(|l| l["name"] == "constant2").expect("bool constant local");
    assert_eq!(b["type"]["kind"], "Bool");
    let (s, _) = request(port, "POST", "/api/edit/add_operation",
        r#"{"node":"Sym","op":"constant","param":"x + 1","x":0,"y":0}"#).unwrap();
    assert_eq!(s, 400, "non-literal constant must be rejected");

    // The followed-by pattern renders as FBY, SCADE's name for it.
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Sym","op":"init_pre","x":40.0,"y":280.0}"#);
    let (_, body) = request(port, "GET", "/api/diagram?node=Sym", "").unwrap();
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["equations"][3]["symbol"]["text"], "FBY");
}

// --- Compile C-Lite from the GUI ----------------------------------------------

#[test]
fn clite_compile_endpoint_emits_and_builds_an_executable() {
    let g = start_server_on_workspace("compile");
    let out_dir = g.tmp.join("build_out");
    let payload = serde_json::json!({
        "compiler": "auto",
        "out_dir": out_dir.to_str().unwrap(),
    });
    let (s, body) = request(g.port, "POST", "/api/clite/compile", &payload.to_string())
        .expect("compile");
    assert_eq!(s, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["compiled"], true, "compile failed: {}", v["log"]);
    for f in ["openlustre_generated.h", "openlustre_generated.c", "driver.c", "Makefile"] {
        assert!(out_dir.join(f).exists(), "{f} not written");
    }
    let exe = v["exe"].as_str().expect("exe path");
    assert!(PathBuf::from(exe).exists(), "exe missing: {exe}");
}

