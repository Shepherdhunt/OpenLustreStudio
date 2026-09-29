//! Interactive simulation sessions — the Studio's Simulation dock.
//!
//! A session keeps ONE live simulator for the chosen root operator across
//! requests, so a Step costs one cycle instead of a replay from cycle 0, and
//! Run N / run-until-breakpoint / stop-on-violation are cheap. The simulator
//! borrows the project it runs, so a session is a dedicated thread that owns
//! both and executes commands sent over a channel; dropping the session drops
//! the channel, which ends the thread.

use std::collections::BTreeMap;
use std::sync::mpsc;

use ol_sim::{Sim, Value};

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
}

struct RunOut {
    rows: Vec<serde_json::Value>,
    stopped: &'static str,
    error: Option<String>,
}

enum Cmd {
    Run(RunReq, mpsc::Sender<Result<RunOut, String>>),
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
}

impl SimSession {
    /// Start a session simulating `root` of `project` (already loaded the way
    /// a build sees it: stdlib merged, constructs lowered).
    pub fn start(project: ol_ir::Project, root: String, signature: u64) -> Result<SimSession, String> {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(serde_json::Value, bool), String>>();
        let root_t = root.clone();
        std::thread::spawn(move || {
            let project = project;
            let mut sim = match Sim::new(&project, &root_t) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            let node = sim.node;
            let sig = |name: &str, kind: &str, ty: &ol_ir::Type| {
                serde_json::json!({ "name": name, "kind": kind, "type": super::type_str(ty) })
            };
            let signals: Vec<serde_json::Value> = node
                .inputs
                .iter()
                .map(|p| sig(&p.name, "input", &p.ty))
                .chain(node.locals.iter().map(|l| sig(&l.name, "local", &l.ty)))
                .chain(node.outputs.iter().map(|p| sig(&p.name, "output", &p.ty)))
                .collect();
            let _ = ready_tx.send(Ok((serde_json::Value::Array(signals), node.contract.is_some())));
            let mut cycle = 0usize;
            for cmd in rx {
                match cmd {
                    Cmd::Run(req, reply) => {
                        let _ = reply.send(run(&mut sim, &mut cycle, req));
                    }
                }
            }
        });
        let (signals, monitored) = ready_rx
            .recv()
            .map_err(|_| "the simulation thread failed to start".to_string())??;
        Ok(SimSession { tx, root, signature, cycle: 0, signals, monitored })
    }

    /// Run up to `req.count` cycles; the response carries one row per cycle
    /// run (`{cycle, values, modes, violations}`), why it stopped
    /// (`count` | `break` | `violation` | `error`) and any error.
    pub fn run(&mut self, req: RunReq) -> Result<serde_json::Value, String> {
        let ended = || "the simulation session ended — start it again".to_string();
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Cmd::Run(req, rtx)).map_err(|_| ended())?;
        let out = rrx.recv().map_err(|_| ended())??;
        self.cycle += out.rows.len();
        Ok(serde_json::json!({
            "cycle": self.cycle,
            "rows": out.rows,
            "stopped": out.stopped,
            "error": out.error,
        }))
    }
}

/// The session thread's side of a run: step with the held (or sequenced)
/// inputs, record every cycle, stop early on a breakpoint or (if asked) a
/// violation. Every cycle's inputs are parsed before the first one runs, so a
/// bad value anywhere in a sequence runs nothing.
fn run(sim: &mut Sim, cycle: &mut usize, req: RunReq) -> Result<RunOut, String> {
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
        for p in &sim.node.inputs {
            let raw = over
                .get(&p.name)
                .or_else(|| req.inputs.get(&p.name))
                .ok_or_else(|| format!("{at}no value given for input `{}`", p.name))?;
            let v = sim.parse_input(&p.name, raw).map_err(|e| format!("{at}{e}"))?;
            inputs.insert(p.name.clone(), v);
        }
        // Held inputs are the same every cycle: parse them once.
        cycles_inputs.push(inputs);
        if req.sequence.is_empty() {
            break;
        }
    }
    let mut rows = Vec::new();
    for k in 0..plan.len() {
        let inputs = &cycles_inputs[k.min(cycles_inputs.len() - 1)];
        let obs = match sim.step_observed(inputs) {
            Ok(o) => o,
            Err(e) => {
                return Ok(RunOut { rows, stopped: "error", error: Some(format!("cycle {}: {e}", *cycle)) })
            }
        };
        rows.push(serde_json::json!({
            "cycle": *cycle,
            "values": obs.values.iter().map(|(_, v)| v.to_csv()).collect::<Vec<_>>(),
            "modes": obs.active_modes,
            "violations": obs.violations,
        }));
        *cycle += 1;
        if req.stop_on_violation && !obs.violations.is_empty() {
            return Ok(RunOut { rows, stopped: "violation", error: None });
        }
        if let Some(b) = &req.brk {
            let values: BTreeMap<String, Value> = obs.values.into_iter().collect();
            match sim.eval_condition(b, &values) {
                Ok(true) => return Ok(RunOut { rows, stopped: "break", error: None }),
                Ok(false) => {}
                Err(e) => {
                    return Ok(RunOut { rows, stopped: "error", error: Some(format!("breakpoint: {e}")) })
                }
            }
        }
    }
    Ok(RunOut { rows, stopped: "count", error: None })
}
