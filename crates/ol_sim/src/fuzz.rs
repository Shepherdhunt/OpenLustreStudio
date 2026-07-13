//! Fuzz simulation: random, type-aware input exploration of one operator.
//!
//! The fuzzer drives the same cycle-accurate interpreter the Studio's watch
//! table uses — so everything the operator executes, including every
//! suboperator call, is exercised and monitored. Each iteration runs a fresh
//! [`Sim`] for a bounded number of cycles with pseudo-random inputs on the
//! user-selected ports (the rest hold pinned or default values), and every
//! cycle is checked for:
//!
//! - **crashes** — evaluation errors (division by zero, unsupported ops) and
//!   interpreter panics (e.g. debug-build integer overflow), caught per step;
//! - **contract violations** — the node's assume/guarantee monitor, evaluated
//!   exactly as the simulator's trace view does;
//! - **non-finite values** — any output or local that becomes `inf`/`NaN`;
//! - **user error predicates** — boolean expressions over the node's inputs,
//!   outputs, and locals (temporal operators like `pre`/`->` are allowed;
//!   each predicate keeps its own state across the iteration's cycles). A
//!   predicate evaluating to `true` is a finding.
//!
//! Findings are deduplicated by kind+detail: the first occurrence keeps its
//! full input trace (every input column, cycle 0 through the failing cycle,
//! in the simulator's CSV value syntax) so the Studio can replay it through
//! the watch table — the same mechanism used for Kind 2 counterexamples —
//! and later occurrences only bump a counter. Runs are deterministic for a
//! given seed.

use std::collections::{BTreeMap, HashMap, HashSet};

use ol_ir::{Expr, Project, Type, TypeBody};

use crate::{
    default_value, evaluate_monitor, eval, parse_value, Sim, SimError, State, Value,
};

/// A user-supplied error predicate: `expr` is a boolean expression over the
/// fuzzed node's inputs, outputs, and locals; `true` at any cycle is a
/// finding named `name`.
#[derive(Debug, Clone)]
pub struct FuzzPredicate {
    pub name: String,
    pub expr: Expr,
}

#[derive(Debug, Clone)]
pub struct FuzzConfig {
    /// Names of the inputs to fuzz. Empty means "every fuzzable input".
    /// Non-listed inputs hold their pinned value (see `held`) or the type
    /// default for the whole run.
    pub fuzz_inputs: Vec<String>,
    /// Cycles per iteration.
    pub cycles: usize,
    /// Number of independent iterations (each with a fresh simulator state).
    pub iterations: usize,
    /// PRNG seed — equal seeds give identical runs.
    pub seed: u64,
    /// User error predicates, checked every cycle.
    pub predicates: Vec<FuzzPredicate>,
    /// Pinned values for non-fuzzed inputs, in the simulator's CSV value
    /// syntax (e.g. `true`, `-3`, `1.5`, `VariantName`, `[1;2;3]`).
    pub held: BTreeMap<String, String>,
    /// Stop after this many distinct findings (0 = no cap).
    pub max_findings: usize,
}

impl Default for FuzzConfig {
    fn default() -> Self {
        FuzzConfig {
            fuzz_inputs: Vec::new(),
            cycles: 25,
            iterations: 200,
            seed: 1,
            predicates: Vec::new(),
            held: BTreeMap::new(),
            max_findings: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FindingKind {
    /// The interpreter failed (evaluation error) or panicked mid-step.
    Crash,
    /// A contract guarantee/mode clause was violated (label from the monitor).
    ContractViolation,
    /// An output or local became `inf`/`NaN`.
    NonFinite,
    /// A user error predicate evaluated to `true`.
    Predicate,
}

impl FindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FindingKind::Crash => "crash",
            FindingKind::ContractViolation => "contract_violation",
            FindingKind::NonFinite => "non_finite",
            FindingKind::Predicate => "predicate",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FuzzFinding {
    pub kind: FindingKind,
    /// Human-readable detail: the crash message, violated clause label,
    /// non-finite item name, or predicate name.
    pub detail: String,
    /// Iteration and 0-based cycle of the FIRST occurrence.
    pub iteration: usize,
    pub cycle: usize,
    /// How many (iteration, cycle) hits deduplicated into this finding.
    pub occurrences: usize,
    /// Input trace of the first occurrence: one column per node input (in
    /// declaration order), rows for cycle 0 ..= failing cycle, values in the
    /// simulator's CSV syntax — directly replayable through the watch table.
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Output (and violated-signal) values at the failing cycle, for display.
    /// Empty when the step itself crashed before producing values.
    pub outputs: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct FuzzReport {
    pub node: String,
    /// Inputs that were actually fuzzed (fuzzable ∩ requested, or all
    /// fuzzable when none were requested).
    pub fuzzed_inputs: Vec<String>,
    /// Inputs that could not be fuzzed because their type has no CSV form
    /// (e.g. records) — they held their default for the whole run.
    pub unfuzzable_inputs: Vec<String>,
    pub iterations_run: usize,
    pub total_cycles: usize,
    pub seed: u64,
    pub findings: Vec<FuzzFinding>,
}

impl FuzzReport {
    pub fn clean(&self) -> bool {
        self.findings.is_empty()
    }
}

// --- Deterministic PRNG (splitmix64) -----------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Avoid the all-zero fixed point without changing distinct seeds.
        Rng(seed.wrapping_add(0x9E3779B97F4A7C15))
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[0, n)` (n > 0).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// Uniform in `[lo, hi]` inclusive.
    fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        let span = (hi as i128) - (lo as i128) + 1;
        let r = (self.next() as u128 % span as u128) as i128;
        (lo as i128 + r) as i64
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    fn f64_unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

// --- Type-aware value generation ----------------------------------------------

fn int_bounds(ty: &Type) -> (i64, i64) {
    match ty {
        Type::Int8 => (i8::MIN as i64, i8::MAX as i64),
        Type::Int16 => (i16::MIN as i64, i16::MAX as i64),
        Type::Int32 => (i32::MIN as i64, i32::MAX as i64),
        Type::Int64 => (i64::MIN, i64::MAX),
        Type::Uint8 => (0, u8::MAX as i64),
        Type::Uint16 => (0, u16::MAX as i64),
        Type::Uint32 => (0, u32::MAX as i64),
        // The simulator carries uint64 in an i64; stay in the representable
        // non-negative half.
        Type::Uint64 => (0, i64::MAX),
        _ => (i64::MIN, i64::MAX),
    }
}

/// Can values of this type cross the CSV boundary (and therefore be fuzzed,
/// traced, and replayed)? Mirrors `parse_value`.
fn is_fuzzable(ty: &Type, project: &Project) -> bool {
    match ty {
        Type::Bool | Type::Char | Type::Fixed { .. } => true,
        t if t.is_integer() || t.is_float() => true,
        Type::Array { elem, .. } => is_fuzzable(elem, project),
        Type::Named { name } => resolve_named(name, project).map_or(false, |body| match body {
            TypeBody::Enum(_) => true,
            TypeBody::Alias { target, .. } => is_fuzzable(target, project),
            TypeBody::Record { .. } => false,
        }),
        _ => false,
    }
}

fn resolve_named<'p>(name: &str, project: &'p Project) -> Option<&'p TypeBody> {
    project
        .packages
        .iter()
        .flat_map(|p| &p.types)
        .find(|t| t.name() == name)
        .map(|t| &t.body)
}

/// One pseudo-random value of type `ty`. `prev` is the input's previous value
/// for stickiness: synchronous machines respond to *held* inputs (a guard like
/// `set_on and speed > 40` almost never fires under white noise), so with
/// probability ~55% the previous value is kept.
fn gen_value(ty: &Type, project: &Project, rng: &mut Rng, prev: Option<&Value>) -> Value {
    if let Some(p) = prev {
        if rng.chance(55) {
            return p.clone();
        }
    }
    fresh_value(ty, project, rng)
}

fn fresh_value(ty: &Type, project: &Project, rng: &mut Rng) -> Value {
    match ty {
        Type::Bool => Value::Bool(rng.chance(50)),
        Type::Char => Value::Int(rng.range_i64(32, 126)),
        t if t.is_float() => {
            // Boundary-heavy menu, then uniform. Finite only: the model is the
            // one that should produce inf/NaN, not the harness.
            let menu = [0.0, 1.0, -1.0, 0.5, -0.5, 1e-6, -1e-6, 1e3, -1e3, 1e6, -1e6];
            let v = if rng.chance(45) {
                menu[rng.below(menu.len() as u64) as usize]
            } else {
                (rng.f64_unit() - 0.5) * 2e4
            };
            // Keep float32 inputs exactly representable so traces round-trip.
            if matches!(t, Type::Float32) { Value::Float(v as f32 as f64) } else { Value::Float(v) }
        }
        t if t.is_integer() => {
            let (lo, hi) = int_bounds(t);
            let v = if rng.chance(35) {
                // Boundaries and small values: where off-by-one and overflow live.
                let menu = [0, 1, 2, -1, -2, lo, hi, lo + 1, hi - 1];
                menu[rng.below(menu.len() as u64) as usize].clamp(lo, hi)
            } else if rng.chance(55) {
                rng.range_i64(lo.max(-16), hi.min(16))
            } else {
                rng.range_i64(lo, hi)
            };
            Value::Int(v)
        }
        Type::Fixed { signed, bits, frac } => {
            let hi = if *signed { (1i64 << (bits - 1)) - 1 } else { (1i64 << (*bits).min(62)) - 1 };
            let lo = if *signed { -(1i64 << (bits - 1)) } else { 0 };
            let stored = if rng.chance(35) {
                let menu = [0, 1, -1, lo, hi, 1i64 << frac, -(1i64 << frac)];
                menu[rng.below(menu.len() as u64) as usize].clamp(lo, hi)
            } else {
                rng.range_i64(lo, hi)
            };
            Value::Fixed { stored, signed: *signed, bits: *bits, frac: *frac }
        }
        Type::Array { elem, len } => {
            Value::Array((0..*len).map(|_| fresh_value(elem, project, rng)).collect())
        }
        Type::Named { name } => match resolve_named(name, project) {
            Some(TypeBody::Enum(e)) if !e.variants.is_empty() => {
                Value::Enum(e.variants[rng.below(e.variants.len() as u64) as usize].clone())
            }
            Some(TypeBody::Alias { target, .. }) => {
                let target = target.clone();
                fresh_value(&target, project, rng)
            }
            _ => default_value(ty, project),
        },
        _ => default_value(ty, project),
    }
}

// --- The fuzz loop -------------------------------------------------------------

pub fn fuzz_node(
    project: &Project,
    node_name: &str,
    cfg: &FuzzConfig,
) -> Result<FuzzReport, SimError> {
    let node = project
        .find_node(node_name)
        .ok_or_else(|| SimError::UnknownNode(node_name.to_string()))?;
    if cfg.cycles == 0 || cfg.iterations == 0 {
        return Err(SimError::EvalError("fuzz needs cycles ≥ 1 and iterations ≥ 1".into()));
    }

    let input_names: Vec<String> = node.inputs.iter().map(|p| p.name.clone()).collect();
    let mut unfuzzable: Vec<String> = Vec::new();
    let fuzzable: HashSet<String> = node
        .inputs
        .iter()
        .filter(|p| {
            let ok = is_fuzzable(&p.ty, project);
            if !ok {
                unfuzzable.push(p.name.clone());
            }
            ok
        })
        .map(|p| p.name.clone())
        .collect();

    let fuzzed: Vec<String> = if cfg.fuzz_inputs.is_empty() {
        input_names.iter().filter(|n| fuzzable.contains(*n)).cloned().collect()
    } else {
        for n in &cfg.fuzz_inputs {
            if !input_names.contains(n) {
                return Err(SimError::EvalError(format!(
                    "cannot fuzz `{n}`: `{node_name}` has no such input"
                )));
            }
            if !fuzzable.contains(n) {
                return Err(SimError::EvalError(format!(
                    "cannot fuzz `{n}`: its type has no CSV form to trace and replay"
                )));
            }
        }
        cfg.fuzz_inputs.clone()
    };
    if fuzzed.is_empty() {
        return Err(SimError::EvalError(format!(
            "`{node_name}` has no fuzzable inputs"
        )));
    }

    // Pinned values for non-fuzzed inputs (validated up front), else defaults.
    let mut held: BTreeMap<String, Value> = BTreeMap::new();
    for p in &node.inputs {
        if fuzzed.contains(&p.name) {
            continue;
        }
        let v = match cfg.held.get(&p.name) {
            Some(raw) => parse_value(raw.trim(), &p.ty, project).map_err(|_| {
                SimError::EvalError(format!(
                    "held value `{raw}` does not parse as {:?} for input `{}`",
                    p.ty, p.name
                ))
            })?,
            None => default_value(&p.ty, project),
        };
        held.insert(p.name.clone(), v);
    }
    for name in cfg.held.keys() {
        if !input_names.contains(name) {
            return Err(SimError::EvalError(format!(
                "held value for `{name}`: `{node_name}` has no such input"
            )));
        }
    }

    // Validate predicates once against a defaults env so a typo fails the
    // request, not every cycle of the run.
    {
        let probe = Sim::new(project, node_name)?;
        let mut env: BTreeMap<String, Value> = probe.consts.clone();
        for p in node.inputs.iter().chain(node.outputs.iter()) {
            env.insert(p.name.clone(), default_value(&p.ty, project));
        }
        for l in &node.locals {
            env.insert(l.name.clone(), default_value(&l.ty, project));
        }
        for pr in &cfg.predicates {
            let mut st = State::default();
            let mut cs: HashMap<usize, State> = HashMap::new();
            match eval(&pr.expr, &env, &mut st, &mut cs, project, None, &mut None) {
                Ok(Value::Bool(_)) => {}
                Ok(v) => {
                    return Err(SimError::EvalError(format!(
                        "error predicate `{}` is not boolean (got {v:?})",
                        pr.name
                    )))
                }
                Err(e) => {
                    return Err(SimError::EvalError(format!(
                        "error predicate `{}` does not evaluate: {e}",
                        pr.name
                    )))
                }
            }
        }
    }

    let mut rng = Rng::new(cfg.seed);
    let mut findings: Vec<FuzzFinding> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut total_cycles = 0usize;
    let mut iterations_run = 0usize;

    'iterations: for iteration in 0..cfg.iterations {
        iterations_run = iteration + 1;
        let mut sim = Sim::new(project, node_name)?;
        // Per-predicate interpreter state, fresh each iteration, persistent
        // across its cycles — so `pre`/`->` in predicates mean what they mean
        // everywhere else.
        let mut pred_states: Vec<(State, HashMap<usize, State>)> = cfg
            .predicates
            .iter()
            .map(|_| (State::default(), HashMap::new()))
            .collect();
        let mut prev: BTreeMap<String, Value> = BTreeMap::new();
        let mut rows: Vec<Vec<String>> = Vec::new();

        for cycle in 0..cfg.cycles {
            // Assemble this cycle's inputs: sticky-random on fuzzed ports,
            // pinned/default elsewhere.
            let mut inputs: BTreeMap<String, Value> = BTreeMap::new();
            for p in &node.inputs {
                let v = if fuzzed.contains(&p.name) {
                    gen_value(&p.ty, project, &mut rng, prev.get(&p.name))
                } else {
                    held.get(&p.name).cloned().unwrap_or_else(|| default_value(&p.ty, project))
                };
                inputs.insert(p.name.clone(), v);
            }
            for (k, v) in &inputs {
                prev.insert(k.clone(), v.clone());
            }
            rows.push(
                node.inputs.iter().map(|p| inputs[&p.name].to_csv()).collect::<Vec<String>>(),
            );
            total_cycles += 1;

            // One interpreter step, with panics (debug-build overflow and
            // friends) caught and reported as crash findings.
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sim.step_env(&inputs)
            }));
            let env = match step {
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "interpreter panic".into());
                    record(
                        &mut findings,
                        &mut index,
                        FindingKind::Crash,
                        format!("panic: {msg}"),
                        iteration,
                        cycle,
                        &input_names,
                        &rows,
                        Vec::new(),
                    );
                    if capped(&findings, cfg) {
                        break 'iterations;
                    }
                    break; // this simulator is poisoned — next iteration
                }
                Ok(Err(e)) => {
                    record(
                        &mut findings,
                        &mut index,
                        FindingKind::Crash,
                        e.to_string(),
                        iteration,
                        cycle,
                        &input_names,
                        &rows,
                        Vec::new(),
                    );
                    if capped(&findings, cfg) {
                        break 'iterations;
                    }
                    break;
                }
                Ok(Ok(env)) => env,
            };

            let outputs_at = |env: &BTreeMap<String, Value>| -> Vec<(String, String)> {
                node.outputs
                    .iter()
                    .map(|p| {
                        (
                            p.name.clone(),
                            env.get(&p.name)
                                .map(|v| v.to_csv())
                                .unwrap_or_else(|| "—".into()),
                        )
                    })
                    .collect()
            };

            // Contract monitor — same evaluation the trace view records.
            if let Some(c) = &sim.contract {
                let mut outs: BTreeMap<String, Value> = BTreeMap::new();
                for p in &node.outputs {
                    if let Some(v) = env.get(&p.name) {
                        outs.insert(p.name.clone(), v.clone());
                    }
                }
                let step = evaluate_monitor(c, &inputs, &outs);
                for label in step.violations {
                    record(
                        &mut findings,
                        &mut index,
                        FindingKind::ContractViolation,
                        label,
                        iteration,
                        cycle,
                        &input_names,
                        &rows,
                        outputs_at(&env),
                    );
                    if capped(&findings, cfg) {
                        break 'iterations;
                    }
                }
            }

            // Non-finite outputs/locals.
            for p in node.outputs.iter().map(|p| &p.name).chain(node.locals.iter().map(|l| &l.name)) {
                if let Some(Value::Float(f)) = env.get(p) {
                    if !f.is_finite() {
                        record(
                            &mut findings,
                            &mut index,
                            FindingKind::NonFinite,
                            format!("`{p}` became {f}"),
                            iteration,
                            cycle,
                            &input_names,
                            &rows,
                            outputs_at(&env),
                        );
                        if capped(&findings, cfg) {
                            break 'iterations;
                        }
                    }
                }
            }

            // User error predicates, over the full watch view (inputs,
            // outputs, locals — plus constants, already in env).
            for (i, pr) in cfg.predicates.iter().enumerate() {
                let (st, cs) = &mut pred_states[i];
                match eval(&pr.expr, &env, st, cs, project, None, &mut None) {
                    Ok(Value::Bool(true)) => {
                        record(
                            &mut findings,
                            &mut index,
                            FindingKind::Predicate,
                            pr.name.clone(),
                            iteration,
                            cycle,
                            &input_names,
                            &rows,
                            outputs_at(&env),
                        );
                        if capped(&findings, cfg) {
                            break 'iterations;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        record(
                            &mut findings,
                            &mut index,
                            FindingKind::Crash,
                            format!("error predicate `{}` failed at runtime: {e}", pr.name),
                            iteration,
                            cycle,
                            &input_names,
                            &rows,
                            outputs_at(&env),
                        );
                        if capped(&findings, cfg) {
                            break 'iterations;
                        }
                    }
                }
            }

            // End-of-cycle for the predicate evaluators — the same snapshot
            // `step_env` takes, so `pre x` and `->` in predicates see exactly
            // the equation semantics.
            for (st, _) in pred_states.iter_mut() {
                for (k, v) in &env {
                    st.prev.insert(k.clone(), v.clone());
                }
                st.cycle += 1;
            }
        }
    }

    Ok(FuzzReport {
        node: node_name.to_string(),
        fuzzed_inputs: fuzzed,
        unfuzzable_inputs: unfuzzable,
        iterations_run,
        total_cycles,
        seed: cfg.seed,
        findings,
    })
}

fn capped(findings: &[FuzzFinding], cfg: &FuzzConfig) -> bool {
    cfg.max_findings > 0 && findings.len() >= cfg.max_findings
}

#[allow(clippy::too_many_arguments)]
fn record(
    findings: &mut Vec<FuzzFinding>,
    index: &mut HashMap<String, usize>,
    kind: FindingKind,
    detail: String,
    iteration: usize,
    cycle: usize,
    columns: &[String],
    rows: &[Vec<String>],
    outputs: Vec<(String, String)>,
) {
    let key = format!("{}\u{1}{}", kind.as_str(), detail);
    if let Some(&i) = index.get(&key) {
        findings[i].occurrences += 1;
        return;
    }
    index.insert(key, findings.len());
    findings.push(FuzzFinding {
        kind,
        detail,
        iteration,
        cycle,
        occurrences: 1,
        columns: columns.to_vec(),
        rows: rows.to_vec(),
        outputs,
    });
}
