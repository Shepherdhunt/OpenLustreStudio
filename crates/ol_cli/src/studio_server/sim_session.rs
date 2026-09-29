//! Interactive simulation sessions — the Studio's Simulation dock.
//!
//! A session keeps ONE live simulator for the chosen root operator across
//! requests, so a Step costs one cycle instead of a replay from cycle 0, and
//! Run N / run-until-breakpoint / stop-on-violation are cheap. The simulator
//! borrows the project it runs, so a session is a dedicated thread that owns
//! both and executes commands sent over a channel; dropping the session drops
//! the channel, which ends the thread.
//!
//! A session can also run the generated C in the loop: the compiled CSV
//! driver of the root is started as a child process and fed the same inputs
//! each cycle, one line at a time, and its outputs (and contract monitor
//! columns) are compared with the simulator's as they come — so a divergence
//! between model and code shows on the cycle it happens. Attaching mid-session
//! replays the session's input history first, so both are in the same state.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use ol_ir::{Project, Type, TypeBody};
use ol_sim::{Sim, Value};

use crate::scenario::CompiledModel;

/// One cycle-stepping request (see [`SimSession::run`]).
pub struct RunReq {
    /// Input values as text (watch-table / CSV syntax), held for every cycle
    /// of this run.
    pub inputs: BTreeMap<String, String>,
    /// Per-cycle inputs (a replayed scenario or counterexample): when not
    /// empty, cycle `k` of the run uses `inputs` overridden by entry `k`, and
    /// the run is as long as the sequence.
    pub sequence: Vec<BTreeMap<String, String>>,
    /// Cycles to run at most (ignored when `sequence` is given).
    pub count: usize,
    /// Stop after the first cycle on which this condition holds.
    pub brk: Option<ol_ir::Expr>,
    /// Stop after the first cycle with a contract violation.
    pub stop_on_violation: bool,
    /// Stop after the first cycle on which the C in the loop disagrees with
    /// the simulator.
    pub stop_on_divergence: bool,
}

struct RunOut {
    rows: Vec<serde_json::Value>,
    stopped: &'static str,
    error: Option<String>,
    /// The C in the loop failed (crashed, hung, or spoke an unexpected
    /// format) and was detached.
    c_error: Option<String>,
}

/// What attaching the C in the loop found: its columns, and the replayed
/// history compared cycle by cycle.
pub struct AttachOut {
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub diverged_at: Option<usize>,
}

enum Cmd {
    Run(RunReq, mpsc::Sender<Result<RunOut, String>>),
    AttachC(Arc<CompiledModel>, mpsc::Sender<Result<AttachOut, String>>),
    DetachC(mpsc::Sender<()>),
}

pub struct SimSession {
    tx: mpsc::Sender<Cmd>,
    /// The operator being simulated.
    pub root: String,
    /// Semantic fingerprint of the model the session was started on; a
    /// different one means the model changed and the session is stale.
    pub signature: u64,
    /// Cycles run so far.
    pub cycle: usize,
    /// `[{name, kind: input|local|output, type}]`, in row-value order.
    pub signals: serde_json::Value,
    /// Whether the root has a contract (rows then carry modes / violations).
    pub monitored: bool,
    /// The C in the loop's compared columns while it is attached.
    pub c_columns: Option<Vec<String>>,
}

impl SimSession {
    /// Start a session simulating `root` of `project` (already loaded the way
    /// a build sees it: stdlib merged, constructs lowered).
    pub fn start(project: Project, root: String, signature: u64) -> Result<SimSession, String> {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(serde_json::Value, bool), String>>();
        let root_t = root.clone();
        std::thread::spawn(move || {
            let project = project;
            let sim = match Sim::new(&project, &root_t) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            let node = sim.node;
            let sig = |name: &str, kind: &str, ty: &Type| {
                serde_json::json!({ "name": name, "kind": kind, "type": super::type_str(ty) })
            };
            let signals: Vec<serde_json::Value> = node
                .inputs
                .iter()
                .map(|p| sig(&p.name, "input", &p.ty))
                .chain(node.locals.iter().map(|l| sig(&l.name, "local", &l.ty)))
                .chain(node.outputs.iter().map(|p| sig(&p.name, "output", &p.ty)))
                .collect();
            let monitored = node.contract.is_some();
            let _ = ready_tx.send(Ok((serde_json::Value::Array(signals), monitored)));
            let mut runner = Runner {
                sim,
                project: &project,
                monitored,
                cycle: 0,
                history: Vec::new(),
                history_full: false,
                c: None,
            };
            for cmd in rx {
                match cmd {
                    Cmd::Run(req, reply) => {
                        let _ = reply.send(runner.run(req));
                    }
                    Cmd::AttachC(exe, reply) => {
                        let _ = reply.send(runner.attach(exe));
                    }
                    Cmd::DetachC(reply) => {
                        runner.c = None;
                        let _ = reply.send(());
                    }
                }
            }
        });
        let (signals, monitored) = ready_rx
            .recv()
            .map_err(|_| "the simulation thread failed to start".to_string())??;
        Ok(SimSession { tx, root, signature, cycle: 0, signals, monitored, c_columns: None })
    }

    /// Run up to `req.count` cycles; the response carries one row per cycle
    /// run (`{cycle, values, modes, violations}`, plus `c` / `c_diff` while
    /// the C is in the loop), why it stopped (`count` | `break` | `violation`
    /// | `divergence` | `c_error` | `error`) and any error.
    pub fn run(&mut self, req: RunReq) -> Result<serde_json::Value, String> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Cmd::Run(req, rtx)).map_err(|_| ended())?;
        let out = rrx.recv().map_err(|_| ended())??;
        self.cycle += out.rows.len();
        if out.c_error.is_some() {
            self.c_columns = None;
        }
        Ok(serde_json::json!({
            "cycle": self.cycle,
            "rows": out.rows,
            "stopped": out.stopped,
            "error": out.error,
            "c_error": out.c_error,
        }))
    }

    /// Put the compiled root in the loop (replacing any attached one): the
    /// session's history is replayed through it first and compared.
    pub fn attach_c(&mut self, exe: Arc<CompiledModel>) -> Result<AttachOut, String> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Cmd::AttachC(exe, rtx)).map_err(|_| ended())?;
        let out = rrx.recv().map_err(|_| ended())??;
        self.c_columns = Some(out.columns.clone());
        Ok(out)
    }

    pub fn detach_c(&mut self) {
        let (rtx, rrx) = mpsc::channel();
        if self.tx.send(Cmd::DetachC(rtx)).is_ok() {
            let _ = rrx.recv();
        }
        self.c_columns = None;
    }
}

fn ended() -> String {
    "the simulation session ended — start it again".to_string()
}

/// Cycles of input history kept for attaching the C mid-session.
const HISTORY_MAX: usize = 100_000;
/// Replayed rows returned by an attach (the client keeps this many).
const ATTACH_ROWS_MAX: usize = 10_000;
/// How long the C in the loop may take to answer one cycle.
const C_TIMEOUT: Duration = Duration::from_secs(5);

/// The session thread's state.
struct Runner<'a> {
    sim: Sim<'a>,
    project: &'a Project,
    monitored: bool,
    cycle: usize,
    /// Every cycle so far: the C driver's input line and the simulator's
    /// values for the C's columns — what an attach replays and compares.
    history: Vec<(String, Vec<String>)>,
    /// The history hit [`HISTORY_MAX`]; attaching needs a Reset first.
    history_full: bool,
    c: Option<CLoop>,
}

impl Runner<'_> {
    /// Step with the held (or sequenced) inputs, record every cycle, stop
    /// early on a breakpoint, a violation or a C divergence (as asked). Every
    /// cycle's inputs are parsed before the first one runs, so a bad value
    /// anywhere in a sequence runs nothing.
    fn run(&mut self, req: RunReq) -> Result<RunOut, String> {
        let empty = BTreeMap::new();
        let plan: Vec<&BTreeMap<String, String>> = if req.sequence.is_empty() {
            vec![&empty; req.count]
        } else {
            req.sequence.iter().collect()
        };
        let mut cycles_inputs = Vec::with_capacity(plan.len());
        for (k, over) in plan.iter().enumerate() {
            let at = if req.sequence.is_empty() { String::new() } else { format!("step {k}: ") };
            let mut inputs = BTreeMap::new();
            for p in &self.sim.node.inputs {
                let raw = over
                    .get(&p.name)
                    .or_else(|| req.inputs.get(&p.name))
                    .ok_or_else(|| format!("{at}no value given for input `{}`", p.name))?;
                let v = self.sim.parse_input(&p.name, raw).map_err(|e| format!("{at}{e}"))?;
                inputs.insert(p.name.clone(), v);
            }
            // Held inputs are the same every cycle: parse them once.
            cycles_inputs.push(inputs);
            if req.sequence.is_empty() {
                break;
            }
        }
        let mut out = RunOut { rows: Vec::new(), stopped: "count", error: None, c_error: None };
        for k in 0..plan.len() {
            let inputs = &cycles_inputs[k.min(cycles_inputs.len() - 1)];
            let obs = match self.sim.step_observed(inputs) {
                Ok(o) => o,
                Err(e) => {
                    out.stopped = "error";
                    out.error = Some(format!("cycle {}: {e}", self.cycle));
                    return Ok(out);
                }
            };
            let values: BTreeMap<String, Value> = obs.values.iter().cloned().collect();
            let line = self.c_input_line(inputs);
            let ir_cells = self.compared_cells(&values, &obs.active_modes, &obs.violations);
            let mut row = serde_json::json!({
                "cycle": self.cycle,
                "values": obs.values.iter().map(|(_, v)| v.to_csv()).collect::<Vec<_>>(),
                "modes": obs.active_modes,
                "violations": obs.violations,
            });
            let mut diverged = false;
            if let Some(c) = self.c.as_mut() {
                match c.step(&line, &ir_cells) {
                    Ok((cells, diff)) => {
                        diverged = !diff.is_empty();
                        row["c"] = serde_json::json!(cells);
                        row["c_diff"] = serde_json::json!(diff);
                    }
                    Err(e) => {
                        out.c_error = Some(format!("cycle {}: {e} — the C program was taken out of the loop", self.cycle));
                        self.c = None;
                    }
                }
            }
            if self.history.len() < HISTORY_MAX {
                self.history.push((line, ir_cells));
            } else {
                self.history_full = true;
            }
            out.rows.push(row);
            self.cycle += 1;
            if req.stop_on_divergence && diverged {
                out.stopped = "divergence";
                return Ok(out);
            }
            if req.stop_on_violation && !obs.violations.is_empty() {
                out.stopped = "violation";
                return Ok(out);
            }
            if let Some(b) = &req.brk {
                match self.sim.eval_condition(b, &values) {
                    Ok(true) => {
                        out.stopped = "break";
                        return Ok(out);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        out.stopped = "error";
                        out.error = Some(format!("breakpoint: {e}"));
                        return Ok(out);
                    }
                }
            }
            if out.c_error.is_some() {
                out.stopped = "c_error";
                return Ok(out);
            }
        }
        Ok(out)
    }

    /// One cycle's inputs as the C driver reads them: in port order, enum
    /// values by their index (the driver parses enums as integers).
    fn c_input_line(&self, inputs: &BTreeMap<String, Value>) -> String {
        self.sim
            .node
            .inputs
            .iter()
            .map(|p| match (inputs.get(&p.name), enum_variants(self.project, &p.ty)) {
                (Some(Value::Enum(v)), Some(vs)) => vs.iter().position(|x| x == v).unwrap_or(0).to_string(),
                (Some(v), _) => v.to_csv(),
                (None, _) => String::new(),
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The simulator's values for the C driver's columns: the outputs, then
    /// the contract monitor's mode and violation cells.
    fn compared_cells(&self, values: &BTreeMap<String, Value>, modes: &[String], violations: &[String]) -> Vec<String> {
        let mut cells: Vec<String> = self
            .sim
            .node
            .outputs
            .iter()
            .map(|p| values.get(&p.name).map(|v| v.to_csv()).unwrap_or_default())
            .collect();
        if self.monitored {
            cells.push(ol_sim::label_list(modes));
            cells.push(ol_sim::label_list(violations));
        }
        cells
    }

    /// Start the compiled program, check it speaks the expected columns, and
    /// replay the session so far through it.
    fn attach(&mut self, exe: Arc<CompiledModel>) -> Result<AttachOut, String> {
        if self.history_full {
            return Err(format!(
                "the session is past {HISTORY_MAX} cycles — Reset, then put the C in the loop"
            ));
        }
        self.c = None;
        let node = self.sim.node;
        let mut columns: Vec<(String, Cmp)> = node
            .outputs
            .iter()
            .map(|p| (p.name.clone(), Cmp::of(self.project, &p.ty)))
            .collect();
        if self.monitored {
            columns.push(("active_mode".into(), Cmp::Text));
            columns.push(("violations".into(), Cmp::Text));
        }
        let header = node.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(",");
        let mut c = CLoop::spawn(exe.exe(), exe.clone(), &header, columns)?;
        let mut rows = Vec::new();
        let mut diverged_at = None;
        let skip = self.history.len().saturating_sub(ATTACH_ROWS_MAX);
        for (k, (line, ir_cells)) in self.history.iter().enumerate() {
            let (cells, diff) = c.step(line, ir_cells).map_err(|e| format!("replaying cycle {k}: {e}"))?;
            if diverged_at.is_none() && !diff.is_empty() {
                diverged_at = Some(k);
            }
            if k >= skip {
                rows.push(serde_json::json!({ "cycle": k, "c": cells, "c_diff": diff }));
            }
        }
        let names = c.columns.iter().map(|(n, _)| n.clone()).collect();
        self.c = Some(c);
        Ok(AttachOut { columns: names, rows, diverged_at })
    }
}

/// An enum type's variants, in declaration (= C value) order.
fn enum_variants(project: &Project, ty: &Type) -> Option<Vec<String>> {
    let Type::Named { name } = ty else { return None };
    project.packages.iter().find_map(|p| match &p.find_type(name)?.body {
        TypeBody::Enum(e) => Some(e.variants.clone()),
        _ => None,
    })
}

/// How one C column is read back and compared with the simulator's cell.
enum Cmp {
    Text,
    /// `%g` output: equal within its six significant digits.
    Float,
    /// Printed as the variant's index; shown and compared by name.
    Enum(Vec<String>),
    /// `[e0;e1;…]`, element-wise.
    Array(Box<Cmp>),
}

impl Cmp {
    fn of(project: &Project, ty: &Type) -> Cmp {
        match ty {
            Type::Float32 | Type::Float64 => Cmp::Float,
            Type::Array { elem, .. } => Cmp::Array(Box::new(Cmp::of(project, elem))),
            t => enum_variants(project, t).map(Cmp::Enum).unwrap_or(Cmp::Text),
        }
    }

    /// The C cell as the simulator would write it, and whether they agree.
    fn read(&self, c: &str, ir: &str) -> (String, bool) {
        match self {
            Cmp::Text => (c.to_string(), c == ir),
            Cmp::Float => {
                let same = match (c.parse::<f64>(), ir.parse::<f64>()) {
                    (Ok(a), Ok(b)) => a == b || (a - b).abs() <= 1e-5 * a.abs().max(b.abs()) || (a.is_nan() && b.is_nan()),
                    _ => c == ir,
                };
                (c.to_string(), same)
            }
            Cmp::Enum(vs) => {
                let name = c.parse::<usize>().ok().and_then(|i| vs.get(i)).cloned().unwrap_or_else(|| c.to_string());
                let same = name == ir;
                (name, same)
            }
            Cmp::Array(elem) => {
                let inner = |s: &str| -> Vec<String> {
                    s.trim_start_matches('[').trim_end_matches(']').split(';').map(str::to_string).collect()
                };
                let (cs, is) = (inner(c), inner(ir));
                let mut same = cs.len() == is.len();
                let mut shown = Vec::new();
                for (k, ce) in cs.iter().enumerate() {
                    let (t, ok) = elem.read(ce, is.get(k).map(String::as_str).unwrap_or(""));
                    same &= ok;
                    shown.push(t);
                }
                (format!("[{}]", shown.join(";")), same)
            }
        }
    }
}

/// The compiled CSV driver running as a child process, driven a line at a
/// time. Killed when dropped.
struct CLoop {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    columns: Vec<(String, Cmp)>,
    /// Keeps the executable (its build directory) alive while it runs.
    _keep: Arc<dyn Send + Sync>,
}

impl CLoop {
    fn spawn(
        exe: &std::path::Path,
        keep: Arc<dyn Send + Sync>,
        header: &str,
        columns: Vec<(String, Cmp)>,
    ) -> Result<CLoop, String> {
        let mut child = Command::new(exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting the compiled program: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin for the compiled program")?;
        let stdout = child.stdout.take().ok_or("no stdout for the compiled program")?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut c = CLoop { child, stdin, lines, columns, _keep: keep };
        c.send(header)?;
        let got = c.recv()?;
        let want: Vec<&str> = std::iter::once("cycle").chain(c.columns.iter().map(|(n, _)| n.as_str())).collect();
        if got.split(',').collect::<Vec<_>>() != want {
            return Err(format!("the compiled program's columns are `{got}`, expected `{}`", want.join(",")));
        }
        Ok(c)
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.stdin, "{line}")
            .and_then(|_| self.stdin.flush())
            .map_err(|_| self.gone())
    }

    fn recv(&mut self) -> Result<String, String> {
        match self.lines.recv_timeout(C_TIMEOUT) {
            Ok(l) => Ok(l),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("the compiled program gave no answer within {} s", C_TIMEOUT.as_secs()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.gone()),
        }
    }

    fn gone(&mut self) -> String {
        match self.child.try_wait() {
            Ok(Some(status)) => format!("the compiled program exited ({status})"),
            _ => "the compiled program closed its output".to_string(),
        }
    }

    /// One cycle: send the inputs, read the outputs; returns the C's cells
    /// (as the simulator would write them) and the columns that differ.
    fn step(&mut self, line: &str, ir_cells: &[String]) -> Result<(Vec<String>, Vec<String>), String> {
        self.send(line)?;
        let got = self.recv()?;
        let parts: Vec<&str> = got.split(',').collect();
        if parts.len() != self.columns.len() + 1 {
            return Err(format!("unexpected output line `{got}`"));
        }
        let mut cells = Vec::with_capacity(self.columns.len());
        let mut diff = Vec::new();
        for (k, (name, cmp)) in self.columns.iter().enumerate() {
            let (shown, same) = cmp.read(parts[k + 1], ir_cells.get(k).map(String::as_str).unwrap_or(""));
            if !same {
                diff.push(name.clone());
            }
            cells.push(shown);
        }
        Ok((cells, diff))
    }
}

impl Drop for CLoop {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_compare_as_the_c_prints_them() {
        // `%g` keeps six significant digits.
        assert!(Cmp::Float.read("0.3", "0.30000000000000004").1);
        assert!(Cmp::Float.read("1e+06", "1000000").1);
        assert!(!Cmp::Float.read("0.3", "0.31").1);
        // Enums travel as their index and come back by name.
        let gear = Cmp::Enum(vec!["Park".into(), "Drive".into()]);
        assert_eq!(gear.read("1", "Drive"), ("Drive".to_string(), true));
        assert_eq!(gear.read("0", "Drive"), ("Park".to_string(), false));
        let arr = Cmp::Array(Box::new(Cmp::Float));
        assert!(arr.read("[0.3;1]", "[0.30000000000000004;1]").1);
        assert!(!arr.read("[0.3]", "[0.3;1]").1, "length mismatch");
        assert!(!Cmp::Text.read("44", "300").1);
    }

    /// A stand-in for a compiled driver: a shell script.
    #[cfg(unix)]
    fn script(name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("ol_cloop_{name}_{stamp}.sh"));
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_differing_answer_is_a_divergence_and_a_dead_program_an_error() {
        let cols = || vec![("y".to_string(), Cmp::Text)];
        // Always answers y = 7.
        let seven = script("seven", "read h\necho cycle,y\nk=0\nwhile read l; do echo \"$k,7\"; k=$((k+1)); done\n");
        let mut c = CLoop::spawn(&seven, Arc::new(()), "x", cols()).expect("spawn");
        assert_eq!(c.step("1", &["7".into()]).unwrap(), (vec!["7".to_string()], vec![]));
        assert_eq!(c.step("2", &["8".into()]).unwrap(), (vec!["7".to_string()], vec!["y".to_string()]));
        drop(c);
        // Wrong columns are refused up front.
        let r = CLoop::spawn(&seven, Arc::new(()), "x", vec![("z".to_string(), Cmp::Text)]);
        assert!(r.err().unwrap().contains("columns"), "column check");
        // Exits after the header: the first step reports it.
        let quits = script("quits", "read h\necho cycle,y\nexit 3\n");
        let mut c = CLoop::spawn(&quits, Arc::new(()), "x", cols()).expect("spawn");
        let e = c.step("1", &["7".into()]).unwrap_err();
        assert!(e.contains("exited") || e.contains("closed"), "{e}");
        let _ = std::fs::remove_file(seven);
        let _ = std::fs::remove_file(quits);
    }
}
