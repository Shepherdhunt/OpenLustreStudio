//! `openlustre fmu` — export an operator as an **FMI 2.0 co-simulation FMU**:
//! the co-simulation entry ticket into the broader MBSE world. The FMU wraps
//! the same generated C every other backend uses (`ol_clite_emit`), so the
//! behavior inside a simulator is the verified C-Lite behavior; one
//! `fmi2DoStep` advances the Lustre program by exactly one cycle,
//! independent of the communication step size the master passes.
//!
//! The archive is fully deterministic (fixed zip timestamps, a GUID hashed
//! from the content), so the .fmu can live under configuration management
//! next to the model. Layout:
//!
//! ```text
//! modelDescription.xml
//! sources/openlustre_generated.{h,c}   the sliced model's C
//! sources/fmi2model.c                  self-contained FMI 2.0 glue
//! binaries/linux64/<id>.so             when a host C compiler is available
//! ```
//!
//! Only scalar interfaces export (bool / integers / floats); array- or
//! record-typed ports are a loud error — decompose or wrap the operator.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Context, Result};
use ol_ir::{NodeKind, Project, Type};

pub struct FmuOptions<'a> {
    /// Root operator; defaults to the project's main.
    pub node: Option<&'a str>,
    /// Also write the archive's contents as a plain directory tree here.
    pub keep_sources: Option<&'a Path>,
    /// Skip the host-compiler shared-library build.
    pub no_binary: bool,
}

pub fn export(project: &Project, out: &Path, opts: &FmuOptions) -> Result<()> {
    let root = match opts.node.or(project.main.as_deref()) {
        Some(r) => r.to_string(),
        None => bail!("no operator selected: pass --node or set a main operator"),
    };
    // State machines must lower into their operators before the slice —
    // the emitted C sees only dataflow nodes.
    let mut lowered = project.clone();
    lowered.lower_state_machines().map_err(|errs| {
        anyhow::anyhow!(errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "))
    })?;
    let sliced = lowered.slice_for_root(&root).map_err(|e| anyhow::anyhow!(e))?;
    let node = sliced
        .find_node(&root)
        .with_context(|| format!("operator `{root}` not found"))?
        .clone();

    // FMI scalar variables only: every port must map to Real/Integer/Boolean.
    for p in node.inputs.iter().chain(node.outputs.iter()) {
        if fmi_type(&p.ty).is_none() {
            bail!(
                "port `{}` of `{root}` has type `{}` — FMU export supports scalar \
                 interfaces (bool, integers, floats); wrap or decompose the operator",
                p.name,
                p.ty.lustre_name()
            );
        }
    }

    let bundle = ol_clite_emit::emit_project(&sliced);
    let ident = c_safe(&root);
    let glue = fmi_glue(&node, &ident);
    // The GUID pins content identity: a model change changes the GUID, an
    // unchanged model re-exports byte-identically.
    let guid = format!("OL-{:016x}", fnv1a(&[&bundle.header, &bundle.source, &glue]));
    let xml = model_description(&node, &ident, &guid);

    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("modelDescription.xml".into(), xml.into_bytes()),
        ("sources/openlustre_generated.h".into(), bundle.header.into_bytes()),
        ("sources/openlustre_generated.c".into(), bundle.source.into_bytes()),
        ("sources/fmi2model.c".into(), glue.into_bytes()),
    ];

    // Host binary (best effort): a source-only FMU is still a valid FMU.
    let mut binary_note = "no host C compiler found — source-only FMU".to_string();
    if !opts.no_binary {
        if let Some(cc) = find_compiler() {
            let tmp = tempdir(out)?;
            for (name, bytes) in &files {
                if let Some(base) = name.strip_prefix("sources/") {
                    std::fs::write(tmp.join(base), bytes)?;
                }
            }
            let so = tmp.join(format!("{ident}.so"));
            let status = std::process::Command::new(&cc)
                .args(["-shared", "-fPIC", "-O2", "-o"])
                .arg(&so)
                .arg(tmp.join("fmi2model.c"))
                .arg(tmp.join("openlustre_generated.c"))
                .arg("-lm")
                .status()
                .with_context(|| format!("running {cc}"))?;
            if !status.success() {
                bail!("{cc} failed to build the FMU shared library");
            }
            files.push((format!("binaries/linux64/{ident}.so"), std::fs::read(&so)?));
            binary_note = format!("binaries/linux64/{ident}.so built with {cc}");
            let _ = std::fs::remove_dir_all(&tmp);
        }
    } else {
        binary_note = "binary skipped (--no-binary)".into();
    }

    if let Some(dir) = opts.keep_sources {
        for (name, bytes) in &files {
            let p = dir.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, bytes).with_context(|| format!("writing {}", p.display()))?;
        }
    }

    let archive = zip_store(&files);
    std::fs::write(out, archive).with_context(|| format!("writing {}", out.display()))?;
    println!(
        "FMU written to {} ({root}, guid {guid}, {} inputs, {} outputs; {binary_note})",
        out.display(),
        node.inputs.len(),
        node.outputs.len()
    );
    Ok(())
}

fn tempdir(near: &Path) -> Result<std::path::PathBuf> {
    let dir = near
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("__fmu_build_tmp");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn find_compiler() -> Option<String> {
    for cc in ["cc", "gcc", "clang"] {
        let ok = std::process::Command::new(cc)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(cc.to_string());
        }
    }
    None
}

/// FMI scalar kind for a Lustre type, or `None` for compound types.
#[derive(Clone, Copy, PartialEq)]
enum Fmi {
    Real,
    Integer,
    Boolean,
}

fn fmi_type(ty: &Type) -> Option<Fmi> {
    match ty {
        Type::Bool => Some(Fmi::Boolean),
        Type::Float32 | Type::Float64 => Some(Fmi::Real),
        Type::Int8
        | Type::Int16
        | Type::Int32
        | Type::Int64
        | Type::Uint8
        | Type::Uint16
        | Type::Uint32
        | Type::Uint64 => Some(Fmi::Integer),
        Type::Char | Type::Array { .. } | Type::Named { .. } => None,
    }
}

fn c_safe(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

fn xml_esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn model_description(node: &ol_ir::NodeDef, ident: &str, guid: &str) -> String {
    let mut xml = String::new();
    let _ = writeln!(xml, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        xml,
        r#"<fmiModelDescription fmiVersion="2.0" modelName="{}" guid="{guid}" generationTool="OpenLustre Studio" numberOfEventIndicators="0" description="Lustre operator {}; one fmi2DoStep = one synchronous cycle">"#,
        xml_esc(&node.name),
        xml_esc(&node.name)
    );
    let _ = writeln!(
        xml,
        r#"  <CoSimulation modelIdentifier="{ident}" canHandleVariableCommunicationStepSize="false" canBeInstantiatedOnlyOncePerProcess="false"/>"#
    );
    let _ = writeln!(xml, "  <ModelVariables>");
    let mut vr = 0usize;
    for p in &node.inputs {
        let (tag, start) = match fmi_type(&p.ty).expect("checked scalar") {
            Fmi::Real => ("Real", r#" start="0.0""#),
            Fmi::Integer => ("Integer", r#" start="0""#),
            Fmi::Boolean => ("Boolean", r#" start="false""#),
        };
        let _ = writeln!(
            xml,
            r#"    <ScalarVariable name="{}" valueReference="{vr}" causality="input" variability="discrete"><{tag}{start}/></ScalarVariable>"#,
            xml_esc(&p.name)
        );
        vr += 1;
    }
    for p in &node.outputs {
        let tag = match fmi_type(&p.ty).expect("checked scalar") {
            Fmi::Real => "Real",
            Fmi::Integer => "Integer",
            Fmi::Boolean => "Boolean",
        };
        let _ = writeln!(
            xml,
            r#"    <ScalarVariable name="{}" valueReference="{vr}" causality="output" variability="discrete" initial="calculated"><{tag}/></ScalarVariable>"#,
            xml_esc(&p.name)
        );
        vr += 1;
    }
    let _ = writeln!(xml, "  </ModelVariables>");
    let _ = writeln!(xml, "  <ModelStructure>");
    let first_out = node.inputs.len();
    if !node.outputs.is_empty() {
        let _ = writeln!(xml, "    <Outputs>");
        for k in 0..node.outputs.len() {
            let _ = writeln!(xml, r#"      <Unknown index="{}"/>"#, first_out + k + 1);
        }
        let _ = writeln!(xml, "    </Outputs>");
        let _ = writeln!(xml, "    <InitialUnknowns>");
        for k in 0..node.outputs.len() {
            let _ = writeln!(xml, r#"      <Unknown index="{}"/>"#, first_out + k + 1);
        }
        let _ = writeln!(xml, "    </InitialUnknowns>");
    }
    let _ = writeln!(xml, "  </ModelStructure>");
    let _ = writeln!(xml, "</fmiModelDescription>");
    xml
}

/// The FMI 2.0 co-simulation glue over the generated `_Input/_Output/_State`
/// structs. Self-contained: the standard FMI type and function signatures
/// are declared inline, so the file compiles without external headers.
fn fmi_glue(node: &ol_ir::NodeDef, ident: &str) -> String {
    let n = &node.name;
    let stateful = node.kind != NodeKind::Function;
    let mut c = String::new();
    let _ = writeln!(
        c,
        r#"/* FMI 2.0 co-simulation glue for the Lustre operator `{n}`.
 * Generated by OpenLustre Studio. One fmi2DoStep = one synchronous cycle.
 * Self-contained: standard FMI 2.0 types and signatures declared inline. */
#include <stdlib.h>
#include <string.h>
#include "openlustre_generated.h"

typedef void* fmi2Component;
typedef void* fmi2ComponentEnvironment;
typedef void* fmi2FMUstate;
typedef unsigned int fmi2ValueReference;
typedef double fmi2Real;
typedef int fmi2Integer;
typedef int fmi2Boolean;
typedef char fmi2Char;
typedef const fmi2Char* fmi2String;
typedef char fmi2Byte;
typedef enum {{ fmi2OK, fmi2Warning, fmi2Discard, fmi2Error, fmi2Fatal, fmi2Pending }} fmi2Status;
typedef enum {{ fmi2ModelExchange, fmi2CoSimulation }} fmi2Type;
typedef enum {{ fmi2DoStepStatus, fmi2PendingStatus, fmi2LastSuccessfulTime, fmi2Terminated }} fmi2StatusKind;
typedef struct {{
  void  (*logger)(fmi2ComponentEnvironment, fmi2String, fmi2Status, fmi2String, fmi2String, ...);
  void* (*allocateMemory)(size_t, size_t);
  void  (*freeMemory)(void*);
  void  (*stepFinished)(fmi2ComponentEnvironment, fmi2Status);
  fmi2ComponentEnvironment componentEnvironment;
}} fmi2CallbackFunctions;

#if defined(_WIN32)
#define FMI2_EXPORT __declspec(dllexport)
#else
#define FMI2_EXPORT
#endif

typedef struct {{
  {n}_Input in;
  {n}_Output out;"#
    );
    if stateful {
        let _ = writeln!(c, "  {n}_State st;");
    }
    let _ = writeln!(
        c,
        r#"}} Ol_{ident}_Fmu;

FMI2_EXPORT const char* fmi2GetTypesPlatform(void) {{ return "default"; }}
FMI2_EXPORT const char* fmi2GetVersion(void) {{ return "2.0"; }}
FMI2_EXPORT fmi2Status fmi2SetDebugLogging(fmi2Component c, fmi2Boolean on, size_t n, const fmi2String cat[]) {{
  (void)c; (void)on; (void)n; (void)cat; return fmi2OK;
}}

FMI2_EXPORT fmi2Component fmi2Instantiate(fmi2String instanceName, fmi2Type fmuType,
    fmi2String guid, fmi2String resourceLocation, const fmi2CallbackFunctions* functions,
    fmi2Boolean visible, fmi2Boolean loggingOn) {{
  (void)instanceName; (void)fmuType; (void)guid; (void)resourceLocation;
  (void)functions; (void)visible; (void)loggingOn;
  Ol_{ident}_Fmu* m = (Ol_{ident}_Fmu*)calloc(1, sizeof(Ol_{ident}_Fmu));
  if (!m) return NULL;"#
    );
    if stateful {
        let _ = writeln!(c, "  {n}_init(&m->st);");
    }
    let _ = writeln!(
        c,
        r#"  return (fmi2Component)m;
}}

FMI2_EXPORT void fmi2FreeInstance(fmi2Component c) {{ free(c); }}
FMI2_EXPORT fmi2Status fmi2SetupExperiment(fmi2Component c, fmi2Boolean tolDefined, fmi2Real tol,
    fmi2Real startTime, fmi2Boolean stopDefined, fmi2Real stopTime) {{
  (void)c; (void)tolDefined; (void)tol; (void)startTime; (void)stopDefined; (void)stopTime;
  return fmi2OK;
}}
FMI2_EXPORT fmi2Status fmi2EnterInitializationMode(fmi2Component c) {{ (void)c; return fmi2OK; }}
FMI2_EXPORT fmi2Status fmi2ExitInitializationMode(fmi2Component c) {{ (void)c; return fmi2OK; }}
FMI2_EXPORT fmi2Status fmi2Terminate(fmi2Component c) {{ (void)c; return fmi2OK; }}
FMI2_EXPORT fmi2Status fmi2Reset(fmi2Component c) {{
  Ol_{ident}_Fmu* m = (Ol_{ident}_Fmu*)c;
  memset(&m->in, 0, sizeof(m->in));
  memset(&m->out, 0, sizeof(m->out));"#
    );
    if stateful {
        let _ = writeln!(c, "  {n}_init(&m->st);");
    }
    let _ = writeln!(c, "  return fmi2OK;\n}}");

    // Typed set/get switches over the value references (inputs then outputs).
    let all: Vec<(usize, &ol_ir::Port, bool)> = node
        .inputs
        .iter()
        .enumerate()
        .map(|(i, p)| (i, p, true))
        .chain(
            node.outputs
                .iter()
                .enumerate()
                .map(|(i, p)| (node.inputs.len() + i, p, false)),
        )
        .collect();
    let field = |p: &ol_ir::Port, is_input: bool| {
        format!("m->{}.{}", if is_input { "in" } else { "out" }, c_safe(&p.name))
    };
    for (fname, kind, cty, get_cast) in [
        ("Real", Fmi::Real, "fmi2Real", "(fmi2Real)"),
        ("Integer", Fmi::Integer, "fmi2Integer", "(fmi2Integer)"),
        ("Boolean", Fmi::Boolean, "fmi2Boolean", "(fmi2Boolean)"),
    ] {
        // Setters (inputs only).
        let _ = writeln!(
            c,
            "\nFMI2_EXPORT fmi2Status fmi2Set{fname}(fmi2Component c, const fmi2ValueReference vr[], size_t nvr, const {cty} value[]) {{\n  Ol_{ident}_Fmu* m = (Ol_{ident}_Fmu*)c;\n  (void)m;\n  for (size_t i = 0; i < nvr; i++) {{\n    switch (vr[i]) {{"
        );
        for (vr, p, is_input) in all.iter().filter(|(_, p, i)| *i && fmi_type(&p.ty) == Some(kind)) {
            let cast = c_field_cast(&p.ty);
            let _ = writeln!(c, "      case {vr}: {} = {cast}value[i]; break;", field(p, *is_input));
        }
        let _ = writeln!(c, "      default: return fmi2Error;\n    }}\n  }}\n  return fmi2OK;\n}}");
        // Getters (inputs and outputs).
        let _ = writeln!(
            c,
            "\nFMI2_EXPORT fmi2Status fmi2Get{fname}(fmi2Component c, const fmi2ValueReference vr[], size_t nvr, {cty} value[]) {{\n  Ol_{ident}_Fmu* m = (Ol_{ident}_Fmu*)c;\n  (void)m;\n  for (size_t i = 0; i < nvr; i++) {{\n    switch (vr[i]) {{"
        );
        for (vr, p, is_input) in all.iter().filter(|(_, p, _)| fmi_type(&p.ty) == Some(kind)) {
            let _ = writeln!(c, "      case {vr}: value[i] = {get_cast}{}; break;", field(p, *is_input));
        }
        let _ = writeln!(c, "      default: return fmi2Error;\n    }}\n  }}\n  return fmi2OK;\n}}");
    }

    let step_call = if stateful {
        format!("{n}_step(&m->st, &m->in, &m->out);")
    } else {
        format!("{n}_step(&m->in, &m->out);")
    };
    let _ = writeln!(
        c,
        r#"
FMI2_EXPORT fmi2Status fmi2SetString(fmi2Component c, const fmi2ValueReference vr[], size_t nvr, const fmi2String value[]) {{
  (void)c; (void)vr; (void)value; return nvr == 0 ? fmi2OK : fmi2Error;
}}
FMI2_EXPORT fmi2Status fmi2GetString(fmi2Component c, const fmi2ValueReference vr[], size_t nvr, fmi2String value[]) {{
  (void)c; (void)vr; (void)value; return nvr == 0 ? fmi2OK : fmi2Error;
}}

/* One communication step = one synchronous Lustre cycle. */
FMI2_EXPORT fmi2Status fmi2DoStep(fmi2Component c, fmi2Real currentCommunicationPoint,
    fmi2Real communicationStepSize, fmi2Boolean noSetFMUStatePriorToCurrentPoint) {{
  (void)currentCommunicationPoint; (void)communicationStepSize; (void)noSetFMUStatePriorToCurrentPoint;
  Ol_{ident}_Fmu* m = (Ol_{ident}_Fmu*)c;
  {step_call}
  return fmi2OK;
}}
FMI2_EXPORT fmi2Status fmi2CancelStep(fmi2Component c) {{ (void)c; return fmi2Error; }}

/* Optional capabilities this FMU does not provide. */
FMI2_EXPORT fmi2Status fmi2GetFMUstate(fmi2Component c, fmi2FMUstate* s) {{ (void)c; (void)s; return fmi2Error; }}
FMI2_EXPORT fmi2Status fmi2SetFMUstate(fmi2Component c, fmi2FMUstate s) {{ (void)c; (void)s; return fmi2Error; }}
FMI2_EXPORT fmi2Status fmi2FreeFMUstate(fmi2Component c, fmi2FMUstate* s) {{ (void)c; (void)s; return fmi2Error; }}
FMI2_EXPORT fmi2Status fmi2SerializedFMUstateSize(fmi2Component c, fmi2FMUstate s, size_t* n) {{ (void)c; (void)s; (void)n; return fmi2Error; }}
FMI2_EXPORT fmi2Status fmi2SerializeFMUstate(fmi2Component c, fmi2FMUstate s, fmi2Byte b[], size_t n) {{ (void)c; (void)s; (void)b; (void)n; return fmi2Error; }}
FMI2_EXPORT fmi2Status fmi2DeSerializeFMUstate(fmi2Component c, const fmi2Byte b[], size_t n, fmi2FMUstate* s) {{ (void)c; (void)b; (void)n; (void)s; return fmi2Error; }}
FMI2_EXPORT fmi2Status fmi2GetDirectionalDerivative(fmi2Component c, const fmi2ValueReference u[], size_t nu,
    const fmi2ValueReference z[], size_t nz, const fmi2Real dv[], fmi2Real out[]) {{
  (void)c; (void)u; (void)nu; (void)z; (void)nz; (void)dv; (void)out; return fmi2Error;
}}
FMI2_EXPORT fmi2Status fmi2GetStatus(fmi2Component c, const fmi2StatusKind k, fmi2Status* v) {{ (void)c; (void)k; (void)v; return fmi2Discard; }}
FMI2_EXPORT fmi2Status fmi2GetRealStatus(fmi2Component c, const fmi2StatusKind k, fmi2Real* v) {{ (void)c; (void)k; (void)v; return fmi2Discard; }}
FMI2_EXPORT fmi2Status fmi2GetIntegerStatus(fmi2Component c, const fmi2StatusKind k, fmi2Integer* v) {{ (void)c; (void)k; (void)v; return fmi2Discard; }}
FMI2_EXPORT fmi2Status fmi2GetBooleanStatus(fmi2Component c, const fmi2StatusKind k, fmi2Boolean* v) {{ (void)c; (void)k; (void)v; return fmi2Discard; }}
FMI2_EXPORT fmi2Status fmi2GetStringStatus(fmi2Component c, const fmi2StatusKind k, fmi2String* v) {{ (void)c; (void)k; (void)v; return fmi2Discard; }}"#
    );
    c
}

/// The C cast that narrows an FMI scalar back into the struct field's type.
fn c_field_cast(ty: &Type) -> &'static str {
    match ty {
        Type::Bool => "(bool)!!",
        Type::Float32 => "(float)",
        Type::Float64 => "(double)",
        Type::Int8 => "(int8_t)",
        Type::Int16 => "(int16_t)",
        Type::Int32 => "(int32_t)",
        Type::Int64 => "(int64_t)",
        Type::Uint8 => "(uint8_t)",
        Type::Uint16 => "(uint16_t)",
        Type::Uint32 => "(uint32_t)",
        Type::Uint64 => "(uint64_t)",
        _ => "",
    }
}

// --- Deterministic store-only zip -------------------------------------------

fn fnv1a(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in parts {
        for b in part.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    let mut crc = 0xFFFFFFFFu32;
    for b in data {
        crc = table[((crc ^ *b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}

/// A minimal, deterministic ZIP archive: store method (no compression),
/// fixed DOS timestamps, entries in the given order. Every conforming
/// reader — and every FMI importer — handles stored entries.
fn zip_store(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let le16 = |v: u16| v.to_le_bytes();
    let le32 = |v: u32| v.to_le_bytes();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let nb = name.as_bytes();
        // Local file header.
        out.extend_from_slice(&le32(0x04034b50));
        out.extend_from_slice(&le16(20)); // version needed
        out.extend_from_slice(&le16(0)); // flags
        out.extend_from_slice(&le16(0)); // method: store
        out.extend_from_slice(&le16(0)); // mod time (fixed)
        out.extend_from_slice(&le16(0x21)); // mod date (fixed: 1980-01-01)
        out.extend_from_slice(&le32(crc));
        out.extend_from_slice(&le32(data.len() as u32));
        out.extend_from_slice(&le32(data.len() as u32));
        out.extend_from_slice(&le16(nb.len() as u16));
        out.extend_from_slice(&le16(0)); // extra len
        out.extend_from_slice(nb);
        out.extend_from_slice(data);
        // Central directory entry.
        central.extend_from_slice(&le32(0x02014b50));
        central.extend_from_slice(&le16(20)); // made by
        central.extend_from_slice(&le16(20)); // needed
        central.extend_from_slice(&le16(0));
        central.extend_from_slice(&le16(0));
        central.extend_from_slice(&le16(0));
        central.extend_from_slice(&le16(0x21));
        central.extend_from_slice(&le32(crc));
        central.extend_from_slice(&le32(data.len() as u32));
        central.extend_from_slice(&le32(data.len() as u32));
        central.extend_from_slice(&le16(nb.len() as u16));
        central.extend_from_slice(&le16(0)); // extra
        central.extend_from_slice(&le16(0)); // comment
        central.extend_from_slice(&le16(0)); // disk
        central.extend_from_slice(&le16(0)); // internal attrs
        central.extend_from_slice(&le32(0)); // external attrs
        central.extend_from_slice(&le32(offset));
        central.extend_from_slice(nb);
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&(0x06054b50u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_ieee_reference() {
        // The canonical check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn zip_layout_is_readable_and_deterministic() {
        let files = vec![
            ("a.txt".to_string(), b"hello".to_vec()),
            ("dir/b.bin".to_string(), vec![0u8, 1, 2, 3]),
        ];
        let z1 = zip_store(&files);
        let z2 = zip_store(&files);
        assert_eq!(z1, z2, "byte-identical on re-export");
        assert_eq!(&z1[0..4], &[0x50, 0x4b, 0x03, 0x04], "zip magic");
        // The EOCD record is present and points at a central dir with 2 entries.
        let eocd = z1.len() - 22;
        assert_eq!(&z1[eocd..eocd + 4], &[0x50, 0x4b, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([z1[eocd + 10], z1[eocd + 11]]), 2);
    }
}
