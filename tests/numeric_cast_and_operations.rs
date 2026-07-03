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
        "Mathematics", "Float Math", "Comparisons", "Logical", "Structures/Arrays",
        "Time/Statefuls", "Choice", "Bitwise", "Higher Order",
    ]);
    let math = &cat["categories"][0]["items"];
    let ids: Vec<&str> = math.as_array().unwrap()
        .iter().map(|i| i["id"].as_str().unwrap()).collect();
    for id in ["plus", "minus", "divide", "multiply", "modulo", "numeric_cast",
               "square_root", "squared", "cubed", "to_nth_power"] {
        assert!(ids.contains(&id), "Mathematics missing {id}: {ids:?}");
    }
    // square_root is enabled (float intrinsics landed) with a float64 contract.
    let sqrt = math.as_array().unwrap().iter().find(|i| i["id"] == "square_root").unwrap();
    assert_eq!(sqrt["enabled"], true);
    assert_eq!(sqrt["output"], "float64");
    // The Float Math family carries the whole <math.h> double set.
    let fam = &cat["categories"][1]["items"];
    let ids: Vec<&str> = fam.as_array().unwrap()
        .iter().map(|i| i["id"].as_str().unwrap()).collect();
    for id in ["sin", "cos", "tan", "asin", "acos", "atan", "atan2", "exp", "log",
               "log10", "pow", "floor", "ceil", "round", "abs", "min", "max"] {
        assert!(ids.contains(&id), "Float Math missing {id}: {ids:?}");
    }
    let atan2 = fam.as_array().unwrap().iter().find(|i| i["id"] == "atan2").unwrap();
    assert_eq!(atan2["pins"], 2);
    assert_eq!(atan2["signature"], "float64, float64 → float64");
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

    // square_root drops as a sqrt intrinsic with a float64 result local.
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Calc","op":"square_root","x":0,"y":264.0}"#);
    let (_, body) = request(port, "GET", "/api/diagram?node=Calc", "").unwrap();
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["equations"][3]["body"], "sqrt(p3_1)");
    let sq_local = d["locals"].as_array().unwrap()
        .iter().find(|l| l["name"] == "square_root3").unwrap();
    assert_eq!(sq_local["type"]["kind"], "Float64");
    assert_eq!(d["equations"][3]["symbol"]["kind"], "op");
    assert_eq!(d["equations"][3]["symbol"]["text"], "sqrt");

    // Two-argument Float Math drops join both pins.
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Calc","op":"atan2","x":0,"y":336.0}"#);
    let (_, body) = request(port, "GET", "/api/diagram?node=Calc", "").unwrap();
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["equations"][4]["body"], "atan2(p4_1, p4_2)");
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

// --- Variadic operations: pin contracts + adjustable input counts -------------

#[test]
fn variadic_operations_declare_contracts_and_resize_their_pins() {
    let g = start_server_on_workspace("ops_nary");
    let port = g.port;

    // The catalog publishes each operation's connection-point contract so
    // the GUI can guide the engineer: and = bool pins in, one bool out.
    let cat = get_json(port, "/api/operations");
    let logical = cat["categories"].as_array().unwrap().iter()
        .find(|c| c["name"] == "Logical").unwrap();
    let and = logical["items"].as_array().unwrap().iter()
        .find(|i| i["id"] == "and").unwrap();
    assert_eq!(and["variadic"], true);
    assert_eq!(and["min_pins"], 2);
    assert_eq!(and["max_pins"], 12);
    assert_eq!(and["inputs"], serde_json::json!(["bool", "bool"]));
    assert_eq!(and["output"], "bool");
    assert_eq!(and["signature"], "bool × 2…12 → bool");
    let not = logical["items"].as_array().unwrap().iter()
        .find(|i| i["id"] == "not").unwrap();
    assert_eq!(not["variadic"], false);
    assert_eq!(not["signature"], "bool → bool");

    post_ok(port, "/api/edit/add_node", r#"{"name":"Gate","kind":"operator"}"#);

    // A variadic operation may be dropped with extra pins right away, and
    // its result local is typed by the contract (bool for `and`).
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Gate","op":"and","inputs":4,"x":40.0,"y":40.0}"#);
    let d = get_json(port, "/api/diagram?node=Gate");
    assert_eq!(d["equations"][0]["body"], "p0_1 and p0_2 and p0_3 and p0_4");
    assert_eq!(d["equations"][0]["nary"]["op"], "and");
    assert_eq!(d["equations"][0]["nary"]["inputs"], 4);
    assert_eq!(d["equations"][0]["nary"]["max"], 12);
    let and_local = d["locals"].as_array().unwrap().iter()
        .find(|l| l["name"] == "and0").expect("result local");
    assert_eq!(and_local["type"]["kind"], "Bool", "and must produce a bool");

    // Out-of-range and fixed-arity drops are rejected, with the reason.
    let (s, _) = request(port, "POST", "/api/edit/add_operation",
        r#"{"node":"Gate","op":"and","inputs":13,"x":0,"y":0}"#).unwrap();
    assert_eq!(s, 400, "13 pins is past the sanity ceiling");
    let (s, body) = request(port, "POST", "/api/edit/add_operation",
        r#"{"node":"Gate","op":"not","inputs":3,"x":0,"y":0}"#).unwrap();
    assert_eq!(s, 400, "`not` has a fixed contract");
    assert!(body.contains("fixed number of inputs"), "{body}");

    // Bind the first pin to a real input, then grow in place: the wiring
    // survives and the new pins arrive as fresh red ghosts.
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Gate","name":"enable","side":"input","type":"bool"}"#);
    post_ok(port, "/api/edit/update_equation",
        r#"{"node":"Gate","index":0,"lhs":"and0","body":"enable and p0_2 and p0_3 and p0_4"}"#);
    post_ok(port, "/api/edit/set_operation_inputs",
        r#"{"node":"Gate","index":0,"inputs":6}"#);
    let d = get_json(port, "/api/diagram?node=Gate");
    assert_eq!(d["equations"][0]["body"],
        "enable and p0_2 and p0_3 and p0_4 and p0_5 and p0_6");
    let ghosts: Vec<&str> = d["ghosts"].as_array().unwrap()
        .iter().map(|g| g["name"].as_str().unwrap()).collect();
    assert!(ghosts.contains(&"p0_5") && ghosts.contains(&"p0_6"), "{ghosts:?}");

    // Shrinking drops the trailing pins only; the bound input stays.
    post_ok(port, "/api/edit/set_operation_inputs",
        r#"{"node":"Gate","index":0,"inputs":2}"#);
    let d = get_json(port, "/api/diagram?node=Gate");
    assert_eq!(d["equations"][0]["body"], "enable and p0_2");
    assert_eq!(d["equations"][0]["nary"]["inputs"], 2);

    // Resizes are journaled edits like any other: undo restores six pins.
    post_ok(port, "/api/edit/undo", "");
    let d = get_json(port, "/api/diagram?node=Gate");
    assert_eq!(d["equations"][0]["body"],
        "enable and p0_2 and p0_3 and p0_4 and p0_5 and p0_6");

    // Out-of-range resizes and fixed-shape equations are refused.
    let (s, _) = request(port, "POST", "/api/edit/set_operation_inputs",
        r#"{"node":"Gate","index":0,"inputs":1}"#).unwrap();
    assert_eq!(s, 400, "one pin is below the minimum of 2");
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Gate","op":"numeric_cast","param":"int16","x":40.0,"y":160.0}"#);
    let (s, body) = request(port, "POST", "/api/edit/set_operation_inputs",
        r#"{"node":"Gate","index":1,"inputs":3}"#).unwrap();
    assert_eq!(s, 400, "{body}");
    assert!(body.contains("fixed number of inputs"), "{body}");
}

// --- Clock blocks in the toolbox -----------------------------------------------

#[test]
fn when_and_merge_drop_from_the_time_family() {
    let g = start_server_on_workspace("ops_clock");
    let port = g.port;

    let cat = get_json(port, "/api/operations");
    let time = cat["categories"].as_array().unwrap().iter()
        .find(|c| c["name"] == "Time/Statefuls").unwrap();
    let ids: Vec<&str> = time["items"].as_array().unwrap()
        .iter().map(|i| i["id"].as_str().unwrap()).collect();
    for id in ["when", "when_not", "merge"] {
        assert!(ids.contains(&id), "Time/Statefuls missing {id}: {ids:?}");
    }

    post_ok(port, "/api/edit/add_node", r#"{"name":"Clocked","kind":"operator"}"#);
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Clocked","op":"when","x":40.0,"y":40.0}"#);
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Clocked","op":"merge","x":40.0,"y":120.0}"#);
    let d = get_json(port, "/api/diagram?node=Clocked");
    assert_eq!(d["equations"][0]["body"], "p0_1 when p0_2");
    assert_eq!(d["equations"][0]["symbol"]["text"], "WHEN");
    assert_eq!(d["equations"][1]["body"], "merge(p1_1, p1_2, p1_3)");
    assert_eq!(d["equations"][1]["symbol"]["text"], "MERGE");
    // All three merge pins are red unbound ghosts awaiting wires.
    let ghosts: Vec<&str> = d["ghosts"].as_array().unwrap()
        .iter().map(|g| g["name"].as_str().unwrap()).collect();
    for pin in ["p1_1", "p1_2", "p1_3"] {
        assert!(ghosts.contains(&pin), "{ghosts:?}");
    }
}

// --- map/fold drop from the Higher Order family --------------------------------

#[test]
fn map_and_fold_drop_with_a_typed_result() {
    let g = start_server_on_workspace("ops_iter");
    let port = g.port;

    // The Higher Order family now offers map/fold as enabled blocks.
    let cat = get_json(port, "/api/operations");
    let ho = cat["categories"].as_array().unwrap().iter()
        .find(|c| c["name"] == "Higher Order").unwrap();
    for id in ["map", "fold"] {
        let item = ho["items"].as_array().unwrap().iter()
            .find(|i| i["id"] == id).unwrap();
        assert_eq!(item["enabled"], true, "{id} should be enabled");
    }

    // A stateless function to iterate: Scale(x) = x * 3.
    post_ok(port, "/api/edit/add_node", r#"{"name":"Scale","kind":"function"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Scale","name":"x","side":"input","type":"int32"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"Scale","name":"y","side":"output","type":"int32"}"#);
    post_ok(port, "/api/edit/add_equation", r#"{"node":"Scale","lhs":"y","body":"x * 3"}"#);

    post_ok(port, "/api/edit/add_node", r#"{"name":"Vec","kind":"operator"}"#);

    // Drop map(Scale) with length 4: body is map(Scale, p0_1), result int32[4].
    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Vec","op":"map","param":"Scale:4","x":40.0,"y":40.0}"#);
    let d = get_json(port, "/api/diagram?node=Vec");
    assert_eq!(d["equations"][0]["body"], "map(Scale, p0_1)");
    assert_eq!(d["equations"][0]["symbol"]["text"], "map(Scale)");
    let map_local = d["locals"].as_array().unwrap().iter()
        .find(|l| l["name"] == "map0").expect("map result local");
    assert_eq!(map_local["type"]["kind"], "Array");
    assert_eq!(map_local["type"]["len"], 4);
    assert_eq!(map_local["type"]["elem"]["kind"], "Int32");

    // A reducing function AddF(acc, e) = acc + e, then fold(AddF).
    post_ok(port, "/api/edit/add_node", r#"{"name":"AddF","kind":"function"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"AddF","name":"acc","side":"input","type":"int32"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"AddF","name":"e","side":"input","type":"int32"}"#);
    post_ok(port, "/api/edit/add_port",
        r#"{"node":"AddF","name":"s","side":"output","type":"int32"}"#);
    post_ok(port, "/api/edit/add_equation", r#"{"node":"AddF","lhs":"s","body":"acc + e"}"#);

    post_ok(port, "/api/edit/add_operation",
        r#"{"node":"Vec","op":"fold","param":"AddF","x":40.0,"y":160.0}"#);
    let d = get_json(port, "/api/diagram?node=Vec");
    assert_eq!(d["equations"][1]["body"], "fold(AddF, p1_1, p1_2)");
    let fold_local = d["locals"].as_array().unwrap().iter()
        .find(|l| l["name"] == "fold1").expect("fold result local");
    assert_eq!(fold_local["type"]["kind"], "Int32", "fold yields the accumulator type");

    // Iterating a stateful operator is rejected on drop, with the reason.
    post_ok(port, "/api/edit/add_node", r#"{"name":"Counter","kind":"operator"}"#);
    let (s, body) = request(port, "POST", "/api/edit/add_operation",
        r#"{"node":"Vec","op":"map","param":"Counter:4","x":0,"y":0}"#).unwrap();
    assert_eq!(s, 400);
    assert!(body.contains("stateless"), "{body}");
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

