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

/// Render a Kind 2 counterexample as a fixed-width per-cycle waveform table.
///
/// The expected shape (Kind 2 v1+ `-json` output) is an array of scopes, each
/// with a `streams` list; each stream has a `name`, a `type`, and an
/// `instantValues` list of `[step, value]` pairs. Returns `None` if the JSON
/// does not match this shape (unparseable counterexamples are surfaced as
/// raw JSON by the caller).
pub fn render_counterexample_waveform(cex: &serde_json::Value) -> Option<String> {
    let scopes = cex.as_array()?;
    if scopes.is_empty() {
        return None;
    }

    // Collect (name, values_indexed_by_cycle) across every stream of every
    // scope so a multi-scope counterexample renders as one wide table.
    let mut columns: Vec<(String, Vec<String>)> = Vec::new();
    let mut max_cycle: usize = 0;
    for scope in scopes {
        let streams = match scope.get("streams").and_then(|s| s.as_array()) {
            Some(s) => s,
            None => continue,
        };
        for s in streams {
            let name = s
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string();
            let mut vals: Vec<String> = Vec::new();
            if let Some(iv) = s.get("instantValues").and_then(|v| v.as_array()) {
                for entry in iv {
                    if let Some(pair) = entry.as_array() {
                        if pair.len() >= 2 {
                            let step = pair[0].as_u64().unwrap_or(0) as usize;
                            let v = pair[1]
                                .as_str()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| pair[1].to_string());
                            while vals.len() <= step {
                                vals.push(String::new());
                            }
                            vals[step] = v;
                            if step > max_cycle {
                                max_cycle = step;
                            }
                        }
                    }
                }
            }
            columns.push((name, vals));
        }
    }
    if columns.is_empty() {
        return None;
    }

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

