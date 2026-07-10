//! FMU export (FMI 2.0 co-simulation): the archive is a valid deterministic
//! zip with modelDescription.xml + sources (+ a host binary when a compiler
//! exists), and — the part that matters — driving the exported FMU through
//! the standard fmi2 API produces exactly the IR simulator's trace,
//! cycle for cycle.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use ol_ir::{Equation, NodeDef, NodeKind, Package, Port, Project, Type};
use ol_sim::{Sim, Value};

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

fn openlustre(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "ol_cli", "--"])
        .args(args)
        .output()
        .expect("cargo run");
    (
        out.status.success(),
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
    )
}

/// A stateful operator with one port of each FMI scalar kind on both sides:
/// Mix(a: bool, x: int32, f: float64) returns (y: int32, g: float64, b: bool)
///   y = x + (0 -> pre y);  g = f * 2.0;  b = a and x > 0;
fn mix_project() -> Project {
    let node = NodeDef {
        name: "Mix".into(),
        kind: NodeKind::Operator,
        inputs: vec![
            Port { name: "a".into(), ty: Type::Bool },
            Port { name: "x".into(), ty: Type::Int32 },
            Port { name: "f".into(), ty: Type::Float64 },
        ],
        outputs: vec![
            Port { name: "y".into(), ty: Type::Int32 },
            Port { name: "g".into(), ty: Type::Float64 },
            Port { name: "b".into(), ty: Type::Bool },
        ],
        locals: vec![],
        equations: vec![
            Equation {
                lhs: vec!["y".into()],
                rhs: ol_stdlib::parse_expr("x + (0 -> pre y)").unwrap(),
            },
            Equation {
                lhs: vec!["g".into()],
                rhs: ol_stdlib::parse_expr("f * 2.0").unwrap(),
            },
            Equation {
                lhs: vec!["b".into()],
                rhs: ol_stdlib::parse_expr("a and x > 0").unwrap(),
            },
        ],
        contract: None,
        diagram: Default::default(),
        probes: vec![],
        requirements: vec![],
        sysml: None,
        generics: vec![],
    };
    Project {
        name: "fmu_mix".into(),
        packages: vec![Package { name: "user".into(), nodes: vec![node], ..Default::default() }],
        main: Some("Mix".into()),
        ..Default::default()
    }
}

const CYCLES: usize = 5;
const A: [bool; CYCLES] = [true, false, true, true, false];
const X: [i64; CYCLES] = [3, 4, -2, 5, 0];
const F: [f64; CYCLES] = [1.5, 2.25, 0.5, 3.0, 1.0];

/// The IR simulator's view of the trace, formatted like the C driver prints.
fn sim_trace(project: &Project) -> Vec<String> {
    let mut sim = Sim::new(project, "Mix").unwrap();
    let mut lines = Vec::new();
    for k in 0..CYCLES {
        let mut inputs = BTreeMap::new();
        inputs.insert("a".to_string(), Value::Bool(A[k]));
        inputs.insert("x".to_string(), Value::Int(X[k]));
        inputs.insert("f".to_string(), Value::Float(F[k]));
        let out = sim.step(&inputs).unwrap();
        let g = match out["g"] {
            Value::Float(v) => v,
            ref other => panic!("g: {other:?}"),
        };
        lines.push(format!(
            "{},{},{}",
            out["y"].as_int().unwrap(),
            g, // exactly-representable values print identically to C's %g
            out["b"].as_bool().unwrap()
        ));
    }
    lines
}

#[test]
fn fmu_archive_is_valid_deterministic_and_describes_the_interface() {
    let tmp = make_tempdir("fmu");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&mix_project()).unwrap()).unwrap();

    let fmu = tmp.join("mix.fmu");
    let src = tmp.join("src");
    let (ok, out) = openlustre(&[
        "fmu", model.to_str().unwrap(),
        "-o", fmu.to_str().unwrap(),
        "--keep-sources", src.to_str().unwrap(),
        "--no-binary",
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("3 inputs, 3 outputs"), "{out}");

    // A real zip: magic, and the stored (uncompressed) entries readable.
    let bytes = std::fs::read(&fmu).unwrap();
    assert_eq!(&bytes[0..4], b"PK\x03\x04");
    let hay = String::from_utf8_lossy(&bytes);
    for needle in ["modelDescription.xml", "sources/fmi2model.c", "sources/openlustre_generated.c"] {
        assert!(hay.contains(needle), "archive missing {needle}");
    }

    // Deterministic: exporting again is byte-identical.
    let fmu2 = tmp.join("mix2.fmu");
    let (ok, out) = openlustre(&[
        "fmu", model.to_str().unwrap(), "-o", fmu2.to_str().unwrap(), "--no-binary",
    ]);
    assert!(ok, "{out}");
    assert_eq!(bytes, std::fs::read(&fmu2).unwrap(), "re-export must be byte-identical");

    // The model description declares the typed interface.
    let xml = std::fs::read_to_string(src.join("modelDescription.xml")).unwrap();
    assert!(xml.contains(r#"fmiVersion="2.0""#), "{xml}");
    assert_eq!(xml.matches(r#"causality="input""#).count(), 3, "{xml}");
    assert_eq!(xml.matches(r#"causality="output""#).count(), 3, "{xml}");
    for needle in [
        r#"<ScalarVariable name="a" valueReference="0" causality="input" variability="discrete"><Boolean start="false"/>"#,
        r#"<ScalarVariable name="x" valueReference="1" causality="input" variability="discrete"><Integer start="0"/>"#,
        r#"<ScalarVariable name="f" valueReference="2" causality="input" variability="discrete"><Real start="0.0"/>"#,
        r#"<CoSimulation modelIdentifier="Mix""#,
    ] {
        assert!(xml.contains(needle), "model description missing:\n{needle}\n--- got:\n{xml}");
    }

    // A compound-typed interface is a loud, actionable error.
    let mut compound = mix_project();
    compound.packages[0].nodes[0].inputs[0].ty = Type::Array { elem: Box::new(Type::Int32), len: 3 };
    let model2 = tmp.join("compound.json");
    std::fs::write(&model2, serde_json::to_string(&compound).unwrap()).unwrap();
    let (ok, out) = openlustre(&[
        "fmu", model2.to_str().unwrap(), "-o", tmp.join("nope.fmu").to_str().unwrap(), "--no-binary",
    ]);
    assert!(!ok, "compound port must be rejected");
    assert!(out.contains("scalar interfaces"), "{out}");

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The real claim: the exported FMU, driven through the standard fmi2 API,
/// produces exactly the IR simulator's trace.
#[test]
fn fmu_behaves_identically_to_the_ir_simulator() {
    let cc = ["/usr/bin/cc", "/usr/bin/gcc", "/usr/bin/clang"]
        .iter()
        .find(|p| std::path::Path::new(p).exists());
    let Some(cc) = cc else { return }; // no host compiler: nothing to drive

    let project = mix_project();
    let tmp = make_tempdir("fmu_equiv");
    let model = tmp.join("model.json");
    std::fs::write(&model, serde_json::to_string_pretty(&project).unwrap()).unwrap();
    let src = tmp.join("src");
    let (ok, out) = openlustre(&[
        "fmu", model.to_str().unwrap(),
        "-o", tmp.join("mix.fmu").to_str().unwrap(),
        "--keep-sources", src.to_str().unwrap(),
        "--no-binary",
    ]);
    assert!(ok, "{out}");

    // A master-in-miniature: instantiate, init, then per cycle set inputs by
    // value reference, DoStep, and read the outputs back.
    let fmt_bool = |b: bool| if b { "1" } else { "0" };
    let driver = format!(
        r#"#include "fmi2model.c"
#include <stdio.h>
int main(void) {{
  fmi2Component c = fmi2Instantiate("mix", fmi2CoSimulation, "guid", "", 0, 0, 0);
  if (!c) return 1;
  fmi2EnterInitializationMode(c);
  fmi2ExitInitializationMode(c);
  const fmi2Boolean A[{n}] = {{{a}}};
  const fmi2Integer X[{n}] = {{{x}}};
  const fmi2Real F[{n}] = {{{f}}};
  const fmi2ValueReference va = 0, vx = 1, vf = 2, vy = 3, vg = 4, vb = 5;
  for (int k = 0; k < {n}; k++) {{
    if (fmi2SetBoolean(c, &va, 1, &A[k]) != fmi2OK) return 2;
    if (fmi2SetInteger(c, &vx, 1, &X[k]) != fmi2OK) return 3;
    if (fmi2SetReal(c, &vf, 1, &F[k]) != fmi2OK) return 4;
    if (fmi2DoStep(c, (fmi2Real)k, 1.0, 1) != fmi2OK) return 5;
    fmi2Integer y; fmi2Real g; fmi2Boolean b;
    fmi2GetInteger(c, &vy, 1, &y);
    fmi2GetReal(c, &vg, 1, &g);
    fmi2GetBoolean(c, &vb, 1, &b);
    printf("%d,%g,%s\n", y, g, b ? "true" : "false");
  }}
  /* An out-of-range value reference is a loud error, not a silent write. */
  fmi2ValueReference bad = 99; fmi2Real v = 0;
  if (fmi2GetReal(c, &bad, 1, &v) != fmi2Error) return 6;
  fmi2FreeInstance(c);
  return 0;
}}
"#,
        n = CYCLES,
        a = A.iter().map(|&b| fmt_bool(b)).collect::<Vec<_>>().join(","),
        x = X.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","),
        f = F.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>().join(","),
    );
    std::fs::write(src.join("sources/driver.c"), driver).unwrap();
    let exe = tmp.join("driver");
    let status = Command::new(cc)
        .arg("-o")
        .arg(&exe)
        .arg(src.join("sources/driver.c"))
        .arg(src.join("sources/openlustre_generated.c"))
        .arg("-I")
        .arg(src.join("sources"))
        .arg("-lm")
        .status()
        .unwrap();
    assert!(status.success(), "driver must compile");
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "driver exit {:?}", out.status.code());
    let fmu_lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect();

    assert_eq!(fmu_lines, sim_trace(&project), "FMU trace == IR simulator trace");
    let _ = std::fs::remove_dir_all(&tmp);
}
