//! Project-level test scenarios — the SCADE Test analog (golden traces +
//! IR↔compiled-C equivalence), as a user-facing feature instead of an
//! internal test pattern.
//!
//! ## Convention
//!
//! A scenario directory holds named input vectors and their recorded golden
//! traces:
//!
//! ```text
//! scenarios/
//!   nominal.csv          # input vector: header row = main node's inputs
//!   nominal.golden.csv   # golden FULL trace captured by `openlustre test record`
//!   fault_case.csv
//!   fault_case.golden.csv
//! ```
//!
//! `record` captures the IR simulator's full trace (cycle, inputs, locals,
//! outputs, and active_mode/violations when the node has a contract) as the
//! golden reference. `run` re-executes every scenario and compares:
//!
//! * **ir** backend — the IR simulator's full trace must match the golden
//!   byte-for-byte.
//! * **c** backend — the generated C-Lite is compiled with `cc` and driven
//!   with the same inputs; every column the C driver emits must match the
//!   golden's column of the same name, cycle by cycle.
//!
//! Failures are reported at cell granularity: scenario, cycle, signal,
//! expected vs actual — the diagnostic shape the implementation plan's
//! Phase 6 called "gold".

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioInfo {
    pub name: String,
    pub input_path: PathBuf,
    pub golden_path: PathBuf,
    pub has_golden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Ir,
    C,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    NoGolden,
    Skipped,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct CellDiff {
    pub cycle: String,
    pub column: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioResult {
    pub name: String,
    pub backend: Backend,
    pub status: Status,
    /// Cell-level differences (capped at [`MAX_DIFFS`] per scenario).
    pub diffs: Vec<CellDiff>,
    /// Human-readable detail: error text, skip reason, or row-count summary.
    pub message: String,
}

const MAX_DIFFS: usize = 20;

pub fn list_scenarios(dir: &Path) -> Vec<ScenarioInfo> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let Some(fname) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !fname.ends_with(".csv") || fname.ends_with(".golden.csv") {
            continue;
        }
        let name = fname.trim_end_matches(".csv").to_string();
        let golden_path = dir.join(format!("{name}.golden.csv"));
        out.push(ScenarioInfo {
            has_golden: golden_path.exists(),
            name,
            input_path: p,
            golden_path,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Run the IR simulator (full trace) on one input vector.
fn ir_full_trace(
    project: &ol_ir::Project,
    node: &str,
    input_csv: &str,
) -> Result<String, String> {
    let mut sim = ol_sim::Sim::new(project, node).map_err(|e| e.to_string())?;
    let trace = sim.run_csv_full(input_csv).map_err(|e| e.to_string())?;
    Ok(trace.to_csv())
}

/// Capture golden traces for every scenario in `dir`. Returns the recorded
/// (name, path) pairs.
pub fn record_goldens(
    project: &ol_ir::Project,
    dir: &Path,
    node: &str,
) -> Result<Vec<(String, PathBuf)>, String> {
    let scenarios = list_scenarios(dir);
    if scenarios.is_empty() {
        return Err(format!(
            "no scenario input files (*.csv) found in {}",
            dir.display()
        ));
    }
    let mut recorded = Vec::new();
    for s in scenarios {
        let input = std::fs::read_to_string(&s.input_path)
            .map_err(|e| format!("{}: {e}", s.input_path.display()))?;
        let golden = ir_full_trace(project, node, &input)
            .map_err(|e| format!("scenario `{}`: {e}", s.name))?;
        std::fs::write(&s.golden_path, &golden)
            .map_err(|e| format!("{}: {e}", s.golden_path.display()))?;
        recorded.push((s.name, s.golden_path));
    }
    Ok(recorded)
}

/// Run every scenario against the requested backends and compare with the
/// goldens. `backends` is typically `[Ir, C]`; the C backend is reported as
/// `Skipped` when `cc` is not available rather than failing the run.
pub fn run_scenarios(
    project: &ol_ir::Project,
    dir: &Path,
    node: &str,
    backends: &[Backend],
) -> Vec<ScenarioResult> {
    let scenarios = list_scenarios(dir);
    let mut results = Vec::new();

    // Compile the C backend once per run — every scenario reuses the binary.
    let c_exe: Option<Result<CompiledModel, String>> = if backends.contains(&Backend::C) {
        if cc_available() {
            Some(compile_model(project, node))
        } else {
            None
        }
    } else {
        None
    };

    for s in &scenarios {
        let input = match std::fs::read_to_string(&s.input_path) {
            Ok(i) => i,
            Err(e) => {
                for &backend in backends {
                    results.push(ScenarioResult {
                        name: s.name.clone(),
                        backend,
                        status: Status::Error,
                        diffs: vec![],
                        message: format!("could not read input: {e}"),
                    });
                }
                continue;
            }
        };
        let golden = if s.has_golden {
            std::fs::read_to_string(&s.golden_path).ok()
        } else {
            None
        };

        for &backend in backends {
            let Some(golden) = &golden else {
                results.push(ScenarioResult {
                    name: s.name.clone(),
                    backend,
                    status: Status::NoGolden,
                    diffs: vec![],
                    message: format!(
                        "no golden trace; run `openlustre test record` to capture {}",
                        s.golden_path.display()
                    ),
                });
                continue;
            };
            let result = match backend {
                Backend::Ir => match ir_full_trace(project, node, &input) {
                    Ok(actual) => compare_csv(golden, &actual, &s.name, backend, false),
                    Err(e) => ScenarioResult {
                        name: s.name.clone(),
                        backend,
                        status: Status::Error,
                        diffs: vec![],
                        message: e,
                    },
                },
                Backend::C => match &c_exe {
                    None => ScenarioResult {
                        name: s.name.clone(),
                        backend,
                        status: Status::Skipped,
                        diffs: vec![],
                        message: "cc not available on this machine".into(),
                    },
                    Some(Err(e)) => ScenarioResult {
                        name: s.name.clone(),
                        backend,
                        status: Status::Error,
                        diffs: vec![],
                        message: format!("C build failed: {e}"),
                    },
                    Some(Ok(compiled)) => match compiled.run(&input) {
                        Ok(actual) => compare_csv(golden, &actual, &s.name, backend, true),
                        Err(e) => ScenarioResult {
                            name: s.name.clone(),
                            backend,
                            status: Status::Error,
                            diffs: vec![],
                            message: e,
                        },
                    },
                },
            };
            results.push(result);
        }
    }
    results
}

/// Compare an actual trace against the golden.
///
/// * `project_columns = false` (IR backend): byte-level equality of the full
///   trace, diffed cell-by-cell when unequal.
/// * `project_columns = true` (C backend): the actual trace's columns are a
///   subset of the golden's (the C driver emits cycle + outputs + monitor
///   columns, while the golden also carries inputs and locals); each actual
///   column is compared against the golden column of the same name.
fn compare_csv(
    golden: &str,
    actual: &str,
    name: &str,
    backend: Backend,
    project_columns: bool,
) -> ScenarioResult {
    let g = parse_csv(golden);
    let a = parse_csv(actual);

    let mut diffs = Vec::new();
    let mut message = String::new();

    // Map each actual column to the golden column with the same header.
    let mut col_pairs: Vec<(usize, usize, String)> = Vec::new(); // (a_idx, g_idx, name)
    for (a_idx, col) in a.header.iter().enumerate() {
        match g.header.iter().position(|h| h == col) {
            Some(g_idx) => col_pairs.push((a_idx, g_idx, col.clone())),
            None => {
                message = format!("column `{col}` missing from golden trace");
            }
        }
    }
    if !project_columns {
        // IR mode also requires the golden to have no extra columns.
        for col in &g.header {
            if !a.header.contains(col) {
                message = format!("column `{col}` missing from actual trace");
            }
        }
    }

    if g.rows.len() != a.rows.len() {
        message = format!(
            "row count differs: golden has {} cycles, actual has {}",
            g.rows.len(),
            a.rows.len()
        );
    }

    let rows = g.rows.len().min(a.rows.len());
    'outer: for r in 0..rows {
        for (a_idx, g_idx, col) in &col_pairs {
            let exp = g.rows[r].get(*g_idx).map(|s| s.as_str()).unwrap_or("");
            let act = a.rows[r].get(*a_idx).map(|s| s.as_str()).unwrap_or("");
            if exp != act {
                diffs.push(CellDiff {
                    cycle: g.rows[r].first().cloned().unwrap_or_else(|| r.to_string()),
                    column: col.clone(),
                    expected: exp.to_string(),
                    actual: act.to_string(),
                });
                if diffs.len() >= MAX_DIFFS {
                    message = format!("more than {MAX_DIFFS} differing cells; truncated");
                    break 'outer;
                }
            }
        }
    }

    let status = if diffs.is_empty() && message.is_empty() {
        Status::Pass
    } else {
        Status::Fail
    };
    ScenarioResult {
        name: name.to_string(),
        backend,
        status,
        diffs,
        message,
    }
}

struct Csv {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn parse_csv(text: &str) -> Csv {
    let mut lines = text.trim().lines();
    let header = lines
        .next()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let rows = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split(',').map(|s| s.trim().to_string()).collect())
        .collect();
    Csv { header, rows }
}

// --- C backend: compile once, run per scenario ---

struct CompiledModel {
    /// Owns the temp dir so it lives as long as the binary.
    #[allow(dead_code)]
    dir: TempDir,
    exe: PathBuf,
}

impl CompiledModel {
    fn run(&self, input_csv: &str) -> Result<String, String> {
        use std::io::Write as _;
        let mut child = Command::new(&self.exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawning compiled model: {e}"))?;
        child
            .stdin
            .as_mut()
            .ok_or("no stdin")?
            .write_all(input_csv.as_bytes())
            .map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "compiled model exited with {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn compile_model(project: &ol_ir::Project, node_name: &str) -> Result<CompiledModel, String> {
    let node = project
        .find_node(node_name)
        .ok_or_else(|| format!("node `{node_name}` not found"))?;

    let bundle = ol_clite_emit::emit_project(project);
    let has_contract = node.contract.is_some();
    let driver = if has_contract {
        ol_clite_emit::harness::emit_csv_driver_with_monitor(
            node,
            node.contract.as_deref(),
        )
    } else {
        ol_clite_emit::harness::emit_csv_driver(node)
    };

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("openlustre_test_{stamp}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dir = TempDir(dir);
    let d = &dir.0;

    std::fs::write(d.join("openlustre_generated.h"), &bundle.header).map_err(|e| e.to_string())?;
    std::fs::write(d.join("openlustre_generated.c"), &bundle.source).map_err(|e| e.to_string())?;
    std::fs::write(d.join("driver.c"), &driver).map_err(|e| e.to_string())?;

    let mut sources = vec![d.join("openlustre_generated.c"), d.join("driver.c")];
    if has_contract {
        let mon = ol_clite_emit::monitor::emit_monitors(project);
        std::fs::write(d.join("openlustre_monitors.h"), &mon.header).map_err(|e| e.to_string())?;
        std::fs::write(d.join("openlustre_monitors.c"), &mon.source).map_err(|e| e.to_string())?;
        sources.push(d.join("openlustre_monitors.c"));
    }

    let exe = d.join("model_under_test");
    let cc = Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Wno-unused-but-set-variable",
            "-Wno-unused-variable",
            "-Werror",
            "-o",
        ])
        .arg(&exe)
        .args(&sources)
        .arg(format!("-I{}", d.display()))
        .output()
        .map_err(|e| format!("invoking cc: {e}"))?;
    if !cc.status.success() {
        return Err(String::from_utf8_lossy(&cc.stderr).to_string());
    }
    Ok(CompiledModel { dir, exe })
}

/// Render results as a human-readable report (the CLI surface).
pub fn render_report(results: &[ScenarioResult]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut other = 0usize;
    for r in results {
        let tag = match r.status {
            Status::Pass => {
                pass += 1;
                "PASS"
            }
            Status::Fail => {
                fail += 1;
                "FAIL"
            }
            Status::NoGolden => {
                other += 1;
                "NO-GOLDEN"
            }
            Status::Skipped => {
                other += 1;
                "SKIP"
            }
            Status::Error => {
                fail += 1;
                "ERROR"
            }
        };
        let backend = match r.backend {
            Backend::Ir => "ir",
            Backend::C => "c ",
        };
        let _ = writeln!(out, "[{tag}] {} ({backend})", r.name);
        if !r.message.is_empty() {
            let _ = writeln!(out, "       {}", r.message);
        }
        for d in &r.diffs {
            let _ = writeln!(
                out,
                "       cycle {}: `{}` expected {} got {}",
                d.cycle, d.column, d.expected, d.actual
            );
        }
    }
    let _ = writeln!(out, "\n{pass} passed, {fail} failed, {other} skipped/missing");
    out
}

/// True when every result is Pass / Skipped / NoGolden (i.e. nothing failed).
pub fn all_green(results: &[ScenarioResult]) -> bool {
    results
        .iter()
        .all(|r| !matches!(r.status, Status::Fail | Status::Error))
}
