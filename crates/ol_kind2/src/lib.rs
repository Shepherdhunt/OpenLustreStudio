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
