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

/// What to ask Kind 2.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SerMode {
    /// Prove every property: guarantees, mode ensures, `--%PROPERTY`s —
    /// with the mode checks (each mode reachable, one mode always active).
    BmcInd,
    /// Check that each contract is realizable: some implementation can meet
    /// the guarantees for every input the assumptions allow.
    Realizability,
    /// Only the mode checks: is every mode reachable, and is some mode
    /// always active? (Guarantees are not proved.)
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
    /// Parsed property results if Kind 2 produced JSON — one per property,
    /// the most conclusive answer when several engines reported it.
    pub properties: Vec<PropertyResult>,
    /// Kind 2's `error` / `fatal` log messages (a rejected input file, a
    /// missing SMT solver, …), with `file:line:col` when it gave one.
    #[serde(default)]
    pub errors: Vec<String>,
    /// Realizability results (`SerMode::Realizability`), in report order.
    #[serde(default)]
    pub realizability: Vec<RealizabilityResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyResult {
    /// Kind 2's name, e.g. `Contract[l12c3].guarantee_name`.
    pub name: String,
    /// `valid`, `falsifiable`, `reachable`, `unreachable`, `unknown`, …
    pub status: String,
    pub scope: Option<String>,
    /// What the property checks: `Guarantee`, `Ensure`, `Assumption`,
    /// `NonVacuityCheck` (a mode is reachable), `OneModeActive` (some mode
    /// always applies), `PropAnnot` (a `--%PROPERTY`), …
    pub source: Option<String>,
    pub counterexample: Option<serde_json::Value>,
    /// A trace reaching the property, for reachability checks.
    #[serde(default)]
    pub witness: Option<serde_json::Value>,
    /// Where the property is in the input file (1-based), when reported.
    #[serde(default)]
    pub line: Option<u64>,
    /// A short unique name: [`display_name`], numbered when two properties
    /// would read the same (two `ensure`s of one mode).
    #[serde(default)]
    pub label: String,
}

/// How a property result reads for a reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// Proved, or (for a mode check) the mode is reachable.
    Holds,
    /// Falsified — or a mode that can never be active.
    Fails,
    /// Timed out or not concluded.
    Unknown,
}

impl PropertyResult {
    pub fn outcome(&self) -> Outcome {
        match self.status.to_ascii_lowercase().as_str() {
            "valid" | "reachable" => Outcome::Holds,
            "falsifiable" | "invalid" | "unreachable" => Outcome::Fails,
            _ => Outcome::Unknown,
        }
    }

    /// True for the mode checks Kind 2 adds on its own.
    pub fn is_mode_check(&self) -> bool {
        matches!(self.source.as_deref(), Some("NonVacuityCheck") | Some("OneModeActive"))
    }

    /// The name without Kind 2's source positions: `C.pos`, `C.Big.ensure`.
    pub fn display_name(&self) -> String {
        display_name(&self.name)
    }
}

/// Strip `[l12c3]` position tags from a Kind 2 property name.
pub fn display_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    for ch in name.chars() {
        match ch {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// The clause at `line` of the Kind 2 input, trimmed of its `;` — e.g.
/// `guarantee "release_implies_arm" release_cmd => master_arm`.
pub fn clause_at(input: &str, line: u64) -> Option<String> {
    let text = input.lines().nth(line.checked_sub(1)? as usize)?.trim();
    let text = text.trim_end_matches(';').trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// One realizability check: of the environment (can the assumptions be met)
/// or of the contract (can the guarantees be met under them).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealizabilityResult {
    /// The node analysed.
    pub node: String,
    /// `environment` or `contract`.
    pub context: String,
    /// `realizable`, `unrealizable`, or `unknown`.
    pub result: String,
    /// For an unrealizable contract: the clauses that conflict, as
    /// `category name` (e.g. `guarantee no_release_in_fault`).
    #[serde(default)]
    pub conflicting: Vec<String>,
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
        SerMode::ModeCoverage | SerMode::BmcInd => {}
    }
    if let Some(main) = &opts.main_node {
        args.push("--lus_main".into());
        args.push(main.clone());
    }
    if let Some(t) = opts.timeout_seconds {
        args.push("--timeout_wall".into());
        args.push(t.to_string());
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
                errors: vec![],
                realizability: vec![],
            });
        }
    };
    let output = child
        .wait_with_output()
        .map_err(|e| Kind2Error::Spawn(opts.kind2_binary.clone(), e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let mut properties = parse_kind2_json(&stdout);
    // Kind 2 has no per-property filter: prove everything, report the asked.
    if !opts.properties.is_empty() {
        properties.retain(|p| {
            let shown = p.display_name();
            opts.properties.iter().any(|want| {
                *want == p.name || *want == shown || shown.rsplit('.').next() == Some(want.as_str())
            })
        });
    }
    if matches!(opts.mode, SerMode::ModeCoverage) {
        properties.retain(|p| p.is_mode_check());
    }
    let (errors, realizability) = parse_kind2_log(&stdout);

    Ok(Kind2Result {
        invocation,
        exit_code: output.status.code().unwrap_or(-1),
        stdout,
        stderr,
        properties,
        errors,
        realizability,
    })
}

fn json_objects(text: &str) -> Vec<serde_json::Value> {
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(text) {
        return arr;
    }
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .collect()
}

/// Kind 2's error log and realizability results from its `-json` output.
pub fn parse_kind2_log(text: &str) -> (Vec<String>, Vec<RealizabilityResult>) {
    let mut errors = Vec::new();
    let mut realizability = Vec::new();
    let mut node = String::new();
    let mut context = String::new();
    for v in json_objects(text) {
        let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        match get("objectType").as_str() {
            "log" if matches!(get("level").as_str(), "error" | "fatal") => {
                let msg = get("value").trim().to_string();
                let at = match (v.get("line").and_then(|l| l.as_u64()), v.get("column").and_then(|c| c.as_u64())) {
                    (Some(l), Some(c)) => format!("line {l}, column {c}: "),
                    (Some(l), None) => format!("line {l}: "),
                    _ => String::new(),
                };
                errors.push(format!("{at}{msg}"));
            }
            "analysisStart" => {
                node = get("top");
                context = get("context");
            }
            "realizabilityCheck" => {
                let mut conflicting = Vec::new();
                if let Some(nodes) = v.pointer("/conflictingSet/nodes").and_then(|n| n.as_array()) {
                    for n in nodes {
                        for e in n.get("elements").and_then(|e| e.as_array()).into_iter().flatten() {
                            let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or("");
                            conflicting.push(format!("{} {}", s("category"), s("name")).trim().to_string());
                        }
                    }
                }
                realizability.push(RealizabilityResult {
                    node: node.clone(),
                    context: context.clone(),
                    result: get("result"),
                    conflicting,
                });
            }
            _ => {}
        }
    }
    (errors, realizability)
}

/// Kind 2's `-json` output is a JSON array (or NDJSON in some versions). We
/// try both. Each property is a `{ objectType: "property", ... }` record.
///
/// Several engines may report the same property (BMC and IC3 both reach a
/// mode, say); one result per name is kept — the first conclusive answer,
/// in first-report order.
pub fn parse_kind2_json(text: &str) -> Vec<PropertyResult> {
    let mut props: Vec<PropertyResult> = Vec::new();
    for v in json_objects(text) {
        let Some(p) = json_to_property(&v) else { continue };
        match props.iter_mut().find(|q| q.name == p.name) {
            Some(q) if q.outcome() == Outcome::Unknown && p.outcome() != Outcome::Unknown => *q = p,
            Some(_) => {}
            None => props.push(p),
        }
    }
    let names: Vec<String> = props.iter().map(|p| p.display_name()).collect();
    for (i, p) in props.iter_mut().enumerate() {
        let same = names.iter().filter(|n| **n == names[i]).count();
        p.label = if same > 1 {
            let k = names[..=i].iter().filter(|n| **n == names[i]).count();
            format!("{} #{k}", names[i])
        } else {
            names[i].clone()
        };
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
    let witness = obj.get("witness").cloned();
    let line = obj.get("line").and_then(|l| l.as_u64());
    Some(PropertyResult {
        name,
        status,
        scope,
        source,
        counterexample,
        witness,
        line,
        label: String::new(),
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

