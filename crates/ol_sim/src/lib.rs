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

use ol_contract_ir::{parse_contracts, ContractDef};
use ol_ir::{BinOp, Expr, Literal, NodeDef, NodeKind, Project, Type, TypeBody, UnaryOp};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
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
                )
                .map_err(|e| SimError::EvalError(format!("constant `{}`: {e}", c.name)))?;
                consts.insert(c.name.clone(), v);
            }
        }

        Ok(Sim {
            project,
            node,
            state: State::default(),
            contract,
            call_states: HashMap::new(),
            consts,
        })
    }

    pub fn step(
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
        for p in &self.node.outputs {
            env.entry(p.name.clone())
                .or_insert_with(|| default_value(&p.ty, self.project));
        }
        for l in &self.node.locals {
            env.entry(l.name.clone())
                .or_insert_with(|| default_value(&l.ty, self.project));
        }

        // Combinational cycles have been ruled out at typecheck time, so a
        // single pass in declaration order suffices for the Phase 0 profile.
        for eq in &self.node.equations {
            let value = eval(
                &eq.rhs,
                &env,
                &mut self.state,
                &mut self.call_states,
                self.project,
            )?;
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

        for (k, v) in &env {
            self.state.prev.insert(k.clone(), v.clone());
        }
        self.state.cycle += 1;

        let mut outputs = BTreeMap::new();
        for p in &self.node.outputs {
            outputs.insert(
                p.name.clone(),
                env.get(&p.name).cloned().unwrap_or_else(|| default_value(&p.ty, self.project)),
            );
        }
        Ok(outputs)
    }

    pub fn run_csv(&mut self, csv: &str) -> Result<Trace, SimError> {
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
                let v = parse_value(raw, &p.ty).map_err(|_| SimError::ParseError {
                    value: raw.into(),
                    col: p.name.clone(),
                    ty: p.ty.clone(),
                })?;
                inputs.insert(p.name.clone(), v);
            }
            let out = self.step(&inputs)?;
            let mut out_row: Vec<Value> = vec![Value::Int(cycle as i64)];
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
        Type::Array { elem, len } => {
            Value::Array((0..*len).map(|_| default_value(elem, project)).collect())
        }
        Type::Named(name) => default_named(name, project).unwrap_or(Value::Int(0)),
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

fn parse_value(raw: &str, ty: &Type) -> Result<Value, ()> {
    match ty {
        Type::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "t" => Ok(Value::Bool(true)),
            "false" | "0" | "f" => Ok(Value::Bool(false)),
            _ => Err(()),
        },
        t if t.is_float() => raw.parse::<f64>().map(Value::Float).map_err(|_| ()),
        t if t.is_integer() => raw.parse::<i64>().map(Value::Int).map_err(|_| ()),
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
        match eval(&g.expr, &env, &mut state, &mut call_states, &project) {
            Ok(Value::Bool(true)) => {}
            _ => violations.push(label),
        }
    }

    // A mode is active when all of its `require` clauses hold; when active,
    // its `ensure` clauses must hold too.
    for m in &c.modes {
        let mut hit = true;
        for r in &m.requires {
            match eval(r, &env, &mut state, &mut call_states, &project) {
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
                match eval(e, &env, &mut state, &mut call_states, &project) {
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

fn eval(
    expr: &Expr,
    env: &BTreeMap<String, Value>,
    state: &mut State,
    call_states: &mut HashMap<usize, State>,
    project: &Project,
) -> Result<Value, SimError> {
    match expr {
        Expr::Const { lit } => Ok(match lit {
            Literal::Bool { value } => Value::Bool(*value),
            Literal::Int { value } => Value::Int(*value),
            Literal::Float { value } => Value::Float(*value),
        }),
        Expr::Var { name } => match env.get(name).cloned() {
            Some(v) => Ok(v),
            None => enum_variant_value(name, project)
                .ok_or_else(|| SimError::EvalError(format!("unbound variable `{name}`"))),
        },
        Expr::Unary { op, arg } => {
            let v = eval(arg, env, state, call_states, project)?;
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
            let l = eval(lhs, env, state, call_states, project)?;
            let r = eval(rhs, env, state, call_states, project)?;
            eval_binary(*op, l, r)
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            let c = eval(cond, env, state, call_states, project)?;
            match c {
                Value::Bool(true) => eval(then_branch, env, state, call_states, project),
                Value::Bool(false) => eval(else_branch, env, state, call_states, project),
                other => Err(SimError::EvalError(format!(
                    "if-condition is not bool: {other:?}"
                ))),
            }
        }
        Expr::Pre { arg } => {
            if state.cycle == 0 {
                Err(SimError::EvalError(
                    "uninitialized `pre` evaluated on the first cycle (missing `->`)".into(),
                ))
            } else if let Expr::Var { name } = arg.as_ref() {
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
            if state.cycle == 0 {
                eval(init, env, state, call_states, project)
            } else {
                eval(body, env, state, call_states, project)
            }
        }
        Expr::Call { node, args } => eval_call(expr, node, args, env, state, call_states, project),
        Expr::Field { base, field } => {
            let bv = eval(base, env, state, call_states, project)?;
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
            let bv = eval(base, env, state, call_states, project)?;
            let iv = eval(index, env, state, call_states, project)?;
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
                vs.push(eval(it, env, state, call_states, project)?);
            }
            Ok(Value::Tuple(vs))
        }
    }
}

fn eval_call(
    call_expr: &Expr,
    node: &str,
    args: &[Expr],
    env: &BTreeMap<String, Value>,
    state: &mut State,
    call_states: &mut HashMap<usize, State>,
    project: &Project,
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
        arg_values.push(eval(a, env, state, call_states, project)?);
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

    match callee.kind {
        NodeKind::Function => {
            // Stateless: a single pass over the body with a throwaway state.
            let mut throwaway = State::default();
            for eq in &callee.equations {
                let v = eval(&eq.rhs, &callee_env, &mut throwaway, call_states, project)?;
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
            for eq in &callee.equations {
                let v = eval(&eq.rhs, &callee_env, &mut sub_state, call_states, project)?;
                bind_lhs(&mut callee_env, eq, v)?;
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
        (BinOp::Add, Float(a), Float(b)) => Float(a + b),
        (BinOp::Sub, Float(a), Float(b)) => Float(a - b),
        (BinOp::Mul, Float(a), Float(b)) => Float(a * b),
        (BinOp::Div, Float(a), Float(b)) if b != 0.0 => Float(a / b),
        (op, l, r) => {
            return Err(SimError::EvalError(format!(
                "binary {op:?} not supported on {l:?} and {r:?}"
            )))
        }
    })
}
