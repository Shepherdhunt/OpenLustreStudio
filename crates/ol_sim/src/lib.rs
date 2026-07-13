//! OpenLustre Studio: cycle-accurate IR interpreter (Phase 6, plan Task 12).
//!
//! The simulator runs a node in isolation against a CSV input vector and
//! produces a CSV output trace plus contract-monitor results. Each cycle is
//! a single read-eval-write step over the IR — the same semantics the C-Lite
//! emitter targets.
//!
//! Stateful subnode calls are supported: every `Expr::Call` to a stateful
//! operator gets its own [`State`] keyed by the call expression's address in
//! the IR. This is sound because the [`Sim`] holds an immutable borrow of the
//! [`Project`] for its entire lifetime, so the expression pointers it stores
//! cannot be invalidated.

use std::collections::{BTreeMap, HashMap};

pub mod fuzz;

use ol_contract_ir::{parse_contracts, ContractDef};
use ol_ir::{BinOp, Expr, IterKind, Literal, NodeDef, NodeKind, Project, Type, TypeBody, UnaryOp};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Fixed-point value: the Q-format stored integer `round(real·2^frac)` plus
    /// its format. Self-describing so the dynamic evaluator applies fixed
    /// semantics (notably multiply's `>> frac`) and casts without a separate
    /// type context. Add/sub/compare reduce to integer ops on `stored`.
    Fixed { stored: i64, signed: bool, bits: u32, frac: u32 },
    Tuple(Vec<Value>),
    /// Record value, keyed by field name. Field order follows the declared
    /// schema in the producing record type.
    Record(BTreeMap<String, Value>),
    /// Fixed-length array.
    Array(Vec<Value>),
    /// Enum variant (variant name only — the enum type is recovered from
    /// context when needed).
    Enum(String),
    /// CSV-only marker for the active-mode column. Never used by the evaluator.
    ModeLabel(String),
}

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        if let Value::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        if let Value::Int(i) = self {
            Some(*i)
        } else {
            None
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        if let Value::Float(f) = self {
            Some(*f)
        } else {
            None
        }
    }
    pub fn to_csv(&self) -> String {
        match self {
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            // The stored integer is what the generated C prints for the backing
            // `intN`, so traces stay byte-identical across the two backends.
            Value::Fixed { stored, .. } => stored.to_string(),
            Value::Tuple(items) => items
                .iter()
                .map(|v| v.to_csv())
                .collect::<Vec<_>>()
                .join("|"),
            Value::Record(m) => {
                // `{k=v;...}` — `;` rather than `,` so the trace stays CSV-safe.
                let parts: Vec<String> =
                    m.iter().map(|(k, v)| format!("{k}={}", v.to_csv())).collect();
                format!("{{{}}}", parts.join(";"))
            }
            Value::Array(xs) => {
                let parts: Vec<String> = xs.iter().map(|v| v.to_csv()).collect();
                format!("[{}]", parts.join(";"))
            }
            Value::Enum(name) => name.clone(),
            Value::ModeLabel(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Trace {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// Per-cycle list of active mode names (one entry per cycle); empty when
    /// no contract is attached.
    pub active_modes: Vec<Vec<String>>,
    /// Per-cycle violations (label, cycle).
    pub violations: Vec<(String, usize)>,
}

impl Trace {
    pub fn to_csv(&self) -> String {
        let mut s = self.headers.join(",");
        s.push('\n');
        for row in &self.rows {
            s.push_str(
                &row.iter()
                    .map(|v| v.to_csv())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            s.push('\n');
        }
        s
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("no node named `{0}` in project")]
    UnknownNode(String),
    #[error("CSV input header mismatch: expected {expected:?}, got {got:?}")]
    HeaderMismatch { expected: Vec<String>, got: Vec<String> },
    #[error("could not parse CSV value `{value}` for column `{col}` of type {ty:?}")]
    ParseError { value: String, col: String, ty: Type },
    #[error("evaluation error: {0}")]
    EvalError(String),
}

#[derive(Debug, Default, Clone)]
pub struct State {
    cycle: usize,
    prev: HashMap<String, Value>,
    /// Completed active cycles per non-base clock chain (keyed by
    /// [`ol_ir::Clock::key`]). A clocked `->` takes its init branch while
    /// its chain's count is still zero — the clocked analogue of
    /// `cycle == 0`.
    clock_ticks: HashMap<String, usize>,
}

pub struct Sim<'a> {
    project: &'a Project,
    pub node: &'a NodeDef,
    state: State,
    contract: Option<ContractDef>,
    /// Per-call-site state, keyed by the address of the `Expr::Call` in the IR.
    /// Populated lazily on first invocation.
    call_states: HashMap<usize, State>,
    /// Project-wide constants, pre-evaluated once at construction time. Seeded
    /// into every step's env so equations can name them directly.
    consts: BTreeMap<String, Value>,
    /// Dependency order for the entry node's equations — declaration order is
    /// not sufficient (forward references would read stale defaults).
    eq_order: Vec<usize>,
    /// Clock inference for the entry node: which cycles each equation runs,
    /// which clock every `pre`/`->` site counts, and the chains to tick.
    clock_info: ol_ir::ClockInfo,
    /// Decision-coverage collector; populated by [`Sim::enable_coverage`].
    coverage: Option<Coverage>,
}

impl<'a> Sim<'a> {
    pub fn new(project: &'a Project, node_name: &str) -> Result<Self, SimError> {
        let node = project
            .find_node(node_name)
            .ok_or_else(|| SimError::UnknownNode(node_name.to_string()))?;
        let mut contract: Option<ContractDef> = None;
        if let Some(cname) = &node.contract {
            for pkg in &project.packages {
                let (cs, _) = parse_contracts(&pkg.contracts);
                if let Some(found) = cs.into_iter().find(|c| &c.name == cname) {
                    contract = Some(found);
                    break;
                }
            }
        }

        // Evaluate constants in declaration order. Later constants may
        // reference earlier ones because we extend the env as we go.
        let mut consts: BTreeMap<String, Value> = BTreeMap::new();
        for pkg in &project.packages {
            for c in &pkg.constants {
                let mut throwaway_state = State::default();
                let mut throwaway_calls: HashMap<usize, State> = HashMap::new();
                let v = eval(
                    &c.value,
                    &consts,
                    &mut throwaway_state,
                    &mut throwaway_calls,
                    project,
                    None,
                    &mut None,
                )
                .map_err(|e| SimError::EvalError(format!("constant `{}`: {e}", c.name)))?;
                consts.insert(c.name.clone(), v);
            }
        }

        let eq_order = ol_ir::evaluation_order(node).map_err(SimError::EvalError)?;
        let clock_info = if ol_ir::node_uses_clocks(node) {
            ol_ir::infer_clocks(node)
        } else {
            ol_ir::ClockInfo::default()
        };

        Ok(Sim {
            project,
            node,
            state: State::default(),
            contract,
            call_states: HashMap::new(),
            consts,
            eq_order,
            clock_info,
            coverage: None,
        })
    }

    pub fn step(
        &mut self,
        inputs: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, SimError> {
        let env = self.step_env(inputs)?;
        let mut outputs = BTreeMap::new();
        for p in &self.node.outputs {
            outputs.insert(
                p.name.clone(),
                env.get(&p.name).cloned().unwrap_or_else(|| default_value(&p.ty, self.project)),
            );
        }
        Ok(outputs)
    }

    /// Step one cycle and return EVERY named item — inputs, locals, and
    /// outputs — with its deterministic value for the cycle. This is the
    /// SCADE-style watch view: nothing is hidden, every signal has exactly
    /// one value per step.
    pub fn step_full(
        &mut self,
        inputs: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, SimError> {
        let env = self.step_env(inputs)?;
        let mut all = BTreeMap::new();
        for p in self.node.inputs.iter().chain(self.node.outputs.iter()) {
            all.insert(
                p.name.clone(),
                env.get(&p.name).cloned().unwrap_or_else(|| default_value(&p.ty, self.project)),
            );
        }
        for l in &self.node.locals {
            all.insert(
                l.name.clone(),
                env.get(&l.name).cloned().unwrap_or_else(|| default_value(&l.ty, self.project)),
            );
        }
        Ok(all)
    }

    fn step_env(
        &mut self,
        inputs: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, SimError> {
        // Constants are visible everywhere in a node's body; seed them first
        // so inputs/outputs/locals with the same name (which shouldn't exist
        // anyway — typecheck rejects collisions) would override them.
        let mut env: BTreeMap<String, Value> = self.consts.clone();
        for (k, v) in inputs {
            env.insert(k.clone(), v.clone());
        }
        // Outputs/locals seed from their previous value when one exists:
        // a clocked variable HOLDS its last value through inactive cycles
        // (the deterministic watch-view semantics). Base-clocked variables
        // are overwritten by their equations every cycle regardless.
        for p in &self.node.outputs {
            env.entry(p.name.clone()).or_insert_with(|| {
                self.state
                    .prev
                    .get(&p.name)
                    .cloned()
                    .unwrap_or_else(|| default_value(&p.ty, self.project))
            });
        }
        for l in &self.node.locals {
            env.entry(l.name.clone()).or_insert_with(|| {
                self.state
                    .prev
                    .get(&l.name)
                    .cloned()
                    .unwrap_or_else(|| default_value(&l.ty, self.project))
            });
        }

        // Dependency order, not declaration order: a forward reference like
        // `n = constant1 + 1; constant1 = 1;` must see this cycle's value.
        // A clocked equation runs only on its clock's active cycles — the
        // order guarantees clock variables compute before they gate anyone.
        for &i in &self.eq_order {
            let eq = &self.node.equations[i];
            if let Some(ck) = self.clock_info.equation_clocks.get(i) {
                if !clock_active(ck, &env)? {
                    continue;
                }
            }
            let value = eval_eq_rhs(
                &eq.rhs,
                &env,
                &mut self.state,
                &mut self.call_states,
                self.project,
                Some(&self.clock_info.site_clocks),
                &mut self.coverage,
            )
            .map_err(|e| match e {
                // Attribute the failure to its equation — the fuzzer's crash
                // findings (and the stepper's CRASH line) name the culprit.
                SimError::EvalError(m) => SimError::EvalError(format!(
                    "in equation `{}`: {m}",
                    eq.lhs.join(", ")
                )),
                other => other,
            })?;
            if eq.lhs.len() == 1 {
                env.insert(eq.lhs[0].clone(), value);
            } else if let Value::Tuple(items) = value {
                for (n, v) in eq.lhs.iter().zip(items.into_iter()) {
                    env.insert(n.clone(), v);
                }
            } else {
                return Err(SimError::EvalError(format!(
                    "multi-output equation produced a non-tuple value: {value:?}"
                )));
            }
        }

        // Count this cycle for every chain that was active, so clocked
        // `->` sites know their first tick has passed.
        for ck in &self.clock_info.chains {
            if clock_active(ck, &env)? {
                *self.state.clock_ticks.entry(ck.key()).or_insert(0) += 1;
            }
        }
        for (k, v) in &env {
            self.state.prev.insert(k.clone(), v.clone());
        }
        self.state.cycle += 1;
        Ok(env)
    }

    pub fn run_csv(&mut self, csv: &str) -> Result<Trace, SimError> {
        self.run_csv_impl(csv, false)
    }

    /// Like [`run_csv`] but every column is present: cycle, inputs, locals,
    /// outputs, then active_mode/violations when the node has a contract.
    pub fn run_csv_full(&mut self, csv: &str) -> Result<Trace, SimError> {
        self.run_csv_impl(csv, true)
    }

    fn run_csv_impl(&mut self, csv: &str, full: bool) -> Result<Trace, SimError> {
        let mut lines = csv.lines();
        let header_line = lines.next().unwrap_or("");
        let headers: Vec<String> = header_line.split(',').map(|s| s.trim().to_string()).collect();
        let expected: Vec<String> = self.node.inputs.iter().map(|p| p.name.clone()).collect();
        if headers != expected {
            return Err(SimError::HeaderMismatch {
                expected,
                got: headers,
            });
        }

        let mut trace = Trace::default();
        trace.headers = vec!["cycle".into()];
        if full {
            trace
                .headers
                .extend(self.node.inputs.iter().map(|p| p.name.clone()));
            trace
                .headers
                .extend(self.node.locals.iter().map(|l| l.name.clone()));
        }
        trace.headers
            .extend(self.node.outputs.iter().map(|p| p.name.clone()));
        if self.contract.is_some() {
            trace.headers.push("active_mode".into());
            trace.headers.push("violations".into());
        }

        for (cycle, row) in lines.enumerate() {
            let fields: Vec<&str> = row.split(',').collect();
            if fields.iter().all(|f| f.trim().is_empty()) {
                continue;
            }
            let mut inputs = BTreeMap::new();
            for (i, p) in self.node.inputs.iter().enumerate() {
                let raw = fields.get(i).copied().unwrap_or("").trim();
                let v = parse_value(raw, &p.ty, self.project).map_err(|_| SimError::ParseError {
                    value: raw.into(),
                    col: p.name.clone(),
                    ty: p.ty.clone(),
                })?;
                inputs.insert(p.name.clone(), v);
            }
            let env = self.step_env(&inputs)?;
            let mut out = BTreeMap::new();
            for p in &self.node.outputs {
                out.insert(
                    p.name.clone(),
                    env.get(&p.name)
                        .cloned()
                        .unwrap_or_else(|| default_value(&p.ty, self.project)),
                );
            }
            let mut out_row: Vec<Value> = vec![Value::Int(cycle as i64)];
            if full {
                for p in &self.node.inputs {
                    out_row.push(env.get(&p.name).cloned().unwrap_or(Value::Bool(false)));
                }
                for l in &self.node.locals {
                    out_row.push(env.get(&l.name).cloned().unwrap_or(Value::Bool(false)));
                }
            }
            for p in &self.node.outputs {
                out_row.push(out.get(&p.name).cloned().unwrap_or(Value::Bool(false)));
            }
            if let Some(c) = &self.contract {
                let step = evaluate_monitor(c, &inputs, &out);
                let mode_label = if step.active_modes.is_empty() {
                    "<none>".to_string()
                } else {
                    step.active_modes.join("|")
                };
                let viol_label = if step.violations.is_empty() {
                    "<none>".to_string()
                } else {
                    step.violations.join("|")
                };
                for v in &step.violations {
                    trace.violations.push((v.clone(), cycle));
                }
                trace.active_modes.push(step.active_modes);
                out_row.push(Value::ModeLabel(mode_label.replace(',', ";")));
                out_row.push(Value::ModeLabel(viol_label.replace(',', ";")));
            }
            trace.rows.push(out_row);
        }

        Ok(trace)
    }
}

fn default_value(ty: &Type, project: &Project) -> Value {
    match ty {
        // Generic parameters never reach the simulator: monomorphization
        // replaced them, or the typechecker refused the project.
        Type::Var { .. } | Type::ArrayVar { .. } => Value::Int(0),
        Type::Bool => Value::Bool(false),
        Type::Float32 | Type::Float64 => Value::Float(0.0),
        Type::Int8
        | Type::Int16
        | Type::Int32
        | Type::Int64
        | Type::Uint8
        | Type::Uint16
        | Type::Uint32
        | Type::Uint64 => Value::Int(0),
        // A char carries as an integer byte; its zero value is the NUL byte.
        Type::Char => Value::Int(0),
        // Fixed-point zero: real 0.0 stores as integer 0.
        Type::Fixed { signed, bits, frac } => Value::Fixed {
            stored: 0,
            signed: *signed,
            bits: *bits,
            frac: *frac,
        },
        Type::Array { elem, len } => {
            Value::Array((0..*len).map(|_| default_value(elem, project)).collect())
        }
        Type::Named { name } => default_named(name, project).unwrap_or(Value::Int(0)),
    }
}

fn default_named(name: &str, project: &Project) -> Option<Value> {
    for pkg in &project.packages {
        for t in &pkg.types {
            if t.name() == name {
                return Some(match &t.body {
                    TypeBody::Enum(e) => e
                        .variants
                        .first()
                        .map(|v| Value::Enum(v.clone()))
                        .unwrap_or(Value::Enum(String::new())),
                    TypeBody::Record { fields, .. } => {
                        let mut m = BTreeMap::new();
                        for f in fields {
                            m.insert(f.name.clone(), default_value(&f.ty, project));
                        }
                        Value::Record(m)
                    }
                    TypeBody::Alias { target, .. } => default_value(target, project),
                });
            }
        }
    }
    None
}

/// Look up `name` against any declared enum in the project; if it matches a
/// variant, return that variant as an [`Value::Enum`].
fn enum_variant_value(name: &str, project: &Project) -> Option<Value> {
    for pkg in &project.packages {
        for t in &pkg.types {
            if let TypeBody::Enum(e) = &t.body {
                if e.variants.iter().any(|v| v == name) {
                    return Some(Value::Enum(name.to_string()));
                }
            }
        }
    }
    None
}

fn parse_value(raw: &str, ty: &Type, project: &Project) -> Result<Value, ()> {
    match ty {
        Type::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "t" => Ok(Value::Bool(true)),
            "false" | "0" | "f" => Ok(Value::Bool(false)),
            _ => Err(()),
        },
        t if t.is_float() => raw.parse::<f64>().map(Value::Float).map_err(|_| ()),
        t if t.is_integer() => raw.parse::<i64>().map(Value::Int).map_err(|_| ()),
        // An enum input reads its variant name — the same text `Value::to_csv`
        // writes, so enum-interface traces round-trip. Unknown variants are a
        // parse error, never a silent default.
        Type::Named { name } => {
            let variants = project
                .packages
                .iter()
                .flat_map(|p| &p.types)
                .find_map(|t| match &t.body {
                    ol_ir::TypeBody::Enum(e) if &e.name == name => Some(&e.variants),
                    _ => None,
                })
                .ok_or(())?;
            if variants.iter().any(|v| v == raw) {
                Ok(Value::Enum(raw.to_string()))
            } else {
                Err(())
            }
        }
        // Arrays at the CSV boundary use `[e0;e1;…]` — the same bracketed,
        // semicolon-separated form `Value::to_csv` produces, so traces
        // round-trip and the generated C driver can match byte-for-byte.
        Type::Array { elem, len } => {
            let inner = raw
                .trim()
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .ok_or(())?;
            let parts: Vec<&str> = if inner.trim().is_empty() {
                Vec::new()
            } else {
                inner.split(';').collect()
            };
            if parts.len() != *len as usize {
                return Err(());
            }
            let mut vals = Vec::with_capacity(parts.len());
            for p in parts {
                vals.push(parse_value(p.trim(), elem, project)?);
            }
            Ok(Value::Array(vals))
        }
        // Fixed-point crosses the CSV boundary as its stored integer (the same
        // form `to_csv` emits and the generated C `intN` reads), so node I/O of
        // a fixed type round-trips and matches the compiled driver.
        Type::Fixed { signed, bits, frac } => raw
            .trim()
            .parse::<i64>()
            .map(|i| Value::Fixed {
                stored: narrow_fixed(*signed, *bits, i),
                signed: *signed,
                bits: *bits,
                frac: *frac,
            })
            .map_err(|_| ()),
        _ => Err(()),
    }
}

/// Per-cycle monitor result: which modes were active, and which contract
/// clauses failed.
struct MonitorStep {
    active_modes: Vec<String>,
    violations: Vec<String>,
}

fn evaluate_monitor(
    c: &ContractDef,
    inputs: &BTreeMap<String, Value>,
    outputs: &BTreeMap<String, Value>,
) -> MonitorStep {
    let mut env: BTreeMap<String, Value> = BTreeMap::new();
    env.extend(inputs.clone());
    env.extend(outputs.clone());
    let mut state = State::default();
    let mut call_states: HashMap<usize, State> = HashMap::new();
    let project = Project::default();

    let mut active = Vec::new();
    let mut violations = Vec::new();

    // Guarantees are always required to hold.
    for (i, g) in c.guarantees.iter().enumerate() {
        let label = g.name.clone().unwrap_or_else(|| format!("guarantee#{i}"));
        match eval(&g.expr, &env, &mut state, &mut call_states, &project, None, &mut None) {
            Ok(Value::Bool(true)) => {}
            _ => violations.push(label),
        }
    }

    // A mode is active when all of its `require` clauses hold; when active,
    // its `ensure` clauses must hold too.
    for m in &c.modes {
        let mut hit = true;
        for r in &m.requires {
            match eval(r, &env, &mut state, &mut call_states, &project, None, &mut None) {
                Ok(Value::Bool(true)) => {}
                _ => {
                    hit = false;
                    break;
                }
            }
        }
        if hit {
            active.push(m.name.clone());
            for (j, e) in m.ensures.iter().enumerate() {
                let label = format!("{}::ensure#{j}", m.name);
                match eval(e, &env, &mut state, &mut call_states, &project, None, &mut None) {
                    Ok(Value::Bool(true)) => {}
                    _ => violations.push(label),
                }
            }
        }
    }

    MonitorStep {
        active_modes: active,
        violations,
    }
}

/// C cast semantics for in-range values: integer narrowing wraps two's
/// complement, float→int truncates toward zero, and float32 targets round
/// through `f32` so the trace matches what the generated C computes.
fn cast_value(to: &Type, v: Value) -> Result<Value, SimError> {
    let out = match (to, v) {
        (Type::Float32, Value::Int(i)) => Value::Float((i as f32) as f64),
        (Type::Float32, Value::Float(f)) => Value::Float((f as f32) as f64),
        (Type::Float64, Value::Int(i)) => Value::Float(i as f64),
        (Type::Float64, Value::Float(f)) => Value::Float(f),
        (t, Value::Int(i)) if t.is_integer() => Value::Int(narrow_int(t, i)),
        (t, Value::Float(f)) if t.is_integer() => Value::Int(narrow_int(t, f as i64)),
        // --- Fixed-point casts (Q-format: stored == round(real·2^frac)) -------
        // Into fixed: rescale the source into the backing integer, then wrap to
        // the storage width (two's complement, matching the C `(intN)` cast).
        (Type::Fixed { signed, bits, frac }, Value::Int(i)) => Value::Fixed {
            stored: narrow_fixed(*signed, *bits, i.wrapping_shl(*frac)),
            signed: *signed,
            bits: *bits,
            frac: *frac,
        },
        (Type::Fixed { signed, bits, frac }, Value::Float(f)) => Value::Fixed {
            stored: narrow_fixed(*signed, *bits, (f * 2f64.powi(*frac as i32)).round() as i64),
            signed: *signed,
            bits: *bits,
            frac: *frac,
        },
        (Type::Fixed { signed, bits, frac }, Value::Fixed { stored, frac: from, .. }) => {
            let rescaled = if *frac >= from {
                stored.wrapping_shl(*frac - from)
            } else {
                stored >> (from - *frac)
            };
            Value::Fixed {
                stored: narrow_fixed(*signed, *bits, rescaled),
                signed: *signed,
                bits: *bits,
                frac: *frac,
            }
        }
        // Out of fixed: exact divide to float, truncate-toward-zero to int (the
        // `int64` division both backends share — see the C-Lite emitter).
        (Type::Float32, Value::Fixed { stored, frac, .. }) => {
            Value::Float(((stored as f64 / 2f64.powi(frac as i32)) as f32) as f64)
        }
        (Type::Float64, Value::Fixed { stored, frac, .. }) => {
            Value::Float(stored as f64 / 2f64.powi(frac as i32))
        }
        (t, Value::Fixed { stored, frac, .. }) if t.is_integer() => {
            Value::Int(narrow_int(t, (stored as i128 / (1i128 << frac)) as i64))
        }
        (t, v) => {
            return Err(SimError::EvalError(format!(
                "cannot cast {v:?} to {t:?}"
            )))
        }
    };
    Ok(out)
}

fn narrow_int(t: &Type, i: i64) -> i64 {
    match t {
        Type::Int8 => i as i8 as i64,
        Type::Int16 => i as i16 as i64,
        Type::Int32 => i as i32 as i64,
        Type::Uint8 => i as u8 as i64,
        Type::Uint16 => i as u16 as i64,
        Type::Uint32 => i as u32 as i64,
        // 64-bit targets keep the i64 carrier's bit pattern unchanged.
        _ => i,
    }
}

/// Wrap a fixed-point stored value to its backing integer width (two's
/// complement), mirroring `narrow_int` for the `(signed, bits)` storage type.
fn narrow_fixed(signed: bool, bits: u32, v: i64) -> i64 {
    match (signed, bits) {
        (true, 8) => v as i8 as i64,
        (true, 16) => v as i16 as i64,
        (true, 32) => v as i32 as i64,
        (false, 8) => v as u8 as i64,
        (false, 16) => v as u16 as i64,
        (false, 32) => v as u32 as i64,
        // 64-bit (and any unsupported width) keeps the i64 carrier unchanged.
        _ => v,
    }
}

/// Clamp a fixed-point stored value to its type's saturation range. Uses the
/// shared `Type::fixed_sat_range` so the bound matches the C-Lite emitter
/// exactly (keeping saturating arithmetic bit-identical across backends).
fn clamp_fixed(signed: bool, bits: u32, v: i64) -> i64 {
    let (lo, hi) = Type::Fixed { signed, bits, frac: 0 }
        .fixed_sat_range()
        .unwrap_or((i64::MIN, i64::MAX));
    v.clamp(lo, hi)
}

/// True on the cycles where every condition along `ck`'s chain holds.
fn clock_active(ck: &ol_ir::Clock, env: &BTreeMap<String, Value>) -> Result<bool, SimError> {
    for (var, on) in ck.conditions() {
        match env.get(&var) {
            Some(Value::Bool(b)) => {
                if *b != on {
                    return Ok(false);
                }
            }
            Some(other) => {
                return Err(SimError::EvalError(format!(
                    "clock `{var}` must be bool, got {other:?}"
                )))
            }
            None => {
                return Err(SimError::EvalError(format!(
                    "clock variable `{var}` has no value this cycle"
                )))
            }
        }
    }
    Ok(true)
}

/// Whether a `pre`/`->` site is on its first tick: cycle 0 for base-clocked
/// sites, "chain never active before" for clocked ones.
fn first_tick(
    expr: &Expr,
    state: &State,
    site_clocks: Option<&HashMap<usize, ol_ir::Clock>>,
) -> bool {
    match site_clocks.and_then(|m| m.get(&(expr as *const Expr as usize))) {
        None | Some(ol_ir::Clock::Base) => state.cycle == 0,
        Some(ck) => state.clock_ticks.get(&ck.key()).copied().unwrap_or(0) == 0,
    }
}

/// Evaluate a boolean decision, recording each atomic condition's value into
/// `obs` as it is reached. Descends through the boolean connectives and
/// evaluates each atomic leaf exactly once (eager, like `eval_binary`), so a
/// stateful sub-call is never stepped twice and the outcome matches a normal
/// `eval` of the same expression.
#[allow(clippy::too_many_arguments)]
fn eval_decision(
    expr: &Expr,
    env: &BTreeMap<String, Value>,
    state: &mut State,
    call_states: &mut HashMap<usize, State>,
    project: &Project,
    site_clocks: Option<&HashMap<usize, ol_ir::Clock>>,
    cov: &mut Option<Coverage>,
    obs: &mut BTreeMap<usize, bool>,
) -> Result<bool, SimError> {
    macro_rules! sub {
        ($e:expr) => {
            eval_decision($e, env, state, call_states, project, site_clocks, cov, obs)?
        };
    }
    // Operands are bound to locals BEFORE combining so both are always
    // evaluated — the IR has no short-circuit (`eval_binary` is eager), and
    // MC/DC needs every condition's value on every evaluation.
    match expr {
        Expr::Binary { op: BinOp::And, lhs, rhs } => {
            let (l, r) = (sub!(lhs), sub!(rhs));
            Ok(l && r)
        }
        Expr::Binary { op: BinOp::Or, lhs, rhs } => {
            let (l, r) = (sub!(lhs), sub!(rhs));
            Ok(l || r)
        }
        Expr::Binary { op: BinOp::Xor, lhs, rhs } => {
            let (l, r) = (sub!(lhs), sub!(rhs));
            Ok(l ^ r)
        }
        Expr::Binary { op: BinOp::Implies, lhs, rhs } => {
            let (l, r) = (sub!(lhs), sub!(rhs));
            Ok(!l || r)
        }
        Expr::Unary { op: UnaryOp::Not, arg } => Ok(!sub!(arg)),
        atomic => {
            let v = eval(atomic, env, state, call_states, project, site_clocks, cov)?;
            let b = v.as_bool().ok_or_else(|| {
                SimError::EvalError(format!("decision condition is not bool: {v:?}"))
            })?;
            obs.insert(atomic as *const Expr as usize, b);
            Ok(b)
        }
    }
}

/// Evaluate an equation's right-hand side, recording an MC/DC trial first if
/// the RHS is a registered decision. Either way returns the RHS value.
fn eval_eq_rhs(
    rhs: &Expr,
    env: &BTreeMap<String, Value>,
    state: &mut State,
    call_states: &mut HashMap<usize, State>,
    project: &Project,
    site_clocks: Option<&HashMap<usize, ol_ir::Clock>>,
    cov: &mut Option<Coverage>,
) -> Result<Value, SimError> {
    let ptr = rhs as *const Expr as usize;
    let is_root = cov.as_ref().map_or(false, |c| c.is_root(ptr));
    if is_root {
        let mut obs = BTreeMap::new();
        let outcome = eval_decision(rhs, env, state, call_states, project, site_clocks, cov, &mut obs)?;
        if let Some(c) = cov.as_mut() {
            c.record_trial(ptr, &obs, outcome);
        }
        Ok(Value::Bool(outcome))
    } else {
        eval(rhs, env, state, call_states, project, site_clocks, cov)
    }
}

fn eval(
    expr: &Expr,
    env: &BTreeMap<String, Value>,
    state: &mut State,
    call_states: &mut HashMap<usize, State>,
    project: &Project,
    site_clocks: Option<&HashMap<usize, ol_ir::Clock>>,
    cov: &mut Option<Coverage>,
) -> Result<Value, SimError> {
    match expr {
        Expr::Last { name } => Err(SimError::EvalError(format!(
            "`last {name}` survives only in an unlowered state machine — lower machines first"
        ))),
        Expr::Const { lit } => Ok(match lit {
            Literal::Bool { value } => Value::Bool(*value),
            Literal::Int { value } => Value::Int(*value),
            Literal::Float { value } => Value::Float(*value),
            // A char is a byte; the evaluator carries it as an integer (Lustre
            // views `char` as a small int).
            Literal::Char { value } => Value::Int(*value as i64),
        }),
        Expr::Var { name } => match env.get(name).cloned() {
            Some(v) => Ok(v),
            None => enum_variant_value(name, project)
                .ok_or_else(|| SimError::EvalError(format!("unbound variable `{name}`"))),
        },
        Expr::Unary { op, arg } => {
            let v = eval(arg, env, state, call_states, project, site_clocks, cov)?;
            Ok(match (op, v) {
                (UnaryOp::Not, Value::Bool(b)) => Value::Bool(!b),
                (UnaryOp::Neg, Value::Int(i)) => Value::Int(-i),
                (UnaryOp::Neg, Value::Float(f)) => Value::Float(-f),
                (op, v) => {
                    return Err(SimError::EvalError(format!(
                        "unary {op:?} not supported on {v:?}"
                    )))
                }
            })
        }
        Expr::Binary { op, lhs, rhs } => {
            let l = eval(lhs, env, state, call_states, project, site_clocks, cov)?;
            let r = eval(rhs, env, state, call_states, project, site_clocks, cov)?;
            eval_binary(*op, l, r)
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            // With coverage on, evaluate the condition through the decision
            // recorder so each atomic condition's value feeds MC/DC; this also
            // serves legacy decision coverage (seen-true/seen-false).
            let cval = if cov.is_some() {
                let mut obs = BTreeMap::new();
                let outcome =
                    eval_decision(cond, env, state, call_states, project, site_clocks, cov, &mut obs)?;
                let ptr = cond.as_ref() as *const Expr as usize;
                if let Some(coverage) = cov.as_mut() {
                    coverage.mark(ptr, outcome);
                    coverage.record_trial(ptr, &obs, outcome);
                }
                outcome
            } else {
                match eval(cond, env, state, call_states, project, site_clocks, cov)? {
                    Value::Bool(b) => b,
                    other => {
                        return Err(SimError::EvalError(format!(
                            "if-condition is not bool: {other:?}"
                        )))
                    }
                }
            };
            if cval {
                eval(then_branch, env, state, call_states, project, site_clocks, cov)
            } else {
                eval(else_branch, env, state, call_states, project, site_clocks, cov)
            }
        }
        Expr::Pre { arg } => {
            if first_tick(expr, state, site_clocks) {
                Err(SimError::EvalError(
                    "uninitialized `pre` evaluated on its first tick (missing `->`)".into(),
                ))
            } else if let Expr::Var { name } = arg.as_ref() {
                // A clocked variable holds its value through inactive cycles,
                // so the previous-cycle snapshot IS its value at the last tick.
                state.prev.get(name).cloned().ok_or_else(|| {
                    SimError::EvalError(format!("no previous value for `{name}`"))
                })
            } else {
                Err(SimError::EvalError(
                    "complex `pre` operands are not supported in the Phase 0 profile".into(),
                ))
            }
        }
        Expr::Arrow { init, body } => {
            if first_tick(expr, state, site_clocks) {
                eval(init, env, state, call_states, project, site_clocks, cov)
            } else {
                eval(body, env, state, call_states, project, site_clocks, cov)
            }
        }
        // The clock checker guarantees a `when` is only reached on cycles
        // where its condition already holds (its equation or merge branch is
        // gated), so sampling is just evaluation here.
        Expr::When { arg, .. } => eval(arg, env, state, call_states, project, site_clocks, cov),
        // Only the active branch evaluates: state under the inactive branch
        // (clocked arrows, stateful calls) must not advance on its off cycles.
        Expr::Merge { clock, on_true, on_false } => match env.get(clock) {
            Some(Value::Bool(true)) => {
                eval(on_true, env, state, call_states, project, site_clocks, cov)
            }
            Some(Value::Bool(false)) => {
                eval(on_false, env, state, call_states, project, site_clocks, cov)
            }
            Some(other) => Err(SimError::EvalError(format!(
                "merge clock `{clock}` must be bool, got {other:?}"
            ))),
            None => Err(SimError::EvalError(format!(
                "merge clock `{clock}` has no value this cycle"
            ))),
        },
        Expr::Cast { to, arg } => {
            let v = eval(arg, env, state, call_states, project, site_clocks, cov)?;
            cast_value(to, v)
        }
        // Float intrinsics compute in f64 (double variants) or f32 (single
        // variants) — the same libm functions the generated C calls
        // (`sqrt` vs `sqrtf`), so both backends agree.
        Expr::FloatIntrinsic { op, args, single } => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                match eval(a, env, state, call_states, project, site_clocks, cov)? {
                    Value::Float(f) => vals.push(f),
                    other => {
                        return Err(SimError::EvalError(format!(
                            "`{}` requires float operands, got {other:?}",
                            op.name()
                        )))
                    }
                }
            }
            if vals.len() != op.arity() {
                return Err(SimError::EvalError(format!(
                    "`{}` takes {} arguments, got {}",
                    op.name(),
                    op.arity(),
                    vals.len()
                )));
            }
            use ol_ir::FloatOp;
            if *single {
                // float32 values are stored f32-rounded; compute in f32 like
                // the generated C's float functions do.
                let x = vals[0] as f32;
                let y = || vals[1] as f32;
                let r = match op {
                    FloatOp::Sqrt => x.sqrt(),
                    FloatOp::Sin => x.sin(),
                    FloatOp::Cos => x.cos(),
                    FloatOp::Tan => x.tan(),
                    FloatOp::Asin => x.asin(),
                    FloatOp::Acos => x.acos(),
                    FloatOp::Atan => x.atan(),
                    FloatOp::Atan2 => x.atan2(y()),
                    FloatOp::Exp => x.exp(),
                    FloatOp::Log => x.ln(),
                    FloatOp::Log10 => x.log10(),
                    FloatOp::Pow => x.powf(y()),
                    FloatOp::Floor => x.floor(),
                    FloatOp::Ceil => x.ceil(),
                    FloatOp::Round => x.round(),
                    FloatOp::Abs => x.abs(),
                    FloatOp::Min => x.min(y()),
                    FloatOp::Max => x.max(y()),
                };
                return Ok(Value::Float(r as f64));
            }
            let x = vals[0];
            let r = match op {
                FloatOp::Sqrt => x.sqrt(),
                FloatOp::Sin => x.sin(),
                FloatOp::Cos => x.cos(),
                FloatOp::Tan => x.tan(),
                FloatOp::Asin => x.asin(),
                FloatOp::Acos => x.acos(),
                FloatOp::Atan => x.atan(),
                FloatOp::Atan2 => x.atan2(vals[1]),
                FloatOp::Exp => x.exp(),
                FloatOp::Log => x.ln(),
                FloatOp::Log10 => x.log10(),
                FloatOp::Pow => x.powf(vals[1]),
                FloatOp::Floor => x.floor(),
                FloatOp::Ceil => x.ceil(),
                // f64::round rounds half away from zero, exactly like C round().
                FloatOp::Round => x.round(),
                FloatOp::Abs => x.abs(),
                // f64::min/max return the non-NaN operand, like C fmin/fmax.
                FloatOp::Min => x.min(vals[1]),
                FloatOp::Max => x.max(vals[1]),
            };
            Ok(Value::Float(r))
        }
        // The printout block: values to stderr (never the CSV trace on
        // stdout), `true` on the terminal_out wire.
        Expr::Printout { args } => {
            let mut parts: Vec<String> = Vec::with_capacity(args.len());
            for a in args {
                let label = match a {
                    Expr::Var { name } => name.clone(),
                    _ => "?".into(),
                };
                let v = eval(a, env, state, call_states, project, site_clocks, cov)?;
                parts.push(format!("{label}={}", v.to_csv()));
            }
            eprintln!("terminal_out | {}", parts.join(" "));
            Ok(Value::Bool(true))
        }
        Expr::ArrayOp { op, args } => {
            let mut arrs: Vec<Vec<Value>> = Vec::with_capacity(args.len());
            for a in args {
                match eval(a, env, state, call_states, project, site_clocks, cov)? {
                    Value::Array(xs) => arrs.push(xs),
                    other => {
                        return Err(SimError::EvalError(format!(
                            "`{}` operand is not an array: {other:?}",
                            op.name()
                        )))
                    }
                }
            }
            match op {
                ol_ir::ArrayOpKind::Concat => {
                    let mut out = arrs.remove(0);
                    out.extend(arrs.remove(0));
                    Ok(Value::Array(out))
                }
                ol_ir::ArrayOpKind::Reverse => {
                    let mut out = arrs.remove(0);
                    out.reverse();
                    Ok(Value::Array(out))
                }
            }
        }
        // Only the matching arm evaluates — like if/then/else and merge,
        // inactive branches must not run.
        Expr::Case { sel, arms, default } => {
            let sv = eval(sel, env, state, call_states, project, site_clocks, cov)?;
            let variant = match &sv {
                Value::Enum(v) => v.clone(),
                other => {
                    return Err(SimError::EvalError(format!(
                        "`case` selector is not an enum value: {other:?}"
                    )))
                }
            };
            if let Some(arm) = arms.iter().find(|a| a.variant == variant) {
                eval(&arm.value, env, state, call_states, project, site_clocks, cov)
            } else if let Some(d) = default {
                eval(d, env, state, call_states, project, site_clocks, cov)
            } else {
                Err(SimError::EvalError(format!(
                    "`case` has no arm for variant `{variant}` and no `_:` default"
                )))
            }
        }
        Expr::Call { node, args } => eval_call(expr, node, args, env, state, call_states, project, site_clocks, cov),
        Expr::Field { base, field } => {
            let bv = eval(base, env, state, call_states, project, site_clocks, cov)?;
            match bv {
                Value::Record(m) => m.get(field).cloned().ok_or_else(|| {
                    SimError::EvalError(format!("record has no field `{field}`"))
                }),
                other => Err(SimError::EvalError(format!(
                    "field access `.{field}` on non-record value: {other:?}"
                ))),
            }
        }
        Expr::Index { base, index } => {
            let bv = eval(base, env, state, call_states, project, site_clocks, cov)?;
            let iv = eval(index, env, state, call_states, project, site_clocks, cov)?;
            let i = iv.as_int().ok_or_else(|| {
                SimError::EvalError(format!("array index must be int, got {iv:?}"))
            })?;
            match bv {
                Value::Array(xs) => {
                    if i < 0 || (i as usize) >= xs.len() {
                        Err(SimError::EvalError(format!(
                            "array index {i} out of bounds (len {})",
                            xs.len()
                        )))
                    } else {
                        Ok(xs[i as usize].clone())
                    }
                }
                other => Err(SimError::EvalError(format!(
                    "indexing non-array value: {other:?}"
                ))),
            }
        }
        Expr::Tuple { items } => {
            let mut vs = Vec::with_capacity(items.len());
            for it in items {
                vs.push(eval(it, env, state, call_states, project, site_clocks, cov)?);
            }
            Ok(Value::Tuple(vs))
        }
        Expr::Array { items } => {
            let mut vs = Vec::with_capacity(items.len());
            for it in items {
                vs.push(eval(it, env, state, call_states, project, site_clocks, cov)?);
            }
            Ok(Value::Array(vs))
        }
        Expr::Struct { fields, .. } => {
            let mut m = BTreeMap::new();
            for fi in fields {
                m.insert(
                    fi.field.clone(),
                    eval(&fi.value, env, state, call_states, project, site_clocks, cov)?,
                );
            }
            Ok(Value::Record(m))
        }
        Expr::Iterate { kind, node: f_name, init, arrays } => {
            let callee = project.find_node(f_name).ok_or_else(|| {
                SimError::EvalError(format!("iterator calls unknown function `{f_name}`"))
            })?;
            if !matches!(callee.kind, NodeKind::Function) {
                return Err(SimError::EvalError(format!(
                    "iterator function `{f_name}` must be a stateless function"
                )));
            }
            // Evaluate the array operands to concrete vectors.
            let mut arrs: Vec<Vec<Value>> = Vec::with_capacity(arrays.len());
            for a in arrays {
                match eval(a, env, state, call_states, project, site_clocks, cov)? {
                    Value::Array(xs) => arrs.push(xs),
                    other => {
                        return Err(SimError::EvalError(format!(
                            "iterator operand is not an array: {other:?}"
                        )))
                    }
                }
            }
            let n = arrs.first().map(|a| a.len()).unwrap_or(0);
            if arrs.iter().any(|a| a.len() != n) {
                return Err(SimError::EvalError(
                    "iterator arrays have unequal lengths".into(),
                ));
            }
            match kind {
                IterKind::Map | IterKind::Mapi => {
                    // Apply F to the k-th element of each array, building the
                    // result array element by element; `mapi` passes the
                    // element index as F's first argument.
                    let mut out = Vec::with_capacity(n);
                    for k in 0..n {
                        let mut args: Vec<Value> = Vec::with_capacity(arrs.len() + 1);
                        if matches!(kind, IterKind::Mapi) {
                            args.push(Value::Int(k as i64));
                        }
                        args.extend(arrs.iter().map(|a| a[k].clone()));
                        out.push(call_function_values(callee, args, project, cov)?);
                    }
                    Ok(Value::Array(out))
                }
                IterKind::Fold | IterKind::Foldi => {
                    // Left fold: acc starts at the seed, then F(acc, elem) —
                    // `foldi` prepends the element index.
                    let seed = init.as_ref().ok_or_else(|| {
                        SimError::EvalError("fold without an accumulator seed".into())
                    })?;
                    let mut acc =
                        eval(seed, env, state, call_states, project, site_clocks, cov)?;
                    for (k, elem) in arrs[0].iter().enumerate() {
                        let mut args: Vec<Value> = Vec::with_capacity(3);
                        if matches!(kind, IterKind::Foldi) {
                            args.push(Value::Int(k as i64));
                        }
                        args.push(acc);
                        args.push(elem.clone());
                        acc = call_function_values(callee, args, project, cov)?;
                    }
                    Ok(acc)
                }
                IterKind::MapFold => {
                    // Combined: thread the accumulator like fold while
                    // collecting F's second output like map; the value is the
                    // tuple (final accumulator, mapped array) the two-name
                    // lhs destructures.
                    let seed = init.as_ref().ok_or_else(|| {
                        SimError::EvalError("mapfold without an accumulator seed".into())
                    })?;
                    let mut acc =
                        eval(seed, env, state, call_states, project, site_clocks, cov)?;
                    let mut out = Vec::with_capacity(n);
                    for elem in &arrs[0] {
                        match call_function_values(
                            callee,
                            vec![acc, elem.clone()],
                            project,
                            cov,
                        )? {
                            Value::Tuple(mut items) if items.len() == 2 => {
                                out.push(items.pop().expect("len 2"));
                                acc = items.pop().expect("len 1");
                            }
                            other => {
                                return Err(SimError::EvalError(format!(
                                    "mapfold's `{f_name}` must produce \
                                     (accumulator, element), got {other:?}"
                                )))
                            }
                        }
                    }
                    Ok(Value::Tuple(vec![acc, Value::Array(out)]))
                }
            }
        }
    }
}

/// Invoke a stateless function with already-computed argument values — the
/// per-element call an iterator makes. Mirrors the `Function` branch of
/// [`eval_call`] but takes `Value`s instead of argument expressions.
fn call_function_values(
    callee: &NodeDef,
    arg_values: Vec<Value>,
    project: &Project,
    cov: &mut Option<Coverage>,
) -> Result<Value, SimError> {
    if arg_values.len() != callee.inputs.len() {
        return Err(SimError::EvalError(format!(
            "iterated `{}` arity mismatch: expected {}, got {}",
            callee.name,
            callee.inputs.len(),
            arg_values.len()
        )));
    }
    let mut callee_env: BTreeMap<String, Value> = BTreeMap::new();
    for pkg in &project.packages {
        for c in &pkg.constants {
            let mut ts = State::default();
            let mut tc: HashMap<usize, State> = HashMap::new();
            if let Ok(v) = eval(&c.value, &callee_env, &mut ts, &mut tc, project, None, &mut None) {
                callee_env.insert(c.name.clone(), v);
            }
        }
    }
    for (p, v) in callee.inputs.iter().zip(arg_values.into_iter()) {
        callee_env.insert(p.name.clone(), v);
    }
    for p in &callee.outputs {
        callee_env.insert(p.name.clone(), default_value(&p.ty, project));
    }
    for l in &callee.locals {
        callee_env.insert(l.name.clone(), default_value(&l.ty, project));
    }
    let order = ol_ir::evaluation_order(callee)
        .map_err(|e| SimError::EvalError(format!("`{}`: {e}", callee.name)))?;
    // Stateless: a throwaway state, and a fresh call map for any nested
    // function calls (functions never touch stateful call state).
    let mut throwaway = State::default();
    let mut sub_calls: HashMap<usize, State> = HashMap::new();
    for &i in &order {
        let eq = &callee.equations[i];
        let v = eval_eq_rhs(&eq.rhs, &callee_env, &mut throwaway, &mut sub_calls, project, None, cov)?;
        bind_lhs(&mut callee_env, eq, v)?;
    }
    extract_output(callee, &mut callee_env)
}

fn eval_call(
    call_expr: &Expr,
    node: &str,
    args: &[Expr],
    env: &BTreeMap<String, Value>,
    state: &mut State,
    call_states: &mut HashMap<usize, State>,
    project: &Project,
    site_clocks: Option<&HashMap<usize, ol_ir::Clock>>,
    cov: &mut Option<Coverage>,
) -> Result<Value, SimError> {
    let callee = project
        .find_node(node)
        .ok_or_else(|| SimError::EvalError(format!("unknown callee `{node}`")))?;
    if args.len() != callee.inputs.len() {
        return Err(SimError::EvalError(format!(
            "call to `{}` arity mismatch: expected {}, got {}",
            node,
            callee.inputs.len(),
            args.len()
        )));
    }
    if matches!(callee.kind, NodeKind::Imported) {
        return Err(SimError::EvalError(format!(
            "imported operator `{node}` cannot be simulated; provide a model or stub"
        )));
    }

    // Evaluate arguments in the OUTER scope (caller's state).
    let mut arg_values: Vec<Value> = Vec::with_capacity(args.len());
    for a in args {
        arg_values.push(eval(a, env, state, call_states, project, site_clocks, cov)?);
    }

    // Project-wide constants are visible inside every callee body. We
    // re-evaluate them here rather than threading a state through every eval
    // — constants don't use temporal operators or calls, so the throwaway
    // state/call_states never matter.
    let mut callee_env: BTreeMap<String, Value> = BTreeMap::new();
    for pkg in &project.packages {
        for c in &pkg.constants {
            let mut throw_state = State::default();
            let mut throw_calls: HashMap<usize, State> = HashMap::new();
            if let Ok(v) = eval(
                &c.value,
                &callee_env,
                &mut throw_state,
                &mut throw_calls,
                project,
                None,
                &mut None,
            ) {
                callee_env.insert(c.name.clone(), v);
            }
        }
    }
    for (p, v) in callee.inputs.iter().zip(arg_values.into_iter()) {
        callee_env.insert(p.name.clone(), v);
    }
    for p in &callee.outputs {
        callee_env.insert(p.name.clone(), default_value(&p.ty, project));
    }
    for l in &callee.locals {
        callee_env.insert(l.name.clone(), default_value(&l.ty, project));
    }

    // Callee bodies need the same dependency-ordered walk as the entry node.
    let callee_order = ol_ir::evaluation_order(callee)
        .map_err(|e| SimError::EvalError(format!("`{}`: {e}", callee.name)))?;
    let callee_clocks = if ol_ir::node_uses_clocks(callee) {
        ol_ir::infer_clocks(callee)
    } else {
        ol_ir::ClockInfo::default()
    };

    match callee.kind {
        NodeKind::Function => {
            // Stateless: a single pass over the body with a throwaway state.
            let mut throwaway = State::default();
            for &i in &callee_order {
                let eq = &callee.equations[i];
                if let Some(ck) = callee_clocks.equation_clocks.get(i) {
                    if !clock_active(ck, &callee_env)? {
                        continue;
                    }
                }
                let v = eval_eq_rhs(
                    &eq.rhs,
                    &callee_env,
                    &mut throwaway,
                    call_states,
                    project,
                    Some(&callee_clocks.site_clocks),
                    cov,
                )?;
                bind_lhs(&mut callee_env, eq, v)?;
            }
            extract_output(callee, &mut callee_env)
        }
        NodeKind::Operator => {
            // Stateful: take this call site's State, evaluate the body in its
            // scope, snapshot, and put it back. The call-site key is the
            // address of the `Expr::Call` node — stable for Sim's lifetime.
            let key = call_expr as *const Expr as usize;
            let mut sub_state = call_states.remove(&key).unwrap_or_default();
            // Clocked locals/outputs hold their last value through inactive
            // cycles — reseed them from the instance's previous snapshot.
            for p in callee.outputs.iter().map(|p| &p.name).chain(callee.locals.iter().map(|l| &l.name)) {
                if let Some(v) = sub_state.prev.get(p) {
                    callee_env.insert(p.clone(), v.clone());
                }
            }
            for &i in &callee_order {
                let eq = &callee.equations[i];
                if let Some(ck) = callee_clocks.equation_clocks.get(i) {
                    if !clock_active(ck, &callee_env)? {
                        continue;
                    }
                }
                let v = eval_eq_rhs(
                    &eq.rhs,
                    &callee_env,
                    &mut sub_state,
                    call_states,
                    project,
                    Some(&callee_clocks.site_clocks),
                    cov,
                )?;
                bind_lhs(&mut callee_env, eq, v)?;
            }
            for ck in &callee_clocks.chains {
                if clock_active(ck, &callee_env)? {
                    *sub_state.clock_ticks.entry(ck.key()).or_insert(0) += 1;
                }
            }
            for (k, v) in &callee_env {
                sub_state.prev.insert(k.clone(), v.clone());
            }
            sub_state.cycle += 1;
            call_states.insert(key, sub_state);
            extract_output(callee, &mut callee_env)
        }
        NodeKind::Imported => unreachable!(),
    }
}

fn bind_lhs(
    env: &mut BTreeMap<String, Value>,
    eq: &ol_ir::Equation,
    value: Value,
) -> Result<(), SimError> {
    if eq.lhs.len() == 1 {
        env.insert(eq.lhs[0].clone(), value);
        Ok(())
    } else if let Value::Tuple(items) = value {
        for (n, v) in eq.lhs.iter().zip(items.into_iter()) {
            env.insert(n.clone(), v);
        }
        Ok(())
    } else {
        Err(SimError::EvalError(format!(
            "multi-output equation produced a non-tuple value: {value:?}"
        )))
    }
}

fn extract_output(callee: &NodeDef, env: &mut BTreeMap<String, Value>) -> Result<Value, SimError> {
    if callee.outputs.len() == 1 {
        Ok(env.remove(&callee.outputs[0].name).unwrap_or(Value::Bool(false)))
    } else {
        Ok(Value::Tuple(
            callee
                .outputs
                .iter()
                .map(|p| env.remove(&p.name).unwrap_or(Value::Bool(false)))
                .collect(),
        ))
    }
}

fn eval_binary(op: BinOp, l: Value, r: Value) -> Result<Value, SimError> {
    use Value::*;
    Ok(match (op, l, r) {
        (BinOp::And, Bool(a), Bool(b)) => Bool(a && b),
        (BinOp::Or, Bool(a), Bool(b)) => Bool(a || b),
        (BinOp::Xor, Bool(a), Bool(b)) => Bool(a ^ b),
        (BinOp::Implies, Bool(a), Bool(b)) => Bool(!a || b),
        (BinOp::Eq, a, b) => Bool(a == b),
        (BinOp::Neq, a, b) => Bool(a != b),
        (BinOp::Lt, Int(a), Int(b)) => Bool(a < b),
        (BinOp::Le, Int(a), Int(b)) => Bool(a <= b),
        (BinOp::Gt, Int(a), Int(b)) => Bool(a > b),
        (BinOp::Ge, Int(a), Int(b)) => Bool(a >= b),
        (BinOp::Lt, Float(a), Float(b)) => Bool(a < b),
        (BinOp::Le, Float(a), Float(b)) => Bool(a <= b),
        (BinOp::Gt, Float(a), Float(b)) => Bool(a > b),
        (BinOp::Ge, Float(a), Float(b)) => Bool(a >= b),
        (BinOp::Add, Int(a), Int(b)) => Int(a + b),
        (BinOp::Sub, Int(a), Int(b)) => Int(a - b),
        (BinOp::Mul, Int(a), Int(b)) => Int(a * b),
        (BinOp::Div, Int(a), Int(b)) if b != 0 => Int(a / b),
        (BinOp::Mod, Int(a), Int(b)) if b != 0 => Int(a % b),
        (BinOp::Div, Int(_), Int(0)) => {
            return Err(SimError::EvalError("integer division by zero".into()))
        }
        (BinOp::Mod, Int(_), Int(0)) => {
            return Err(SimError::EvalError("modulo by zero".into()))
        }
        (BinOp::BitAnd, Int(a), Int(b)) => Int(a & b),
        (BinOp::BitOr, Int(a), Int(b)) => Int(a | b),
        (BinOp::BitXor, Int(a), Int(b)) => Int(a ^ b),
        (BinOp::Shl, Int(a), Int(b)) if (0..64).contains(&b) => Int(a.wrapping_shl(b as u32)),
        (BinOp::Shr, Int(a), Int(b)) if (0..64).contains(&b) => Int(a.wrapping_shr(b as u32)),
        (BinOp::Add, Float(a), Float(b)) => Float(a + b),
        (BinOp::Sub, Float(a), Float(b)) => Float(a - b),
        (BinOp::Mul, Float(a), Float(b)) => Float(a * b),
        (BinOp::Div, Float(a), Float(b)) if b != 0.0 => Float(a / b),
        (BinOp::Div, Float(_), Float(b)) if b == 0.0 => {
            return Err(SimError::EvalError("float division by zero".into()))
        }
        // --- Fixed-point: integer ops on the stored value (same format on both
        // sides is guaranteed by the type checker). Eq/Neq are handled by the
        // generic value-equality arms above. ---------------------------------
        // Add/sub do NOT re-narrow the intermediate: C promotes and narrows only
        // at assignment (a no-op for in-range values), so the carriers stay wide
        // on both sides — keeping nested `(a+b)*c` bit-identical to the emitter.
        (BinOp::Add, Fixed { stored: a, signed, bits, frac }, Fixed { stored: b, .. }) => Fixed {
            stored: a.wrapping_add(b),
            signed,
            bits,
            frac,
        },
        (BinOp::Sub, Fixed { stored: a, signed, bits, frac }, Fixed { stored: b, .. }) => Fixed {
            stored: a.wrapping_sub(b),
            signed,
            bits,
            frac,
        },
        // Multiply: i64 intermediate then `>> frac`, wrapped to the storage
        // width — identical to the generated `(intN)(((int64_t)a*b) >> frac)`.
        (BinOp::Mul, Fixed { stored: a, signed, bits, frac }, Fixed { stored: b, .. }) => Fixed {
            stored: narrow_fixed(signed, bits, a.wrapping_mul(b) >> frac),
            signed,
            bits,
            frac,
        },
        // Divide: rescale the numerator by frac before the integer divide so the
        // quotient stays in Q-format. i64 intermediate, truncates toward zero —
        // identical to the generated `(intN)(((int64_t)a << frac) / b)`.
        (BinOp::Div, Fixed { stored: a, signed, bits, frac }, Fixed { stored: b, .. }) if b != 0 => {
            Fixed { stored: narrow_fixed(signed, bits, a.wrapping_shl(frac) / b), signed, bits, frac }
        }
        (BinOp::Div | BinOp::SatDiv, Fixed { .. }, Fixed { stored: 0, .. }) => {
            return Err(SimError::EvalError("fixed-point division by zero".into()))
        }
        // Saturating ops: same i64 intermediate as their plain counterparts, then
        // clamp to the type's [min,max] (no wrap). The bound comes from the shared
        // `fixed_sat_range`, so it is identical to the C-Lite emitter.
        (BinOp::SatAdd, Fixed { stored: a, signed, bits, frac }, Fixed { stored: b, .. }) => Fixed {
            stored: clamp_fixed(signed, bits, a.wrapping_add(b)),
            signed,
            bits,
            frac,
        },
        (BinOp::SatSub, Fixed { stored: a, signed, bits, frac }, Fixed { stored: b, .. }) => Fixed {
            stored: clamp_fixed(signed, bits, a.wrapping_sub(b)),
            signed,
            bits,
            frac,
        },
        (BinOp::SatMul, Fixed { stored: a, signed, bits, frac }, Fixed { stored: b, .. }) => Fixed {
            stored: clamp_fixed(signed, bits, a.wrapping_mul(b) >> frac),
            signed,
            bits,
            frac,
        },
        (BinOp::SatDiv, Fixed { stored: a, signed, bits, frac }, Fixed { stored: b, .. }) if b != 0 => {
            Fixed { stored: clamp_fixed(signed, bits, a.wrapping_shl(frac) / b), signed, bits, frac }
        }
        (BinOp::Lt, Fixed { stored: a, .. }, Fixed { stored: b, .. }) => Bool(a < b),
        (BinOp::Le, Fixed { stored: a, .. }, Fixed { stored: b, .. }) => Bool(a <= b),
        (BinOp::Gt, Fixed { stored: a, .. }, Fixed { stored: b, .. }) => Bool(a > b),
        (BinOp::Ge, Fixed { stored: a, .. }, Fixed { stored: b, .. }) => Bool(a >= b),
        (op, l, r) => {
            return Err(SimError::EvalError(format!(
                "binary {op:?} not supported on {l:?} and {r:?}"
            )))
        }
    })
}

// --- Decision coverage (toward MC/DC) ---------------------------------------
//
// A decision site is the condition of an `if/then/else`. A scenario suite
// achieves decision coverage of a site when the condition has evaluated to
// BOTH true and false across the suite. Sites are registered for the entry
// node and every node it transitively calls, keyed by the condition
// expression's address in the IR (stable for the Sim's lifetime, exactly
// like call-site state).

/// One if-condition with its observed outcomes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DecisionSite {
    /// Node the decision lives in.
    pub node: String,
    /// The equation's lhs, for locating the decision.
    pub context: String,
    /// The condition, rendered in Lustre surface syntax.
    pub condition: String,
    pub seen_true: bool,
    pub seen_false: bool,
}

// --- MC/DC (Modified Condition/Decision Coverage) ---------------------------
//
// A DECISION is a boolean expression: an `if/then/else` condition, or a
// boolean equation right-hand side with two or more conditions. Its
// CONDITIONS are the atomic boolean leaves — the operands left once the
// boolean connectives (`and`/`or`/`xor`/`implies`/`not`) are peeled away.
// For each evaluation we record a TRIAL: every condition's value plus the
// decision's outcome. MC/DC is achieved for a condition when the suite holds
// some pair of trials that differ in only that condition and flip the
// outcome — demonstrating the condition independently affects the result.

/// One observed evaluation of a decision: each condition's value (in the
/// decision's fixed condition order) and the resulting outcome.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct McdcTrial {
    pub values: Vec<bool>,
    pub outcome: bool,
}

/// A decision tracked for MC/DC, with its conditions and the distinct trials
/// observed so far.
#[derive(Debug, Clone, serde::Serialize)]
pub struct McdcDecision {
    pub node: String,
    pub context: String,
    pub decision: String,
    pub conditions: Vec<String>,
    /// The decision's boolean structure over condition indices — what the
    /// masking analysis re-evaluates.
    pub shape: DecisionShape,
    pub trials: Vec<McdcTrial>,
}

/// The boolean structure of a decision with its atomic conditions abstracted
/// to indices (into the decision's condition list). Recorded at coverage
/// registration so masking MC/DC can re-evaluate the decision on modified
/// condition vectors without re-running the model.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum DecisionShape {
    /// Condition `i` of the decision.
    Cond(usize),
    Not(Box<DecisionShape>),
    And(Box<DecisionShape>, Box<DecisionShape>),
    Or(Box<DecisionShape>, Box<DecisionShape>),
    Xor(Box<DecisionShape>, Box<DecisionShape>),
    Implies(Box<DecisionShape>, Box<DecisionShape>),
}

impl DecisionShape {
    /// The decision's outcome for one assignment of its conditions.
    pub fn eval(&self, values: &[bool]) -> bool {
        match self {
            DecisionShape::Cond(i) => values.get(*i).copied().unwrap_or(false),
            DecisionShape::Not(a) => !a.eval(values),
            DecisionShape::And(a, b) => a.eval(values) && b.eval(values),
            DecisionShape::Or(a, b) => a.eval(values) || b.eval(values),
            DecisionShape::Xor(a, b) => a.eval(values) ^ b.eval(values),
            DecisionShape::Implies(a, b) => !a.eval(values) || b.eval(values),
        }
    }
}

#[derive(Debug)]
struct DecisionData {
    node: String,
    context: String,
    decision_text: String,
    cond_ptrs: Vec<usize>,
    cond_texts: Vec<String>,
    shape: DecisionShape,
    trials: Vec<McdcTrial>,
    seen: std::collections::HashSet<McdcTrial>,
}

#[derive(Debug, Default)]
pub struct Coverage {
    /// ptr-of-cond -> index into `sites` (legacy decision coverage).
    index: HashMap<usize, usize>,
    sites: Vec<DecisionSite>,
    /// ptr-of-decision-root -> index into `decisions` (MC/DC).
    roots: HashMap<usize, usize>,
    decisions: Vec<DecisionData>,
}

impl Coverage {
    fn mark(&mut self, key: usize, outcome: bool) {
        if let Some(&i) = self.index.get(&key) {
            if outcome {
                self.sites[i].seen_true = true;
            } else {
                self.sites[i].seen_false = true;
            }
        }
    }

    fn is_root(&self, ptr: usize) -> bool {
        self.roots.contains_key(&ptr)
    }

    /// Record one trial of the decision rooted at `root`, projecting the
    /// observed atomic values into the decision's fixed condition order.
    fn record_trial(&mut self, root: usize, obs: &BTreeMap<usize, bool>, outcome: bool) {
        if let Some(&di) = self.roots.get(&root) {
            let d = &mut self.decisions[di];
            let values: Vec<bool> = d
                .cond_ptrs
                .iter()
                .map(|p| obs.get(p).copied().unwrap_or(false))
                .collect();
            let t = McdcTrial { values, outcome };
            if d.seen.insert(t.clone()) {
                d.trials.push(t);
            }
        }
    }
}

/// The atomic boolean conditions of a decision, in evaluation order: descend
/// through the boolean connectives, treat everything else as one condition.
/// Each leaf is keyed by its address (stable for the Sim's lifetime).
fn decision_conditions(expr: &Expr) -> Vec<(usize, String)> {
    decision_structure(expr).0
}

/// The conditions AND the decision's boolean structure over their indices,
/// in one traversal so the leaf order agrees.
fn decision_structure(expr: &Expr) -> (Vec<(usize, String)>, DecisionShape) {
    fn go(e: &Expr, out: &mut Vec<(usize, String)>) -> DecisionShape {
        match e {
            Expr::Binary { op: op @ (BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Implies), lhs, rhs } => {
                let l = Box::new(go(lhs, out));
                let r = Box::new(go(rhs, out));
                match op {
                    BinOp::And => DecisionShape::And(l, r),
                    BinOp::Or => DecisionShape::Or(l, r),
                    BinOp::Xor => DecisionShape::Xor(l, r),
                    _ => DecisionShape::Implies(l, r),
                }
            }
            Expr::Unary { op: UnaryOp::Not, arg } => DecisionShape::Not(Box::new(go(arg, out))),
            _ => {
                out.push((e as *const Expr as usize, ol_lustre_emit::format_expr(e)));
                DecisionShape::Cond(out.len() - 1)
            }
        }
    }
    let mut v = Vec::new();
    let shape = go(expr, &mut v);
    (v, shape)
}

/// Unique-cause MC/DC analysis. For each condition, find two trials that
/// differ in only that condition yet produce opposite outcomes — the pair
/// that demonstrates the condition independently affects the decision.
/// Returns, per condition, the indices of such a pair, or `None`.
pub fn mcdc_independence(num_conditions: usize, trials: &[McdcTrial]) -> Vec<Option<(usize, usize)>> {
    (0..num_conditions)
        .map(|i| {
            for a in 0..trials.len() {
                for b in (a + 1)..trials.len() {
                    let (ta, tb) = (&trials[a], &trials[b]);
                    if ta.values.len() != num_conditions || tb.values.len() != num_conditions {
                        continue;
                    }
                    if ta.outcome != tb.outcome
                        && ta.values[i] != tb.values[i]
                        && (0..num_conditions).filter(|&j| j != i).all(|j| ta.values[j] == tb.values[j])
                    {
                        return Some((a, b));
                    }
                }
            }
            None
        })
        .collect()
}

/// Masking MC/DC analysis — the DO-178C-accepted alternative to unique-cause
/// (CAST-6). A condition's independent effect is demonstrated by a pair of
/// trials where the condition differs, the outcome differs, and the
/// condition is CONTROLLING in both trials: flipping it (alone) flips the
/// decision as re-evaluated over `shape`, so every other differing condition
/// is masked. Textually identical conditions are treated as one coupled
/// condition — they always carry the same value, so unique-cause can never
/// isolate them, and flipping toggles every instance together.
pub fn mcdc_masking_independence(
    shape: &DecisionShape,
    conditions: &[String],
    trials: &[McdcTrial],
) -> Vec<Option<(usize, usize)>> {
    let n = conditions.len();
    // Coupled group per condition: every index with the same text.
    let group: Vec<Vec<usize>> = (0..n)
        .map(|i| (0..n).filter(|&j| conditions[j] == conditions[i]).collect())
        .collect();
    let controlling = |values: &[bool], g: &[usize]| -> bool {
        let mut flipped = values.to_vec();
        for &j in g {
            flipped[j] = !flipped[j];
        }
        shape.eval(values) != shape.eval(&flipped)
    };
    (0..n)
        .map(|i| {
            for a in 0..trials.len() {
                for b in (a + 1)..trials.len() {
                    let (ta, tb) = (&trials[a], &trials[b]);
                    if ta.values.len() != n || tb.values.len() != n {
                        continue;
                    }
                    if ta.outcome != tb.outcome
                        && ta.values[i] != tb.values[i]
                        && controlling(&ta.values, &group[i])
                        && controlling(&tb.values, &group[i])
                    {
                        return Some((a, b));
                    }
                }
            }
            None
        })
        .collect()
}

impl<'a> Sim<'a> {
    /// Turn on decision-coverage collection. Registers every if-condition in
    /// the entry node and all nodes it transitively calls. Subsequent
    /// `step`/`run_csv*` calls accumulate outcomes; read them back with
    /// [`Sim::coverage_sites`].
    pub fn enable_coverage(&mut self) {
        let mut coverage = Coverage::default();
        let mut visited: std::collections::BTreeSet<String> = Default::default();
        let mut queue: std::collections::VecDeque<&str> = Default::default();
        queue.push_back(self.node.name.as_str());
        while let Some(name) = queue.pop_front() {
            if !visited.insert(name.to_string()) {
                continue;
            }
            let Some(node) = self.project.find_node(name) else { continue };
            for eq in &node.equations {
                let ctx = eq.lhs.join(", ");
                // Register a decision root with its atomic conditions (MC/DC).
                let mut register_decision = |root: &Expr| {
                    let key = root as *const Expr as usize;
                    if coverage.roots.contains_key(&key) {
                        return;
                    }
                    let (conds, shape) = decision_structure(root);
                    let idx = coverage.decisions.len();
                    coverage.roots.insert(key, idx);
                    coverage.decisions.push(DecisionData {
                        node: node.name.clone(),
                        context: ctx.clone(),
                        decision_text: ol_lustre_emit::format_expr(root),
                        cond_ptrs: conds.iter().map(|(p, _)| *p).collect(),
                        cond_texts: conds.iter().map(|(_, t)| t.clone()).collect(),
                        shape,
                        trials: Vec::new(),
                        seen: Default::default(),
                    });
                };
                // A boolean equation RHS with two or more conditions is a
                // decision in its own right (dataflow logic without `if`).
                if decision_conditions(&eq.rhs).len() >= 2 {
                    register_decision(&eq.rhs);
                }
                eq.rhs.visit(|e| {
                    match e {
                        Expr::IfThenElse { cond, .. } => {
                            let key = cond.as_ref() as *const Expr as usize;
                            let idx = coverage.sites.len();
                            if coverage.index.insert(key, idx).is_none() {
                                coverage.sites.push(DecisionSite {
                                    node: node.name.clone(),
                                    context: ctx.clone(),
                                    condition: ol_lustre_emit::format_expr(cond),
                                    seen_true: false,
                                    seen_false: false,
                                });
                            }
                            // Every if-condition is also an MC/DC decision.
                            register_decision(cond);
                        }
                        Expr::Call { node: callee, .. } => {
                            // visit() gives no &mut access for the queue from
                            // the closure; collect names afterwards instead.
                            let _ = callee;
                        }
                        _ => {}
                    }
                });
                // Enqueue callees (separate pass: visit's closure can't
                // borrow the queue mutably while `coverage` is also captured).
                let mut callees: Vec<String> = Vec::new();
                eq.rhs.visit(|e| {
                    if let Expr::Call { node: callee, .. } = e {
                        callees.push(callee.clone());
                    }
                });
                for c in callees {
                    if !visited.contains(&c) {
                        // Safe: names outlive the loop via the project borrow.
                        if let Some(n) = self.project.find_node(&c) {
                            queue.push_back(&n.name);
                        }
                    }
                }
            }
        }
        self.coverage = Some(coverage);
    }

    /// Snapshot of every registered decision site and its outcomes so far.
    /// `None` until [`Sim::enable_coverage`] is called.
    pub fn coverage_sites(&self) -> Option<&[DecisionSite]> {
        self.coverage.as_ref().map(|c| c.sites.as_slice())
    }

    /// MC/DC decisions and the distinct trials observed so far. `None` until
    /// [`Sim::enable_coverage`] is called.
    pub fn mcdc_decisions(&self) -> Option<Vec<McdcDecision>> {
        self.coverage.as_ref().map(|c| {
            c.decisions
                .iter()
                .map(|d| McdcDecision {
                    node: d.node.clone(),
                    context: d.context.clone(),
                    decision: d.decision_text.clone(),
                    conditions: d.cond_texts.clone(),
                    shape: d.shape.clone(),
                    trials: d.trials.clone(),
                })
                .collect()
        })
    }
}
