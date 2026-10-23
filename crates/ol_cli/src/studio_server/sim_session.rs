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
    pub brk: Option<Breakpoint>,
    /// Stop after the first cycle with a contract violation.
    pub stop_on_violation: bool,
    /// Stop after the first cycle on which the C in the loop disagrees with
    /// the simulator.
    pub stop_on_divergence: bool,
    /// FUZZ mode: generate every input's value instead of using `inputs` /
    /// `sequence` (see [`FuzzCfg`]).
    pub fuzz: Option<FuzzCfg>,
}

/// How a fuzzing run generates inputs (the Simulation dock's FUZZ toggle).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FuzzMode {
    /// Valid values covering each input's full declared data-type range —
    /// bools, every integer width across its whole span, finite reals across
    /// the full float range (subnormals included), enum variants, struct
    /// fields and array elements.
    Range,
    /// Bad-data injection: one input per cycle receives malformed value text
    /// (out-of-range numbers, NaN, wrong enum names, broken struct syntax)
    /// while the rest stay valid. The run reports how each injection was
    /// refused — the input-validation contract under attack.
    Bad,
}

impl FuzzMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FuzzMode::Range => "range",
            FuzzMode::Bad => "bad",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FuzzCfg {
    pub mode: FuzzMode,
    /// The generator seed; the same seed replays the same values, so a fuzz
    /// finding can be saved as a TestScenario and repeated exactly.
    pub seed: u64,
}

/// splitmix64 — small, deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A finite f32 from raw bits (NaN / ±Inf redrawn): the whole finite
    /// float32 range, subnormals included.
    fn f32_all(&mut self) -> f32 {
        loop {
            let v = f32::from_bits(self.next_u64() as u32);
            if v.is_finite() {
                return v;
            }
        }
    }

    /// A finite f64 from raw bits.
    fn f64_all(&mut self) -> f64 {
        loop {
            let v = f64::from_bits(self.next_u64());
            if v.is_finite() {
                return v;
            }
        }
    }
}

/// A valid value anywhere in `ty`'s declared range.
fn fuzz_value(ty: &Type, project: &Project, rng: &mut Rng) -> Result<Value, String> {
    Ok(match ty {
        Type::Bool => Value::Bool(rng.next_u64() & 1 == 1),
        Type::Int8 => Value::Int((rng.next_u64() & 0xFF) as u8 as i8 as i64),
        Type::Int16 => Value::Int((rng.next_u64() & 0xFFFF) as u16 as i16 as i64),
        Type::Int32 => Value::Int((rng.next_u64() & 0xFFFF_FFFF) as u32 as i32 as i64),
        Type::Int64 => Value::Int(rng.next_u64() as i64),
        // uint64 spans 0..=u64::MAX in the model, but simulator storage (and
        // the JSON API) holds signed 64-bit — the supported span is sampled.
        Type::Uint8 => Value::Int((rng.next_u64() & 0xFF) as i64),
        Type::Uint16 => Value::Int((rng.next_u64() & 0xFFFF) as i64),
        Type::Uint32 => Value::Int((rng.next_u64() & 0xFFFF_FFFF) as i64),
        Type::Uint64 => Value::Int((rng.next_u64() & i64::MAX as u64) as i64),
        Type::Float32 => Value::Float32(rng.f32_all()),
        Type::Float64 => Value::Float(rng.f64_all()),
        Type::Char => Value::Int((rng.next_u64() & 0xFF) as i64),
        Type::Array { elem, len } => Value::Array(
            (0..*len).map(|_| fuzz_value(elem, project, rng)).collect::<Result<_, _>>()?,
        ),
        Type::Named { name } => match project.packages.iter().find_map(|p| p.find_type(name)).map(|t| &t.body) {
            Some(TypeBody::Enum(e)) => {
                if e.variants.is_empty() {
                    return Err(format!("enum `{name}` has no variants to sample"));
                }
                Value::Enum(e.variants[rng.next_u64() as usize % e.variants.len()].clone())
            }
            Some(TypeBody::Record { fields, .. }) => Value::Record(
                fields
                    .iter()
                    .map(|f| Ok((f.name.clone(), fuzz_value(&f.ty, project, rng)?)))
                    .collect::<Result<std::collections::BTreeMap<_, _>, String>>()?,
            ),
            Some(TypeBody::Alias { target, .. }) => fuzz_value(target, project, rng)?,
            None => return Err(format!("type `{name}` is not declared in the model")),
        },
    })
}

/// One menu entry, chosen by the generator.
fn pick_menu(menu: &[&str], rng: &mut Rng) -> String {
    menu[rng.next_u64() as usize % menu.len()].to_string()
}

/// One malformed value text for `ty`, drawn from a per-type menu of classic
/// bad inputs (out-of-range, wrong kind, broken syntax).
fn bad_text(ty: &Type, project: &Project, rng: &mut Rng) -> String {
    match ty {
        Type::Bool => pick_menu(&["yes", "2", "", "TRUE OR FALSE"], rng),
        Type::Int8 => pick_menu(&["128", "-129", "1.5", "abc", ""], rng),
        Type::Int16 => pick_menu(&["32768", "-32769", "0x10", "12,5"], rng),
        Type::Int32 => pick_menu(&["2147483648", "-2147483649", "4 GB", ""], rng),
        Type::Int64 => pick_menu(&["9223372036854775808", "--5", "e3", ""], rng),
        Type::Uint8 => pick_menu(&["256", "-1", "1.0", "0xFF", ""], rng),
        Type::Uint16 => pick_menu(&["65536", "-2", "1e3", ""], rng),
        Type::Uint32 => pick_menu(&["4294967296", "-3", "twelve", ""], rng),
        Type::Uint64 => pick_menu(&["18446744073709551616", "-4", "6.02e23", ""], rng),
        Type::Float32 | Type::Float64 => pick_menu(&["NaN", "Inf", "-Infinity", "1e999", "abc", ""], rng),
        Type::Char => pick_menu(&["256", "-1", "abc", ""], rng),
        Type::Array { len, .. } => {
            if *len == 2 {
                pick_menu(&["[1", "1;2", "[]", "[0;0;0]"], rng)
            } else {
                pick_menu(&["[1", "1;2", "[]", "[0;0]"], rng)
            }
        }
        Type::Named { name } => match project.packages.iter().find_map(|p| p.find_type(name)).map(|t| &t.body) {
            Some(TypeBody::Enum(_)) => pick_menu(&["", "NotAVariant", "maybe", "3"], rng),
            Some(TypeBody::Record { .. }) => pick_menu(&["{x}", "{}", "null", "3", "{a=1;}", "[1;2]"], rng),
            Some(TypeBody::Alias { target, .. }) => bad_text(target, project, rng),
            None => "?undeclared".to_string(),
        },
    }
}

/// A checked, per-run namespace for a breakpoint. Its auxiliary project
/// contains only the requested globals and their dependencies; the live
/// model's constructor and state remain isolated from unrelated declarations.
pub struct Breakpoint {
    pub expr: ol_ir::Expr,
    pub project: Project,
    pub node: String,
}

struct RunOut {
    rows: Vec<serde_json::Value>,
    stopped: &'static str,
    error: Option<String>,
    /// Cycles the simulator actually stepped (bad-data fuzz attempts that
    /// were refused run nothing, so they don't advance the session).
    cycles_run: usize,
    /// FUZZ echo: `{mode, seed, injected, refused}` for the session note.
    fuzz: Option<serde_json::Value>,
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
    /// Signal names, roles and resolved typed schemas, in row-value order.
    /// Inputs also include editable placeholder values (never measurements).
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
                let mut signal = serde_json::json!({
                    "name": name, "kind": kind, "type": super::type_str(ty),
                    "schema": ol_sim::input_schema(ty, &project)?,
                });
                if kind == "input" {
                    signal["placeholder"] = serde_json::json!(true);
                    signal["placeholder_note"] = serde_json::json!(
                        "Placeholder values for editing: zero numbers, false Booleans and an unambiguous neutral enum variant when available. Enter values for your test; these are not sensor measurements or a validated configuration."
                    );
                    match ol_sim::input_placeholder(ty, &project) {
                        Ok(value) => {
                            signal["default_value"] = value_json(&value);
                            signal["default_text"] = serde_json::json!(value.to_csv());
                        }
                        Err(error) => {
                            signal["default_value"] = serde_json::Value::Null;
                            signal["default_text"] = serde_json::Value::Null;
                            signal["default_error"] = serde_json::json!(error);
                        }
                    }
                }
                Ok::<_, String>(signal)
            };
            let signals = node
                .inputs
                .iter()
                .map(|p| sig(&p.name, "input", &p.ty))
                .chain(node.locals.iter().map(|l| sig(&l.name, "local", &l.ty)))
                .chain(node.outputs.iter().map(|p| sig(&p.name, "output", &p.ty)))
                .collect::<Result<Vec<_>, _>>();
            let signals = match signals {
                Ok(signals) => signals,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
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
        self.cycle += out.cycles_run;
        if out.c_error.is_some() {
            self.c_columns = None;
        }
        Ok(serde_json::json!({
            "cycle": self.cycle,
            "rows": out.rows,
            "stopped": out.stopped,
            "error": out.error,
            "c_error": out.c_error,
            "fuzz": out.fuzz,
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

/// Preserve struct/array shape and enum names at the JSON API boundary.
fn value_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Bool(v) => serde_json::json!(v),
        Value::Int(v) => serde_json::json!(v),
        Value::Float(v) => serde_json::json!(v),
        Value::Float32(v) => serde_json::json!(v),
        Value::Record(fields) => serde_json::Value::Object(fields.iter()
            .map(|(name, value)| (name.clone(), value_json(value))).collect()),
        Value::Array(values) | Value::Tuple(values) => serde_json::Value::Array(values.iter().map(value_json).collect()),
        Value::Enum(name) | Value::ModeLabel(name) => serde_json::json!(name),
    }
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
    /// anywhere in a sequence runs nothing. A FUZZ mode replaces the held /
    /// sequenced inputs with generated ones (see [`FuzzMode`]); in bad-data
    /// mode a refused injection is recorded and the model is not stepped.
    fn run(&mut self, req: RunReq) -> Result<RunOut, String> {
        // Resolve requested constants before the first main cycle. This is a
        // separate evaluator: breakpoint-only globals never enter model state.
        let breakpoint_sim = req.brk.as_ref().map(|b| Sim::new(&b.project, &b.node))
            .transpose().map_err(|e| format!("breakpoint: {e}"))?;
        let mut rng = req.fuzz.map(|c| Rng::new(c.seed));
        let empty = BTreeMap::new();
        let plan: Vec<&BTreeMap<String, String>> = if req.sequence.is_empty() || req.fuzz.is_some() {
            vec![&empty; req.count]
        } else {
            req.sequence.iter().collect()
        };
        // Bad-data fuzzing: what each cycle injected and what became of it.
        #[derive(Clone)]
        enum Inject {
            None,
            Accepted { name: String, text: String },
            Refused { name: String, text: String, error: String },
        }
        let mut injected = vec![Inject::None; plan.len()];
        let mut cycles_inputs = Vec::with_capacity(plan.len());
        for (k, over) in plan.iter().enumerate() {
            let at = if req.sequence.is_empty() || req.fuzz.is_some() { String::new() } else { format!("step {k}: ") };
            if let (Some(cfg), Some(rng)) = (req.fuzz, rng.as_mut()) {
                // FUZZ generates every input's value; held/sequenced text is ignored.
                let mut inputs = BTreeMap::new();
                for p in &self.sim.node.inputs {
                    let v = fuzz_value(&p.ty, self.project, rng)?;
                    inputs.insert(p.name.clone(), v);
                }
                if cfg.mode == FuzzMode::Bad && !self.sim.node.inputs.is_empty() {
                    // One input per cycle receives malformed text; the rest stay valid.
                    let p = &self.sim.node.inputs[k % self.sim.node.inputs.len()];
                    let text = bad_text(&p.ty, self.project, rng);
                    match self.sim.parse_input(&p.name, &text) {
                        Ok(v) => {
                            inputs.insert(p.name.clone(), v);
                            injected[k] = Inject::Accepted { name: p.name.clone(), text };
                        }
                        Err(error) => {
                            injected[k] = Inject::Refused { name: p.name.clone(), text, error: error.to_string() };
                        }
                    }
                }
                cycles_inputs.push(inputs);
                continue;
            }
            for name in req.inputs.keys().chain(over.keys()) {
                if !self.sim.node.inputs.iter().any(|p| p.name == *name) {
                    return Err(format!("{at}unexpected input `{name}` for `{}`", self.sim.node.name));
                }
            }
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
        let mut out = RunOut { rows: Vec::new(), stopped: "count", error: None, cycles_run: 0,
                               fuzz: req.fuzz.map(|c| serde_json::json!({"mode": c.mode.as_str(), "seed": c.seed})),
                               c_error: None };
        let mut refused = 0usize;
        for k in 0..plan.len() {
            // A refused bad-data injection ran nothing; report it and move on.
            if let Inject::Refused { name, text, error } = &injected[k] {
                refused += 1;
                out.rows.push(serde_json::json!({
                    "cycle": self.cycle,
                    "values": serde_json::Value::Null,
                    "modes": [],
                    "violations": [],
                    "fuzz_bad": { "input": name, "text": text, "outcome": "refused", "error": error },
                }));
                continue;
            }
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
            // FUZZ rows carry what was actually fed, so a run can be replayed
            // and a finding saved as a TestScenario.
            if req.fuzz.is_some() {
                row["fuzz_inputs"] = serde_json::json!(
                    self.sim.node.inputs.iter()
                        .map(|p| (p.name.clone(), inputs.get(&p.name).map(Value::to_csv).unwrap_or_default()))
                        .collect::<BTreeMap<_, _>>()
                );
                if let Inject::Accepted { name, text } = &injected[k] {
                    row["fuzz_bad"] = serde_json::json!({
                        "input": name, "text": text, "outcome": "accepted",
                    });
                }
            }
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
            out.cycles_run += 1;
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
                match breakpoint_sim.as_ref().unwrap().eval_condition(&b.expr, &values) {
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
        if let Some(meta) = out.fuzz.as_mut() {
            meta["refused"] = serde_json::json!(refused);
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
    /// A real: the driver prints it as the simulator does (shortest
    /// round-trip digits), so equal means the same number — compared as
    /// numbers only so a notation difference can't hide or fake a mismatch.
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
                    (Ok(a), Ok(b)) => a == b || (a.is_nan() && b.is_nan()),
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
        // Reals compare as numbers, exactly: notation can differ, value can't.
        assert!(Cmp::Float.read("0.30000000000000004", "0.30000000000000004").1);
        assert!(!Cmp::Float.read("0.3", "0.30000000000000004").1);
        assert!(Cmp::Float.read("1e6", "1000000").1);
        assert!(Cmp::Float.read("NaN", "NaN").1);
        // Enums travel as their index and come back by name.
        let gear = Cmp::Enum(vec!["Park".into(), "Drive".into()]);
        assert_eq!(gear.read("1", "Drive"), ("Drive".to_string(), true));
        assert_eq!(gear.read("0", "Drive"), ("Park".to_string(), false));
        let arr = Cmp::Array(Box::new(Cmp::Float));
        assert!(arr.read("[0.5;1]", "[0.5;1]").1);
        assert!(!arr.read("[0.3;1]", "[0.30000000000000004;1]").1);
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
