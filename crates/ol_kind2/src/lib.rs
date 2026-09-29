//! OpenLustre Studio: Kind 2 adapter (Phase 7, plan Task 13).
//!
//! Drives the external `kind2` binary against a generated `.lus` file and
//! parses its JSON output. Kind 2 is a separate tool — this crate does not
//! depend on it at build time; it simply shells out and translates results.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `kind2 --enable BMC ...` style invocation (default).
    BmcInd,
    Realizability,
    ModeCoverage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kind2Options {
    pub kind2_binary: String,
    pub mode: SerMode,
    pub main_node: Option<String>,
    pub extra_args: Vec<String>,
    /// Wall-clock timeout for the prover, in seconds. `None` lets Kind 2
    /// run with its default (unlimited).
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
    /// If non-empty, restrict the prover to these named properties via
    /// `--lus_props`. Empty means "all properties" (Kind 2's default).
    #[serde(default)]
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SerMode {
    BmcInd,
    Realizability,
    ModeCoverage,
}

impl Default for Kind2Options {
    fn default() -> Self {
        Self {
            kind2_binary: "kind2".into(),
            mode: SerMode::BmcInd,
            main_node: None,
            extra_args: vec![],
            timeout_seconds: None,
            properties: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kind2Result {
    pub invocation: Vec<String>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Parsed property results if Kind 2 produced JSON.
    pub properties: Vec<PropertyResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyResult {
    pub name: String,
    pub status: String,
    pub scope: Option<String>,
    pub source: Option<String>,
    pub counterexample: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum Kind2Error {
    #[error("could not invoke kind2 (`{0}`): {1}")]
    Spawn(String, std::io::Error),
}

pub fn run_kind2(lus_path: &Path, opts: &Kind2Options) -> Result<Kind2Result, Kind2Error> {
    let mut args: Vec<String> = vec!["-json".into()];
    match opts.mode {
        SerMode::Realizability => {
            args.push("--enable".into());
            args.push("CONTRACTCK".into());
        }
        SerMode::ModeCoverage => {
            args.push("--enable".into());
            args.push("MCS".into());
        }
        SerMode::BmcInd => {}
    }
    if let Some(main) = &opts.main_node {
        args.push("--lus_main".into());
        args.push(main.clone());
    }
    if let Some(t) = opts.timeout_seconds {
        args.push("--timeout_wall".into());
        args.push(t.to_string());
    }
    if !opts.properties.is_empty() {
        args.push("--lus_props".into());
        args.push(opts.properties.join(","));
    }
    for a in &opts.extra_args {
        args.push(a.clone());
    }
    args.push(lus_path.display().to_string());

    let mut invocation = vec![opts.kind2_binary.clone()];
    invocation.extend(args.clone());

    let child = Command::new(&opts.kind2_binary)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            // Surface a friendly "kind2 missing" result rather than failing —
            // many users will run this without Kind 2 installed.
            return Ok(Kind2Result {
                invocation,
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("could not launch `{}`: {e}", opts.kind2_binary),
                properties: vec![],
            });
        }
    };
    let output = child
        .wait_with_output()
        .map_err(|e| Kind2Error::Spawn(opts.kind2_binary.clone(), e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let properties = parse_kind2_json(&stdout);

    Ok(Kind2Result {
        invocation,
        exit_code: output.status.code().unwrap_or(-1),
        stdout,
        stderr,
        properties,
    })
}

/// Kind 2's `-json` output is a JSON array (or NDJSON in some versions). We
/// try both. Each property is a `{ objectType: "property", ... }` record.
pub fn parse_kind2_json(text: &str) -> Vec<PropertyResult> {
    let mut props = Vec::new();
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(text) {
        for v in arr {
            if let Some(p) = json_to_property(&v) {
                props.push(p);
            }
        }
        return props;
    }
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(p) = json_to_property(&v) {
                props.push(p);
            }
        }
    }
    props
}

fn json_to_property(v: &serde_json::Value) -> Option<PropertyResult> {
    let obj = v.as_object()?;
    if obj.get("objectType")?.as_str()? != "property" {
        return None;
    }
    let name = obj
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("unnamed")
        .to_string();
    let status = obj
        .get("answer")
        .and_then(|a| a.as_object())
        .and_then(|a| a.get("value"))
        .and_then(|s| s.as_str())
        .or_else(|| obj.get("status").and_then(|s| s.as_str()))
        .unwrap_or("unknown")
        .to_string();
    let scope = obj
        .get("scope")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let source = obj
        .get("source")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let counterexample = obj.get("counterExample").cloned();
    Some(PropertyResult {
        name,
        status,
        scope,
        source,
        counterexample,
    })
}

/// One signal of a counterexample, one value per cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CexStream {
    /// The node or contract the stream belongs to.
    pub scope: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// Kind 2's classification (`input`, `output`, `local`, …); empty when
    /// the JSON carries none.
    pub class: String,
    /// Values by cycle; empty where Kind 2 reported no value for a cycle.
    pub values: Vec<String>,
}

/// A counterexample as signals over a common cycle axis — what the Studio's
/// waveform viewer draws and what it replays in the simulator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Counterexample {
    pub cycles: usize,
    pub streams: Vec<CexStream>,
}

/// Parse a Kind 2 counterexample into streams over cycles.
///
/// The expected shape (Kind 2 v1+ `-json` output) is an array of scopes, each
/// with a `streams` list; each stream has a `name`, a `type`, and an
/// `instantValues` list of `[step, value]` pairs. Every stream of every
/// top-level scope is kept, padded to the longest. Returns `None` if the
/// JSON does not match this shape.
pub fn counterexample_streams(cex: &serde_json::Value) -> Option<Counterexample> {
    let scopes = cex.as_array()?;
    let mut streams: Vec<CexStream> = Vec::new();
    let mut max_cycle: usize = 0;
    for scope in scopes {
        let Some(list) = scope.get("streams").and_then(|s| s.as_array()) else {
            continue;
        };
        let scope_name = scope.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        for s in list {
            let text = |key: &str| s.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut values: Vec<String> = Vec::new();
            if let Some(iv) = s.get("instantValues").and_then(|v| v.as_array()) {
                for pair in iv.iter().filter_map(|e| e.as_array()).filter(|p| p.len() >= 2) {
                    let step = pair[0].as_u64().unwrap_or(0) as usize;
                    let v = pair[1]
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| pair[1].to_string());
                    if values.len() <= step {
                        values.resize(step + 1, String::new());
                    }
                    values[step] = v;
                    max_cycle = max_cycle.max(step);
                }
            }
            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
            streams.push(CexStream { scope: scope_name.clone(), name, ty: text("type"), class: text("class"), values });
        }
    }
    if streams.is_empty() {
        return None;
    }
    for s in &mut streams {
        s.values.resize(max_cycle + 1, String::new());
    }
    Some(Counterexample { cycles: max_cycle + 1, streams })
}

/// Render a Kind 2 counterexample as a fixed-width per-cycle waveform table
/// (the text form of [`counterexample_streams`]; `None` when that is).
pub fn render_counterexample_waveform(cex: &serde_json::Value) -> Option<String> {
    let parsed = counterexample_streams(cex)?;
    let max_cycle = parsed.cycles - 1;
    let columns: Vec<(String, Vec<String>)> =
        parsed.streams.into_iter().map(|s| (s.name, s.values)).collect();

    // Compute column widths so the table aligns.
    let widths: Vec<usize> = columns
        .iter()
        .map(|(name, vals)| {
            let v_max = vals.iter().map(|v| v.len()).max().unwrap_or(0);
            name.len().max(v_max).max(1)
        })
        .collect();
    let cycle_w = format!("{max_cycle}").len().max(5);

    let pad = |s: &str, w: usize| -> String {
        if s.len() >= w {
            s.to_string()
        } else {
            let mut out = s.to_string();
            for _ in s.len()..w {
                out.push(' ');
            }
            out
        }
    };

    let mut out = String::new();
    out.push_str(&pad("cycle", cycle_w));
    for (i, (name, _)) in columns.iter().enumerate() {
        out.push_str(" | ");
        out.push_str(&pad(name, widths[i]));
    }
    out.push('\n');
    out.push_str(&"-".repeat(cycle_w));
    for (i, _) in columns.iter().enumerate() {
        out.push_str("-+-");
        out.push_str(&"-".repeat(widths[i]));
    }
    out.push('\n');
    for cycle in 0..=max_cycle {
        let c = format!("{cycle}");
        out.push_str(&pad(&c, cycle_w));
        for (i, (_, vals)) in columns.iter().enumerate() {
            out.push_str(" | ");
            let v = vals.get(cycle).cloned().unwrap_or_default();
            out.push_str(&pad(&v, widths[i]));
        }
        out.push('\n');
    }
    Some(out)
}

