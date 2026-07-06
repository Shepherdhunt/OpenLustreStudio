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

/// Run the IR simulator (full trace) on one input vector. When `acc` is
/// provided, decision coverage is collected and merged into it (keyed by
/// node/context/condition so outcomes accumulate across scenarios).
fn ir_full_trace(
    project: &ol_ir::Project,
    node: &str,
    input_csv: &str,
    acc: Option<&mut CoverageAcc>,
    mcdc: Option<&mut McdcAcc>,
) -> Result<String, String> {
    let mut sim = ol_sim::Sim::new(project, node).map_err(|e| e.to_string())?;
    let collect = acc.is_some() || mcdc.is_some();
    if collect {
        sim.enable_coverage();
    }
    let trace = sim.run_csv_full(input_csv).map_err(|e| e.to_string())?;
    if let (Some(acc), Some(sites)) = (acc, sim.coverage_sites()) {
        for site in sites {
            let key = (site.node.clone(), site.context.clone(), site.condition.clone());
            let entry = acc.entry(key).or_insert((false, false));
            entry.0 |= site.seen_true;
            entry.1 |= site.seen_false;
        }
    }
    // Merge MC/DC trials across scenarios, keyed by the decision's identity.
    if let (Some(mcdc), Some(decisions)) = (mcdc, sim.mcdc_decisions()) {
        for d in decisions {
            let key = (d.node.clone(), d.context.clone(), d.decision.clone());
            let entry = mcdc
                .entry(key)
                .or_insert_with(|| McdcDecisionAgg { conditions: d.conditions.clone(), trials: Default::default() });
            for t in d.trials {
                entry.trials.insert(t);
            }
        }
    }
    Ok(trace.to_csv())
}

type CoverageAcc =
    std::collections::BTreeMap<(String, String, String), (bool, bool)>;

/// Suite-level MC/DC accumulation: distinct trials per decision, keyed by
/// (node, equation context, decision text) so they merge across scenarios.
type McdcAcc = std::collections::BTreeMap<(String, String, String), McdcDecisionAgg>;

struct McdcDecisionAgg {
    conditions: Vec<String>,
    trials: std::collections::HashSet<ol_sim::McdcTrial>,
}

/// Suite-level decision coverage: how many if-conditions were driven both
/// true and false by the scenario suite (the first rung toward MC/DC).
#[derive(Debug, Clone, Serialize)]
pub struct CoverageSummary {
    pub total: usize,
    pub covered: usize,
    pub uncovered: Vec<UncoveredDecision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UncoveredDecision {
    pub node: String,
    pub context: String,
    pub condition: String,
    /// Which outcome was never observed: "true", "false", or "both".
    pub missing: String,
}

fn summarize_coverage(acc: &CoverageAcc) -> CoverageSummary {
    let total = acc.len();
    let mut covered = 0usize;
    let mut uncovered = Vec::new();
    for ((node, context, condition), (t, f)) in acc {
        if *t && *f {
            covered += 1;
        } else {
            uncovered.push(UncoveredDecision {
                node: node.clone(),
                context: context.clone(),
                condition: condition.clone(),
                missing: match (t, f) {
                    (false, false) => "both",
                    (false, true) => "true",
                    (true, false) => "false",
                    _ => unreachable!(),
                }
                .to_string(),
            });
        }
    }
    CoverageSummary { total, covered, uncovered }
}

/// Suite-level MC/DC: how many conditions were shown to independently affect
/// their decision (a test pair that flips only that condition flips the
/// outcome). This is the DO-178C Level A metric, one rung above decision
/// coverage.
#[derive(Debug, Clone, Serialize)]
pub struct McdcSummary {
    pub total_decisions: usize,
    pub covered_decisions: usize,
    pub total_conditions: usize,
    pub covered_conditions: usize,
    pub uncovered: Vec<McdcUncovered>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McdcUncovered {
    pub node: String,
    pub context: String,
    pub decision: String,
    pub condition: String,
    pub reason: String,
}

fn summarize_mcdc(acc: &McdcAcc) -> McdcSummary {
    let mut total_decisions = 0;
    let mut covered_decisions = 0;
    let mut total_conditions = 0;
    let mut covered_conditions = 0;
    let mut uncovered = Vec::new();
    for ((node, context, decision), agg) in acc {
        let n = agg.conditions.len();
        if n == 0 {
            continue;
        }
        total_decisions += 1;
        let trials: Vec<ol_sim::McdcTrial> = agg.trials.iter().cloned().collect();
        let indep = ol_sim::mcdc_independence(n, &trials);
        let mut all = true;
        for (i, pair) in indep.iter().enumerate() {
            total_conditions += 1;
            if pair.is_some() {
                covered_conditions += 1;
            } else {
                all = false;
                uncovered.push(McdcUncovered {
                    node: node.clone(),
                    context: context.clone(),
                    decision: decision.clone(),
                    condition: agg.conditions[i].clone(),
                    reason: "no test pair flips only this condition and the outcome".into(),
                });
            }
        }
        if all {
            covered_decisions += 1;
        }
    }
    McdcSummary {
        total_decisions,
        covered_decisions,
        total_conditions,
        covered_conditions,
        uncovered,
    }
}

/// A scenario run's full outcome: per-scenario/backend results plus the
/// suite-level decision coverage and MC/DC measured on the IR backend.
#[derive(Debug, Clone, Serialize)]
pub struct RunOutcome {
    pub results: Vec<ScenarioResult>,
    pub coverage: Option<CoverageSummary>,
    pub mcdc: Option<McdcSummary>,
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
        let golden = ir_full_trace(project, node, &input, None, None)
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
) -> RunOutcome {
    let scenarios = list_scenarios(dir);
    let mut results = Vec::new();
    let mut cov_acc: CoverageAcc = CoverageAcc::new();
    let mut mcdc_acc: McdcAcc = McdcAcc::new();
    let want_coverage = backends.contains(&Backend::Ir);

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
                Backend::Ir => match ir_full_trace(
                    project,
                    node,
                    &input,
                    if want_coverage { Some(&mut cov_acc) } else { None },
                    if want_coverage { Some(&mut mcdc_acc) } else { None },
                ) {
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
    RunOutcome {
        results,
        coverage: if want_coverage && !cov_acc.is_empty() {
            Some(summarize_coverage(&cov_acc))
        } else {
            None
        },
        mcdc: if want_coverage && !mcdc_acc.is_empty() {
            Some(summarize_mcdc(&mcdc_acc))
        } else {
            None
        },
    }
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

/// A C compiler discovered on this machine: a POSIX-style driver on PATH
/// (`cc`, `gcc`, `clang`), or — on Windows, where none of those usually
/// exist — MSVC's `cl.exe` reached through the `vcvars64.bat` environment of
/// the newest Visual Studio installation, found via `vswhere`.
enum CompilerKind {
    Posix(&'static str),
    #[cfg(windows)]
    Msvc(PathBuf),
}

fn find_compiler() -> Option<CompilerKind> {
    for name in ["cc", "gcc", "clang"] {
        let found = Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if found {
            return Some(CompilerKind::Posix(name));
        }
    }
    #[cfg(windows)]
    if let Some(bat) = find_msvc_vcvars() {
        return Some(CompilerKind::Msvc(bat));
    }
    None
}

#[cfg(windows)]
fn find_msvc_vcvars() -> Option<PathBuf> {
    let pf86 = std::env::var_os("ProgramFiles(x86)")?;
    let vswhere = Path::new(&pf86).join("Microsoft Visual Studio\\Installer\\vswhere.exe");
    if !vswhere.exists() {
        return None;
    }
    let out = Command::new(&vswhere)
        .args([
            "-products", "*", "-latest",
            "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property", "installationPath",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let install = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if install.is_empty() {
        return None;
    }
    let bat = Path::new(&install).join("VC\\Auxiliary\\Build\\vcvars64.bat");
    bat.exists().then_some(bat)
}

pub fn cc_available() -> bool {
    find_compiler().is_some()
}

/// Compile C sources that already live in `dir` into `dir/<exe_name>`,
/// using a POSIX-style compiler from PATH or MSVC via vcvars64 — the same
/// discovery the scenario harness uses. `which` restricts the choice
/// (`"cc"`/`"gcc"`/`"clang"`/`"msvc"`); `None` is auto. Returns a
/// human-readable log of what ran.
pub(crate) fn compile_in_dir(
    dir: &Path,
    source_names: &[&str],
    exe_name: &str,
    which: Option<&str>,
) -> Result<String, String> {
    compile_in_dir_defs(dir, source_names, exe_name, which, &[])
}

/// Like [`compile_in_dir`] but with extra preprocessor `defines` (macro
/// names) — `-D<name>` for POSIX compilers, `/D<name>` for MSVC. Used to
/// turn on `OL_DEBUG` for the debug-run build.
pub(crate) fn compile_in_dir_defs(
    dir: &Path,
    source_names: &[&str],
    exe_name: &str,
    which: Option<&str>,
    defines: &[&str],
) -> Result<String, String> {
    let compiler = match which {
        None | Some("auto") => {
            find_compiler().ok_or("no C compiler found (cc/gcc/clang on PATH, or MSVC)")?
        }
        Some(name @ ("cc" | "gcc" | "clang")) => {
            let ok = Command::new(name)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                return Err(format!("`{name}` is not available on PATH"));
            }
            match name {
                "cc" => CompilerKind::Posix("cc"),
                "gcc" => CompilerKind::Posix("gcc"),
                _ => CompilerKind::Posix("clang"),
            }
        }
        #[cfg(windows)]
        Some("msvc") => CompilerKind::Msvc(
            find_msvc_vcvars().ok_or("MSVC (vcvars64.bat) not found via vswhere")?,
        ),
        Some(other) => return Err(format!("unknown compiler `{other}`")),
    };

    let (out, desc) = match compiler {
        CompilerKind::Posix(name) => {
            let exe = dir.join(exe_name);
            let mut cmd = Command::new(name);
            cmd.current_dir(dir).args(["-std=c11", "-Wall", "-O2", "-o"]).arg(&exe);
            for d in defines {
                cmd.arg(format!("-D{d}"));
            }
            let out = cmd
                .args(source_names)
                .arg("-I.")
                // Float intrinsics call `<math.h>`; glibc needs an explicit -lm.
                .arg("-lm")
                .output()
                .map_err(|e| format!("invoking {name}: {e}"))?;
            (out, name.to_string())
        }
        #[cfg(windows)]
        CompilerKind::Msvc(vcvars) => {
            let mut cmdline = format!(
                "\"{}\" >NUL 2>&1 && cl /nologo /std:c11 /W3 /O2 /Fe:{exe_name}",
                vcvars.display()
            );
            for d in defines {
                cmdline.push_str(&format!(" /D{d}"));
            }
            for s in source_names {
                cmdline.push(' ');
                cmdline.push_str(s);
            }
            cmdline.push_str(" /I.");
            use std::os::windows::process::CommandExt as _;
            let out = Command::new("cmd")
                .current_dir(dir)
                .arg("/S")
                .arg("/C")
                .raw_arg(format!("\"{cmdline}\""))
                .output()
                .map_err(|e| format!("invoking cl via vcvars64: {e}"))?;
            (out, "cl (MSVC via vcvars64)".to_string())
        }
    };
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(format!("[{desc}] compile failed:\n{log}"));
    }
    Ok(format!("[{desc}] compiled {exe_name}\n{log}"))
}

fn compile_model(project: &ol_ir::Project, node_name: &str) -> Result<CompiledModel, String> {
    // Selected-root generation: compile only the node under test and what it
    // transitively uses, exactly as the production emit path does.
    let project = &project.slice_for_root(node_name)?;
    let node = project
        .find_node(node_name)
        .ok_or_else(|| format!("node `{node_name}` not found"))?;

    let bundle = ol_clite_emit::emit_project(project);
    let has_contract = node.contract.is_some();
    let driver = if has_contract {
        ol_clite_emit::harness::emit_csv_driver_with_monitor(
            node,
            node.contract.as_deref(),
            project,
        )
    } else {
        ol_clite_emit::harness::emit_csv_driver(node, project)
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

    let exe_name = if cfg!(windows) { "model_under_test.exe" } else { "model_under_test" };
    let exe = d.join(exe_name);
    let compiler = find_compiler()
        .ok_or_else(|| "no C compiler found (cc/gcc/clang on PATH, or MSVC)".to_string())?;
    let cc = match compiler {
        CompilerKind::Posix(name) => Command::new(name)
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
            // Float intrinsics call `<math.h>`; glibc needs an explicit -lm.
            .arg("-lm")
            .output()
            .map_err(|e| format!("invoking {name}: {e}"))?,
        #[cfg(windows)]
        CompilerKind::Msvc(vcvars) => {
            // cl.exe needs the vcvars environment and writes .obj files into
            // the working directory, so run `vcvars64.bat && cl` through cmd
            // inside the temp dir with bare file names.
            let mut cmdline = format!(
                "\"{}\" >NUL 2>&1 && cl /nologo /std:c11 /W3 /Fe:{exe_name}",
                vcvars.display()
            );
            for s in &sources {
                let fname = s.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                cmdline.push(' ');
                cmdline.push_str(fname);
            }
            cmdline.push_str(" /I.");
            use std::os::windows::process::CommandExt as _;
            Command::new("cmd")
                .current_dir(d)
                .arg("/S")
                .arg("/C")
                .raw_arg(format!("\"{cmdline}\""))
                .output()
                .map_err(|e| format!("invoking cl via vcvars64: {e}"))?
        }
    };
    if !cc.status.success() {
        // MSVC reports errors on stdout, GNU-style compilers on stderr.
        return Err(format!(
            "{}{}",
            String::from_utf8_lossy(&cc.stdout),
            String::from_utf8_lossy(&cc.stderr)
        ));
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
