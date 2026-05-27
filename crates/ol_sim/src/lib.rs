//! OpenLustre Studio: cycle-accurate IR interpreter (Phase 6, plan Task 12).
//!
//! The simulator runs a node in isolation against a CSV input vector and
//! produces a CSV output trace plus contract-monitor results. Each cycle is
//! a single read-eval-write step over the IR — the same semantics the C-Lite
//! emitter targets.

use std::collections::{BTreeMap, HashMap};

use ol_contract_ir::{parse_contracts, ContractDef};
use ol_ir::{BinOp, Expr, Literal, NodeDef, Project, Type, UnaryOp};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Tuple(Vec<Value>),
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

#[derive(Debug, Default)]
pub struct State {
    cycle: usize,
    prev: HashMap<String, Value>,
}

pub struct Sim<'a> {
    project: &'a Project,
    pub node: &'a NodeDef,
    state: State,
    contract: Option<ContractDef>,
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
        Ok(Sim {
            project,
            node,
            state: State::default(),
            contract,
        })
    }

    pub fn step(&mut self, inputs: &BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, SimError> {
        let mut env: BTreeMap<String, Value> = inputs.clone();
        // Provide defaults for outputs/locals so referencing them before
        // assignment yields a deterministic zero rather than panicking.
        for p in &self.node.outputs {
            env.entry(p.name.clone())
                .or_insert_with(|| default_value(&p.ty));
        }
        for l in &self.node.locals {
            env.entry(l.name.clone())
                .or_insert_with(|| default_value(&l.ty));
        }

        // We iterate equations in declaration order. Combinational cycles
        // have been ruled out at typecheck time, so a single pass suffices for
        // the Phase 0 profile.
        for eq in &self.node.equations {
            let value = eval(&eq.rhs, &env, &self.state, self.project)?;
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

        // Snapshot current bindings into `prev` for the next cycle.
        for (k, v) in &env {
            self.state.prev.insert(k.clone(), v.clone());
        }
        self.state.cycle += 1;

        let mut outputs = BTreeMap::new();
        for p in &self.node.outputs {
            outputs.insert(
                p.name.clone(),
                env.get(&p.name).cloned().unwrap_or_else(|| default_value(&p.ty)),
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
        let mut out_headers: Vec<String> = vec!["cycle".into()];
        out_headers.extend(self.node.outputs.iter().map(|p| p.name.clone()));
        if self.contract.is_some() {
            out_headers.push("active_mode".into());
        }
        trace.headers = out_headers;

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
                let modes = evaluate_active_modes(c, &inputs, &out);
                let label = if modes.is_empty() {
                    "<none>".to_string()
                } else {
                    modes.join("|")
                };
                trace.active_modes.push(modes);
                // CSV-safe representation of the active mode list.
                let escaped = label.replace(',', ";");
                out_row.push(Value::Float(0.0));
                let last = out_row.len() - 1;
                out_row[last] = Value::ModeLabel(escaped);
            }
            trace.rows.push(out_row);
        }

        Ok(trace)
    }
}

fn default_value(ty: &Type) -> Value {
    match ty {
        Type::Bool => Value::Bool(false),
        Type::Float32 | Type::Float64 => Value::Float(0.0),
        _ => Value::Int(0),
    }
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

fn evaluate_active_modes(
    c: &ContractDef,
    inputs: &BTreeMap<String, Value>,
    outputs: &BTreeMap<String, Value>,
) -> Vec<String> {
    let mut env: BTreeMap<String, Value> = BTreeMap::new();
    env.extend(inputs.clone());
    env.extend(outputs.clone());
    let state = State::default();
    let project = Project::default();
    let mut active = Vec::new();
    for m in &c.modes {
        let mut hit = true;
        for r in &m.requires {
            match eval(r, &env, &state, &project) {
                Ok(Value::Bool(true)) => {}
                _ => {
                    hit = false;
                    break;
                }
            }
        }
        if hit {
            active.push(m.name.clone());
        }
    }
    active
}

fn eval(
    expr: &Expr,
    env: &BTreeMap<String, Value>,
    state: &State,
    project: &Project,
) -> Result<Value, SimError> {
    match expr {
        Expr::Const { lit } => Ok(match lit {
            Literal::Bool { value } => Value::Bool(*value),
            Literal::Int { value } => Value::Int(*value),
            Literal::Float { value } => Value::Float(*value),
        }),
        Expr::Var { name } => env
            .get(name)
            .cloned()
            .ok_or_else(|| SimError::EvalError(format!("unbound variable `{name}`"))),
        Expr::Unary { op, arg } => {
            let v = eval(arg, env, state, project)?;
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
            let l = eval(lhs, env, state, project)?;
            let r = eval(rhs, env, state, project)?;
            eval_binary(*op, l, r)
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            let c = eval(cond, env, state, project)?;
            match c {
                Value::Bool(true) => eval(then_branch, env, state, project),
                Value::Bool(false) => eval(else_branch, env, state, project),
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
                eval(init, env, state, project)
            } else {
                eval(body, env, state, project)
            }
        }
        Expr::Call { node, args } => {
            // Function calls only — operator calls require nested state, which
            // is out of scope for this MVP simulator. Functions are stateless
            // so we can evaluate them as a fresh expression substitution.
            let callee = project
                .find_node(node)
                .ok_or_else(|| SimError::EvalError(format!("unknown callee `{node}`")))?;
            if !callee.is_function() {
                return Err(SimError::EvalError(format!(
                    "Phase 0 simulator only inlines function calls; `{node}` is stateful"
                )));
            }
            if args.len() != callee.inputs.len() {
                return Err(SimError::EvalError(format!(
                    "call to `{}` arity mismatch: expected {}, got {}",
                    node,
                    callee.inputs.len(),
                    args.len()
                )));
            }
            let mut callee_env: BTreeMap<String, Value> = BTreeMap::new();
            for (p, a) in callee.inputs.iter().zip(args.iter()) {
                callee_env.insert(p.name.clone(), eval(a, env, state, project)?);
            }
            for p in &callee.outputs {
                callee_env.insert(p.name.clone(), default_value(&p.ty));
            }
            for l in &callee.locals {
                callee_env.insert(l.name.clone(), default_value(&l.ty));
            }
            let local_state = State::default();
            for eq in &callee.equations {
                let v = eval(&eq.rhs, &callee_env, &local_state, project)?;
                if eq.lhs.len() == 1 {
                    callee_env.insert(eq.lhs[0].clone(), v);
                }
            }
            if callee.outputs.len() == 1 {
                Ok(callee_env
                    .remove(&callee.outputs[0].name)
                    .unwrap_or(Value::Bool(false)))
            } else {
                Ok(Value::Tuple(
                    callee
                        .outputs
                        .iter()
                        .map(|p| callee_env.remove(&p.name).unwrap_or(Value::Bool(false)))
                        .collect(),
                ))
            }
        }
        Expr::Field { .. } | Expr::Index { .. } | Expr::Tuple { .. } => Err(SimError::EvalError(
            "records, arrays, and tuple literals are not supported in the Phase 0 simulator".into(),
        )),
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
